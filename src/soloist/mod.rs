//! Spotify Soloist, Spotify's own headless player, as a second way to play.
//!
//! Spotify stops handing out decryption keys to librespot for some accounts
//! (librespot issue #1649); its official clients keep playing. Soloist is
//! one of them: Lilac downloads it from Spotify when the user asks, runs it
//! as a child process and controls it as a Spotify Connect device. Builds
//! expire 90 days after they are made, so Lilac keeps track of the
//! installed build, replaces it in time and says so when it cannot.

pub mod build;
pub mod firewall;
pub mod ws;

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use futures_util::StreamExt as _;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui_state::{Dialog, ToastKind};
use crate::{app, notify, paths, process, runtime};
use build::{Build, Reminder};

/// Stands for Soloist's Connect id until Spotify's device list gives the
/// real one (or while it cannot be asked, e.g. rate limited): commands go
/// over the local socket either way.
pub const LOCAL_DEVICE: &str = "soloist-local";

/// How Soloist appears in Spotify's device lists.
pub const DEVICE_NAME: &str = "Lilac (Soloist)";

/// After Spotify refused the built-in player, Automatic plays through
/// Soloist for this long before trying the built-in player again.
const REFUSAL_MEMORY: u64 = 3 * build::DAY;

/// How long a play request waits for Soloist to start and log in.
const READY_TIMEOUT: Duration = Duration::from_secs(25);

/// Kept across runs, readable by the user only (the API key is a secret).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub api_key: Option<String>,
    pub build: Option<Build>,
    /// Spotify logged this device in once; its session is in the data dir.
    pub paired: bool,
    /// Unix seconds of the last look for a newer build.
    pub last_check: u64,
    /// The CDN's ETag for a newer build than the installed one.
    pub newer: Option<String>,
    /// The loudest expiry reminder shown for the installed build.
    pub reminded: Option<(u64, Reminder)>,
    /// When Spotify last refused the built-in player's key requests.
    pub builtin_refused_at: Option<u64>,
}

impl Store {
    fn load() -> Self {
        std::fs::read(store_file())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Ok(raw) = serde_json::to_vec_pretty(self) {
            let _ = paths::write_private(&store_file(), &raw);
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Install {
    Idle,
    /// Bytes received and expected (when the server says).
    Downloading(u64, Option<u64>),
    Unpacking,
    Failed(SharedString),
}

/// Where the guided setup stands; each step needs the ones before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Download,
    Key,
    Pair,
    Done,
}

/// Whether a firewall keeps phones from reaching Soloist while it pairs.
#[derive(Clone, Debug, PartialEq)]
pub enum Gate {
    Unchecked,
    /// Nothing in the way that Lilac knows of.
    Clear,
    Blocked(firewall::Kind),
    /// Waiting for the user at the password prompt.
    Opening(firewall::Kind),
    Open(firewall::Kind),
    Failed(firewall::Kind, SharedString),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Process {
    Stopped,
    Starting,
    Running,
    Failed(SharedString),
}

type Waiter = Box<dyn FnOnce(String, &mut App)>;

pub struct Soloist {
    pub store: Store,
    pub install: Install,
    pub process: Process,
    pub logged_in: bool,
    /// Its Spotify Connect id, once it is logged in and listed.
    pub device_id: Option<String>,
    pid: Option<u32>,
    /// The last lines Soloist wrote, to say why it stopped.
    log: Arc<Mutex<VecDeque<String>>>,
    ws: Option<ws::Connection>,
    pub gate: Gate,
    /// The firewall opening for pairing; dropping it closes the port.
    opening: Option<firewall::Opening>,
    waiters: Vec<Waiter>,
    tasks: Vec<Task<()>>,
    check: Option<Task<()>>,
    /// Bumped on every start, so a stale process's exit is ignored.
    generation: u64,
}

pub enum SoloistEvent {
    /// Logged in and listed: plays can go to `device_id`.
    Ready,
}

impl EventEmitter<SoloistEvent> for Soloist {}

impl Soloist {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let store = Store::load();
        if let Some(key) = &store.api_key {
            crate::diagnostics::private(key);
        }
        let mut this = Self {
            store,
            install: Install::Idle,
            process: Process::Stopped,
            logged_in: false,
            device_id: None,
            pid: None,
            log: Arc::default(),
            ws: None,
            gate: Gate::Unchecked,
            opening: None,
            waiters: Vec::new(),
            tasks: Vec::new(),
            check: None,
            generation: 0,
        };
        // A Soloist left running by a Lilac that did not exit cleanly (or
        // an older Lilac) would keep playing on its own; Lilac owns it.
        // The pid is read now, before this Lilac starts a Soloist of its
        // own that would write its pid there.
        if let Some(stale) = stale_pid(&paths::soloist_dir().join("data")) {
            std::thread::spawn(move || end_pid(stale));
        }
        // A build that vanished from disk is not installed any more.
        if this.store.build.is_some() && !binary().exists() {
            this.store.build = None;
            this.store.save();
        }
        cx.on_app_quit(|this, _| {
            this.stop_process();
            async {}
        })
        .detach();
        this.schedule_check(cx);
        this
    }

    // ------------------------------------------------------------- readiness

    pub fn installed(&self) -> Option<&Build> {
        self.store.build.as_ref()
    }

    pub fn has_key(&self) -> bool {
        self.store.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }

    pub fn expired(&self) -> bool {
        self.installed().is_some_and(|b| b.expired(now()))
    }

    /// Everything needed to play here without asking the user first.
    pub fn usable(&self) -> bool {
        self.installed().is_some() && !self.expired() && self.has_key() && self.store.paired
    }

    /// Spotify refused the built-in player recently.
    pub fn builtin_refused(&self) -> bool {
        self.store
            .builtin_refused_at
            .is_some_and(|at| now().saturating_sub(at) < REFUSAL_MEMORY)
    }

    pub fn step(&self) -> Step {
        if self.installed().is_none() || self.expired() {
            Step::Download
        } else if !self.has_key() {
            Step::Key
        } else if !self.store.paired {
            Step::Pair
        } else {
            Step::Done
        }
    }

    // ----------------------------------------------------------------- setup

    /// Open the guided setup where it left off.
    pub fn open_setup(cx: &mut App) {
        app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::SoloistSetup, cx));
        app::soloist(cx).update(cx, |s, cx| s.advance_setup(cx));
    }

    fn setup_open(cx: &App) -> bool {
        app::ui(cx).read(cx).dialog == Some(Dialog::SoloistSetup)
    }

    /// Get the current setup step going by itself where it can: pairing
    /// needs Soloist running and a look at the firewall.
    pub fn advance_setup(&mut self, cx: &mut Context<Self>) {
        if !Self::setup_open(cx) || self.step() != Step::Pair {
            return;
        }
        // One Lilac in the phone's list while pairing: the right one.
        cx.defer(|cx| app::player(cx).update(cx, |p, cx| p.hide_builtin(true, cx)));
        self.start(cx);
        if self.gate == Gate::Unchecked {
            self.gate = match firewall::detect() {
                Some(kind) => Gate::Blocked(kind),
                None => Gate::Clear,
            };
            cx.notify();
        }
    }

    /// The setup was closed: whatever it opened in the firewall closes too.
    pub fn setup_closed(&mut self, cx: &mut Context<Self>) {
        self.close_firewall();
        cx.defer(|cx| app::player(cx).update(cx, |p, cx| p.hide_builtin(false, cx)));
        if !self.store.paired && self.waiters.is_empty() {
            self.stop(cx);
        }
        cx.notify();
    }

    /// Ask for the password and let phones on private networks reach
    /// Soloist's pairing port.
    pub fn open_firewall(&mut self, cx: &mut Context<Self>) {
        let kind = match self.gate {
            Gate::Blocked(kind) | Gate::Failed(kind, _) => kind,
            _ => return,
        };
        let Some(port) = self.pairing_port() else {
            self.gate = Gate::Failed(
                kind,
                "Soloist is still starting; try again in a moment".into(),
            );
            cx.notify();
            return;
        };
        self.gate = Gate::Opening(kind);
        cx.notify();
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { firewall::open(kind, port) })
                .await;
            this.update(cx, |this, cx| {
                // Soloist restarted meanwhile: its port is a different one.
                if this.generation != generation {
                    this.gate = Gate::Blocked(kind);
                } else {
                    match result {
                        Ok(opening) => {
                            this.opening = Some(opening);
                            this.gate = Gate::Open(kind);
                        }
                        Err(err) => this.gate = Gate::Failed(kind, format!("{err:#}").into()),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close_firewall(&mut self) {
        self.opening = None;
        if let Gate::Open(kind) | Gate::Opening(kind) = self.gate {
            self.gate = Gate::Blocked(kind);
        }
    }

    /// Soloist's pairing port, while it runs.
    pub fn pairing_port(&self) -> Option<u16> {
        firewall::pairing_port(self.pid?)
    }

    pub fn record_builtin_refused(&mut self, cx: &mut Context<Self>) {
        self.store.builtin_refused_at = Some(now());
        self.store.save();
        cx.notify();
    }

    pub fn forget_builtin_refused(&mut self, cx: &mut Context<Self>) {
        self.store.builtin_refused_at = None;
        self.store.save();
        cx.notify();
    }

    pub fn set_api_key(&mut self, key: String, cx: &mut Context<Self>) {
        let key = key.trim().to_string();
        crate::diagnostics::private(&key);
        self.store.api_key = (!key.is_empty()).then_some(key);
        self.store.save();
        // A running Soloist keeps the key it started with.
        if self.process != Process::Stopped {
            self.restart(cx);
        }
        self.advance_setup(cx);
        cx.notify();
    }

    // --------------------------------------------------------------- process

    /// Start Soloist if it is not running. Plays queued with `when_ready`
    /// go out once it is logged in.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if matches!(self.process, Process::Starting | Process::Running) {
            return;
        }
        if let Err(err) = self.spawn(cx) {
            self.fail(format!("{err:#}"), cx);
        }
    }

    fn spawn(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let build = self.installed().context("Soloist is not installed")?;
        if build.expired(now()) {
            self.remind(cx);
            bail!("this Soloist build has expired");
        }
        let key = self
            .store
            .api_key
            .clone()
            .filter(|k| !k.is_empty())
            .context("Soloist needs your API key")?;
        let data = paths::soloist_dir().join("data");
        let cache = paths::cache_dir().join("soloist");
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&cache)?;
        end_stale(&data);
        let _ = std::fs::remove_file(data.join("ws.port"));
        let _ = std::fs::remove_file(data.join("ws.addr"));

        let volume = (app::settings(cx).read(cx).volume * 100.0).round() as u32;
        let mut command = process::background(binary());
        end_with_us(&mut command);
        let mut child = command
            .args(["--device-name", DEVICE_NAME, "--api-key", &key])
            .arg("--data-dir")
            .arg(&data)
            .arg("--cache-dir")
            .arg(&cache)
            // The smallest cache Soloist allows; 0 would mean unbounded.
            .args(["--cache-size", "100"])
            .args(["--ws", "127.0.0.1:0"])
            .args(["--initial-volume", &volume.min(100).to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not start Soloist")?;
        end_with_us_started(&child);

        self.generation += 1;
        let generation = self.generation;
        self.pid = Some(child.id());
        self.process = Process::Starting;
        self.logged_in = false;
        self.device_id = None;
        self.log.lock().unwrap().clear();
        for pipe in [
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = self.log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut log = log.lock().unwrap();
                    if log.len() == 20 {
                        log.pop_front();
                    }
                    log.push_back(line);
                }
            });
        }
        let (exit_tx, exit_rx) = futures_channel_oneshot();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            let _ = exit_tx.send(code);
        });

        self.tasks.clear();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let code = exit_rx.await.ok().flatten();
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.exited(code, cx);
                }
            })
            .ok();
        }));
        self.tasks.push(cx.spawn(async move |this, cx| {
            // Soloist writes the port it was given once the socket is up.
            let mut addr = None;
            for _ in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if let Some(found) = read_ws_addr(&data) {
                    addr = Some(found);
                    break;
                }
            }
            let Some(addr) = addr else {
                this.update(cx, |this, cx| {
                    this.fail("Soloist did not open its control socket".into(), cx)
                })
                .ok();
                return;
            };
            match runtime::run(async move { ws::connect(&addr).await }).await {
                Ok((connection, events)) => {
                    this.update(cx, |this, cx| {
                        this.ws = Some(connection);
                        this.process = Process::Running;
                        cx.notify();
                    })
                    .ok();
                    pump_events(this, events, cx).await;
                }
                Err(err) => {
                    this.update(cx, |this, cx| this.fail(format!("{err:#}"), cx))
                        .ok();
                }
            }
        }));
        cx.notify();
        Ok(())
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.stop_process();
        self.process = Process::Stopped;
        cx.notify();
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.stop_process();
        self.process = Process::Stopped;
        self.start(cx);
    }

    fn stop_process(&mut self) {
        // A restarted Soloist listens on another port.
        self.close_firewall();
        self.generation += 1;
        self.tasks.clear();
        self.ws = None;
        self.logged_in = false;
        self.device_id = None;
        if let Some(pid) = self.pid.take() {
            // In the background: a hung shutdown is killed after a while.
            std::thread::spawn(move || end_pid(pid));
        }
    }

    fn exited(&mut self, code: Option<i32>, cx: &mut Context<Self>) {
        self.pid = None;
        self.ws = None;
        self.tasks.clear();
        self.logged_in = false;
        self.device_id = None;
        match code {
            Some(0) => {
                self.process = Process::Stopped;
                cx.notify();
            }
            Some(10) => {
                // Our clock and Soloist's may disagree by a little; trust it.
                if let Some(build) = &mut self.store.build {
                    build.built_at = build.built_at.min(now() - build::LIFETIME_SECS);
                    self.store.save();
                }
                self.fail("this Soloist build has expired".into(), cx);
                self.remind(cx);
                self.check_now(cx);
            }
            _ => {
                let why = self
                    .log
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| "Soloist stopped".into());
                self.fail(why, cx);
            }
        }
    }

    fn fail(&mut self, why: String, cx: &mut Context<Self>) {
        self.stop_process();
        self.process = Process::Failed(why.clone().into());
        if !self.waiters.is_empty() {
            self.waiters.clear();
            app::toast_error(format!("Soloist could not play: {why}"), cx);
        }
        cx.notify();
    }

    fn on_event(&mut self, event: ws::Event, cx: &mut Context<Self>) {
        match event {
            ws::Event::Auth { logged_in, .. } => {
                let was = self.logged_in;
                self.logged_in = logged_in;
                if logged_in && !self.store.paired {
                    self.store.paired = true;
                    self.store.save();
                    self.close_firewall();
                    cx.defer(|cx| {
                        // Refused before: Soloist becomes the engine and the
                        // built-in player does not come back.
                        crate::player::Player::prefer_soloist(cx);
                        app::player(cx).update(cx, |p, cx| {
                            p.hide_builtin(false, cx);
                            p.apply_engine(cx);
                        })
                    });
                    if !Self::setup_open(cx) {
                        app::toast("Soloist is paired with your account", cx);
                    }
                }
                if logged_in && !was {
                    // Ready for local control at once; the real Connect id
                    // follows when Spotify lists it.
                    if self.device_id.is_none() {
                        self.device_id = Some(LOCAL_DEVICE.to_string());
                        for waiter in std::mem::take(&mut self.waiters) {
                            waiter(LOCAL_DEVICE.to_string(), cx);
                        }
                        cx.emit(SoloistEvent::Ready);
                    }
                    self.find_device(cx);
                }
                if !logged_in {
                    self.device_id = None;
                }
                cx.notify();
            }
            ws::Event::Error(message) => app::toast_error(format!("Soloist: {message}"), cx),
            ws::Event::Status(_) => {}
            // What plays is the player's to show; straight from Soloist,
            // it is exact and immediate (Spotify never echoes the changes
            // Lilac makes itself).
            ws::Event::State(state) => {
                if self.device_id.is_some() {
                    cx.defer(move |cx| {
                        app::player(cx).update(cx, |p, cx| p.on_soloist_state(*state, cx))
                    });
                }
            }
            ws::Event::Queue(upcoming) => {
                if self.device_id.is_some() {
                    cx.defer(move |cx| {
                        app::player(cx).update(cx, |p, cx| p.on_soloist_queue(upcoming, cx))
                    });
                }
            }
            ws::Event::Position(position) => {
                if self.device_id.is_some() {
                    cx.defer(move |cx| {
                        app::player(cx).update(cx, |p, cx| p.on_soloist_position(position, cx))
                    });
                }
            }
        }
    }

    /// Learn Soloist's Connect id from the account's device list; it shows
    /// up there a moment after logging in.
    fn find_device(&mut self, cx: &mut Context<Self>) {
        let api = app::api(cx);
        self.tasks.push(cx.spawn(async move |this, cx| {
            // Spotify lists a new device a few seconds after it logs in;
            // asking sooner only spends Web API requests.
            for delay in [4u64, 6] {
                cx.background_executor()
                    .timer(Duration::from_secs(delay))
                    .await;
                // Only Spotify's Web API knows the id, and only starting a
                // list part-way needs it; while that API is paused, stop.
                if crate::spotify::api::penalty_left().is_some() {
                    return;
                }
                let api = api.clone();
                let Ok(list) = runtime::run(async move { api.remote_devices().await }).await else {
                    continue;
                };
                let id = list
                    .devices
                    .iter()
                    .find(|d| d.name == DEVICE_NAME)
                    .and_then(|d| d.id.clone());
                if let Some(id) = id {
                    this.update(cx, |this, cx| {
                        let real = id.clone();
                        cx.defer(move |cx| {
                            app::player(cx).update(cx, |p, cx| p.soloist_id_known(real, cx))
                        });
                        this.device_id = Some(id.clone());
                        for waiter in std::mem::take(&mut this.waiters) {
                            waiter(id.clone(), cx);
                        }
                        cx.emit(SoloistEvent::Ready);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            }
            // Not listed (or Spotify cannot be asked): local control
            // keeps working; nothing to fail.
            let _ = this;
        }));
    }

    /// Run `then` with Soloist's Connect id once it is up, starting it if
    /// needed. Gives up (with a toast) if it is not ready in time.
    pub fn when_ready(
        &mut self,
        then: impl FnOnce(String, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.device_id.clone() {
            cx.defer(move |cx| then(id, cx));
            return;
        }
        self.waiters.push(Box::new(then));
        self.start(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READY_TIMEOUT).await;
            this.update(cx, |this, cx| {
                if this.device_id.is_none() && !this.waiters.is_empty() {
                    this.waiters.clear();
                    let hint = if this.store.paired {
                        "Soloist did not come up in time"
                    } else {
                        "Soloist is not set up yet"
                    };
                    app::toast_error(hint, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Send a command over the local socket, which is quicker than going
    /// through Spotify. False when there is no connection.
    pub fn command(&self, name: &str, fields: Value) -> bool {
        match &self.ws {
            Some(ws) if self.logged_in => ws.send.send(ws::command(name, fields)).is_ok(),
            _ => false,
        }
    }

    /// Started (or about to be) and not logged in yet.
    pub fn coming_up(&self) -> bool {
        !self.logged_in && !matches!(self.process, Process::Failed(_))
    }

    pub fn is_device(&self, id: &str) -> bool {
        id == LOCAL_DEVICE || self.device_id.as_deref() == Some(id)
    }

    // --------------------------------------------------------------- install

    /// Download the current build from Spotify and install it, replacing
    /// the installed one.
    pub fn install(&mut self, cx: &mut Context<Self>) {
        if matches!(self.install, Install::Downloading(..) | Install::Unpacking) {
            return;
        }
        let Some(url) = build::archive_url() else {
            self.install = Install::Failed("Spotify has no Soloist build for this system".into());
            cx.notify();
            return;
        };
        self.install = Install::Downloading(0, None);
        cx.notify();
        let (progress_tx, mut progress_rx) = tokio::sync::watch::channel((0u64, None));
        let download = runtime::spawn(download(url, progress_tx));
        cx.spawn(async move |this, cx| {
            // Progress, at most a few times a second.
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let changed = progress_rx.has_changed();
                let (got, total) = *progress_rx.borrow_and_update();
                let finished = changed.is_err();
                this.update(cx, |this, cx| {
                    if matches!(this.install, Install::Downloading(..)) {
                        this.install = if finished {
                            Install::Unpacking
                        } else {
                            Install::Downloading(got, total)
                        };
                        cx.notify();
                    }
                })
                .ok();
                if finished {
                    break;
                }
            }
            let result = match download.await {
                Ok(result) => result,
                Err(err) => Err(anyhow!("download failed: {err}")),
            };
            this.update(cx, |this, cx| this.installed_build(result, cx))
                .ok();
        })
        .detach();
    }

    fn installed_build(&mut self, result: Result<Downloaded>, cx: &mut Context<Self>) {
        let downloaded = match result {
            Ok(downloaded) => downloaded,
            Err(err) => {
                let why: SharedString = format!("{err:#}").into();
                self.install = Install::Failed(why.clone());
                cx.notify();
                if self
                    .store
                    .build
                    .as_ref()
                    .is_some_and(|b| b.days_left(now()) < 3)
                {
                    app::toast_error(format!("Could not update Soloist: {why}"), cx);
                }
                return;
            }
        };
        let was_running = self.process != Process::Stopped;
        self.stop_process();
        self.process = Process::Stopped;
        if let Err(err) = put_in_place(&downloaded.dir) {
            self.install = Install::Failed(format!("{err:#}").into());
            cx.notify();
            return;
        }
        let first = self.store.build.is_none();
        let version = downloaded.version.clone();
        self.store.build = Some(Build {
            version: downloaded.version,
            built_at: downloaded.built_at,
            etag: downloaded.etag,
            last_modified: downloaded.last_modified,
            installed_at: now(),
        });
        self.store.newer = None;
        self.store.last_check = now();
        self.store.reminded = None;
        self.store.save();
        self.install = Install::Idle;
        if first {
            app::toast(format!("Soloist {version} installed"), cx);
        } else {
            app::toast(format!("Soloist updated to {version}"), cx);
            notify::send(
                "Soloist updated",
                &format!("Lilac installed Soloist {version}."),
            );
        }
        if was_running || !self.waiters.is_empty() {
            self.start(cx);
        }
        self.schedule_check(cx);
        self.advance_setup(cx);
        cx.notify();
    }

    pub fn remove(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        let _ = std::fs::remove_file(binary());
        let _ = std::fs::remove_file(paths::soloist_dir().join("CHANGELOG.md"));
        let _ = std::fs::remove_file(paths::soloist_dir().join("THIRD_PARTY_LICENSES.txt"));
        let _ = std::fs::remove_dir_all(paths::cache_dir().join("soloist"));
        self.store.build = None;
        self.store.newer = None;
        self.store.reminded = None;
        self.store.save();
        self.install = Install::Idle;
        cx.notify();
    }

    /// Forget the pairing: the stored Spotify session goes, the key stays.
    pub fn unpair(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        let _ = std::fs::remove_dir_all(paths::soloist_dir().join("data"));
        self.store.paired = false;
        self.store.save();
        cx.notify();
    }

    // --------------------------------------------------------------- updates

    pub fn next_check(&self) -> u64 {
        build::next_check(self.installed(), self.store.last_check, now())
    }

    /// Sleep until the next update check is due. One timer, not a poll.
    fn schedule_check(&mut self, cx: &mut Context<Self>) {
        if self.installed().is_none() {
            self.check = None;
            return;
        }
        self.remind(cx);
        let wait = self.next_check().saturating_sub(now());
        self.check = Some(cx.spawn(async move |this, cx| {
            // Capped, so a laptop waking from sleep does not wait a full day.
            cx.background_executor()
                .timer(Duration::from_secs(wait.clamp(30, 6 * 60 * 60)))
                .await;
            this.update(cx, |this, cx| {
                if now() >= this.next_check() {
                    this.check_now(cx);
                } else {
                    this.schedule_check(cx);
                }
            })
            .ok();
        }));
    }

    /// Ask Spotify's CDN whether a newer build is out, and install it when
    /// the installed one is due for replacing.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        self.check_for_update(false, cx);
    }

    /// Look for a newer build; `asked` installs any newer one at once and
    /// says when there is none.
    pub fn check_for_update(&mut self, asked: bool, cx: &mut Context<Self>) {
        let Some(url) = build::archive_url() else {
            return;
        };
        let Some(current) = self.installed().cloned() else {
            return;
        };
        self.check = Some(cx.spawn(async move |this, cx| {
            let head = runtime::run(async move { head(&url).await }).await;
            this.update(cx, |this, cx| {
                this.store.last_check = now();
                if let Ok(etag) = &head {
                    let newer = etag
                        .clone()
                        .filter(|e| current.etag.as_deref() != Some(e.as_str()));
                    this.store.newer = newer;
                }
                this.store.save();
                let due = build::should_update(&current, now()) || current.expired(now());
                let auto = app::settings(cx).read(cx).soloist_auto_update;
                if this.store.newer.is_some() && (asked || due && auto || current.expired(now())) {
                    this.install(cx);
                } else if asked {
                    match head {
                        Ok(_) => {
                            app::toast(format!("Soloist {} is the latest", current.version), cx)
                        }
                        Err(err) => {
                            app::toast_error(format!("Could not reach Spotify: {err:#}"), cx)
                        }
                    }
                }
                this.schedule_check(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Tell the user about the build's expiry, once per level per build.
    fn remind(&mut self, cx: &mut Context<Self>) {
        let Some(build) = self.installed().cloned() else {
            return;
        };
        let level = build::reminder(&build, now());
        let shown = match self.store.reminded {
            Some((built_at, level)) if built_at == build.built_at => level,
            _ => Reminder::None,
        };
        if level <= shown {
            return;
        }
        self.store.reminded = Some((build.built_at, level));
        self.store.save();
        let days = build.days_left(now());
        let auto = app::settings(cx).read(cx).soloist_auto_update;
        match level {
            Reminder::None => {}
            Reminder::Soon => {
                let then = if auto {
                    "Lilac will download a newer one before then."
                } else {
                    "Update it in Settings."
                };
                app::toast(format!("Soloist expires in {days} days. {then}"), cx);
            }
            Reminder::Urgent => {
                let text = format!(
                    "Soloist expires {}. Update it to keep playing.",
                    if days == 0 {
                        "today".to_string()
                    } else {
                        format!("in {days} days")
                    }
                );
                app::ui(cx).update(cx, |ui, cx| {
                    ui.toast_with_action(
                        text.clone(),
                        ToastKind::Error,
                        "Update",
                        |_, cx| app::soloist(cx).update(cx, |s, cx| s.install(cx)),
                        cx,
                    )
                });
                notify::send("Soloist expires soon", &text);
            }
            Reminder::Expired => {
                app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::SoloistExpired, cx));
                notify::send(
                    "Soloist has expired",
                    "Spotify's Soloist builds run for 90 days. Download a newer one in Lilac.",
                );
            }
        }
    }
}

// ---------------------------------------------------------------- helpers

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn binary() -> PathBuf {
    paths::soloist_dir().join(if cfg!(windows) {
        "soloist.exe"
    } else {
        "soloist"
    })
}

fn store_file() -> PathBuf {
    paths::config_dir().join("soloist.json")
}

fn futures_channel_oneshot<T>() -> (
    tokio::sync::oneshot::Sender<T>,
    tokio::sync::oneshot::Receiver<T>,
) {
    tokio::sync::oneshot::channel()
}

async fn pump_events(
    this: WeakEntity<Soloist>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<ws::Event>,
    cx: &mut AsyncApp,
) {
    while let Some(event) = events.recv().await {
        if this
            .update(cx, |this, cx| this.on_event(event, cx))
            .is_err()
        {
            return;
        }
    }
}

fn read_ws_addr(data: &Path) -> Option<String> {
    let port = std::fs::read_to_string(data.join("ws.port")).ok()?;
    let port: u16 = port.trim().parse().ok()?;
    let host = std::fs::read_to_string(data.join("ws.addr"))
        .ok()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "127.0.0.1".into());
    Some(format!("{host}:{port}"))
}

/// A Soloist left over from a Lilac that did not shut down cleanly holds
/// the data directory; end it.
fn end_stale(data: &Path) {
    if let Some(pid) = stale_pid(data) {
        end_pid(pid);
    }
}

fn stale_pid(data: &Path) -> Option<u32> {
    std::fs::read_to_string(data.join("soloist.pid"))
        .ok()
        .and_then(|p| p.trim().parse::<u32>().ok())
}

/// Ask a Soloist to stop, and make it stop: Soloist can hang while
/// shutting down, still holding its data folder, and only one Soloist may
/// use the folder, so a hung one would keep every later one from starting.
fn end_pid(pid: u32) {
    if !is_soloist(pid) {
        return;
    }
    terminate(pid);
    if !exited_within(pid, Duration::from_secs(3)) {
        force_kill(pid);
        exited_within(pid, Duration::from_secs(1));
    }
}

fn exited_within(pid: u32, limit: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < limit {
        if !is_soloist(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !is_soloist(pid)
}

fn force_kill(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(windows)]
    let _ = pid;
}

/// Whether `pid` is a Soloist process (and not another one that got its
/// number since).
fn is_soloist(pid: u32) -> bool {
    let name = binary()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if cfg!(target_os = "linux") {
        return std::fs::read_link(format!("/proc/{pid}/exe"))
            .is_ok_and(|exe| exe.file_name().is_some_and(|n| n.to_string_lossy() == name));
    }
    let output = if cfg!(windows) {
        process::background("tasklist")
            .args(["/NH", "/FI", &format!("PID eq {pid}")])
            .output()
    } else {
        Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
    };
    output.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&name))
}

fn terminate(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // SIGTERM lets Soloist remove its socket files and pid file.
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    #[cfg(windows)]
    let _ = process::background("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

/// Have the system end Soloist when Lilac goes, however it goes (a crash
/// or a kill leaves no chance to stop it, and even a clean quit exits
/// before `stop_process`'s thread gets to run). Linux sets it up before the
/// start, Windows right after (`end_with_us_started`); elsewhere the next
/// start ends a leftover one (`end_stale`).
fn end_with_us(command: &mut Command) {
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt as _;
        command.pre_exec(|| {
            // SIGKILL: a Soloist that hangs on SIGTERM must not outlive us.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// On Windows: Soloist goes into a job that ends everything in it once the
/// last handle to the job closes, and the only handle is Lilac's, which the
/// system closes when Lilac's process ends.
fn end_with_us_started(child: &std::process::Child) {
    #[cfg(windows)]
    job::adopt(child);
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle as _;
    use std::sync::OnceLock;

    type Handle = *mut c_void;

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    }

    /// One job for every Soloist Lilac starts, kept open until Lilac ends.
    fn job() -> Option<Handle> {
        static JOB: OnceLock<Option<usize>> = OnceLock::new();
        let job = JOB.get_or_init(|| unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                log::warn!(
                    "soloist: no job object: {}",
                    std::io::Error::last_os_error()
                );
                return None;
            }
            let mut limits = ExtendedLimits::default();
            limits.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&raw const limits).cast(),
                size_of::<ExtendedLimits>() as u32,
            );
            if set == 0 {
                log::warn!(
                    "soloist: job object limits: {}",
                    std::io::Error::last_os_error()
                );
                return None;
            }
            Some(job as usize)
        });
        job.map(|job| job as Handle)
    }

    #[cfg(all(test, target_pointer_width = "64"))]
    #[test]
    fn limits_match_the_system_layout() {
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION on 64-bit Windows.
        assert_eq!(size_of::<ExtendedLimits>(), 144);
    }

    pub fn adopt(child: &std::process::Child) {
        let Some(job) = job() else { return };
        if unsafe { AssignProcessToJobObject(job, child.as_raw_handle()) } == 0 {
            log::warn!(
                "soloist: not tied to Lilac: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

struct Downloaded {
    /// Where the unpacked build waits to be put in place.
    dir: PathBuf,
    version: String,
    built_at: u64,
    etag: Option<String>,
    last_modified: Option<String>,
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default()
}

async fn head(url: &str) -> Result<Option<String>> {
    let response = http().head(url).send().await?.error_for_status()?;
    Ok(header(&response, reqwest::header::ETAG))
}

fn header(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Fetch the archive from Spotify's CDN and unpack it next to the
/// installed build, without touching that yet.
async fn download(
    url: String,
    progress: tokio::sync::watch::Sender<(u64, Option<u64>)>,
) -> Result<Downloaded> {
    let response = http()
        .get(&url)
        .send()
        .await
        .context("could not reach Spotify's download server")?
        .error_for_status()?;
    let etag = header(&response, reqwest::header::ETAG);
    let last_modified = header(&response, reqwest::header::LAST_MODIFIED);
    let total = response.content_length();

    let root = paths::soloist_dir();
    std::fs::create_dir_all(&root)?;
    // Windows builds come as a zip; Windows' own tar unpacks both.
    let archive = root.join(if url.ends_with(".zip") {
        "download.zip"
    } else {
        "download.tar.gz"
    });
    let mut file = tokio::fs::File::create(&archive).await?;
    let mut got = 0u64;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.context("the download broke off")?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        got += chunk.len() as u64;
        let _ = progress.send((got, total));
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;
    drop(file);
    drop(progress);

    let staging = root.join("staging");
    tokio::task::spawn_blocking(move || -> Result<Downloaded> {
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        let status = process::background("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .status()
            .context("could not run tar to unpack Soloist")?;
        let _ = std::fs::remove_file(&archive);
        if !status.success() {
            bail!("the downloaded archive could not be unpacked");
        }
        let exe = staging.join(binary().file_name().unwrap_or_default());
        let output = process::background(&exe)
            .arg("--version")
            .output()
            .context("the downloaded Soloist does not run on this system")?;
        let line = String::from_utf8_lossy(&output.stdout);
        let (version, built_at) = build::parse_version(&line)
            .with_context(|| format!("unexpected Soloist version line: {}", line.trim()))?;
        Ok(Downloaded {
            dir: staging,
            version,
            built_at,
            etag,
            last_modified,
        })
    })
    .await
    .map_err(|err| anyhow!("unpacking failed: {err}"))?
}

/// Move an unpacked build over the installed one.
fn put_in_place(staging: &Path) -> Result<()> {
    let root = paths::soloist_dir();
    for name in [
        binary().file_name().unwrap_or_default().to_os_string(),
        "CHANGELOG.md".into(),
        "THIRD_PARTY_LICENSES.txt".into(),
    ] {
        let from = staging.join(&name);
        if from.exists() {
            std::fs::rename(&from, root.join(&name))
                .with_context(|| format!("could not install {}", name.to_string_lossy()))?;
        }
    }
    let _ = std::fs::remove_dir_all(staging);
    Ok(())
}

/// "Feb 16, 2027" for Unix seconds (UTC; a day either way does not matter
/// for a 90-day lifetime).
pub fn date(secs: u64) -> String {
    let days = (secs / build::DAY) as i64;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {day}, {year}", MONTHS[(month - 1) as usize])
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would shadow the standard one.
    use super::{Store, build, date};

    #[test]
    fn formats_dates() {
        assert_eq!(date(0), "Jan 1, 1970");
        assert_eq!(date(1792800000), "Oct 24, 2026");
        assert_eq!(date(1792800000 + build::LIFETIME_SECS), "Jan 22, 2027");
    }

    #[test]
    fn store_tolerates_missing_fields() {
        let store: Store = serde_json::from_str(r#"{"paired":true}"#).unwrap();
        assert!(store.paired);
        assert!(store.api_key.is_none());
    }
}
