//! Playback state: what is playing, where in the track we are, and what comes next.
//!
//! With an account, playback runs through Spotify Connect: lilac is one
//! device among the user's others, Spotify decides what plays next, and this
//! entity mirrors that state for the UI. When another device is active, the
//! same controls drive that device instead. In demo mode there is nothing to
//! stream, so a local queue and a clock stand in.

mod backend;
pub mod queue;
mod resume;
mod sink;

use std::time::{Duration, Instant};

use gpui_kit::*;
use librespot_metadata::audio::{AudioItem, UniqueFields};

use crate::app;
use crate::model::{AlbumRef, ArtistRef, ContextUri, Images, ItemKind, ShowRef, Track};
use crate::runtime;
use crate::session::{Session, SessionEvent};
use crate::settings::{Engine, Settings};
use serde_json::{Value, json};

use backend::{Backend, Channels, Cluster, EngineEvent, Options, Start};
pub use backend::{Device, DeviceKind};
pub use queue::{Queue, Repeat};
use resume::LastSession;
use sink::{Cue, OutputControl};

/// Restart the current track instead of going to the previous one past this point.
/// Stands in for the device playing until Spotify names it (see
/// `Player::name_other_device`).
pub const OTHER_DEVICE: &str = "lilac-other-device";
const RESTART_THRESHOLD_MS: u32 = 3000;
/// Give up waiting for the restored session after this; the loading screen
/// must never hold the app hostage to a slow network.
const RESTORE_TIMEOUT: Duration = Duration::from_millis(4500);
/// Spirc handles one command at a time and uploads its state after each, so
/// a burst of seeks or volume steps would queue up network round trips.
/// Bursts are collapsed: the first change goes out at once, the last one
/// after this much quiet.
const COMMAND_SETTLE: Duration = Duration::from_millis(400);
/// How often a track that failed to load is asked for again, and the wait
/// first. Spotify limits how many decryption keys an account may fetch in a
/// burst; more retries would only add to the burst.
const UNAVAILABLE_RETRIES: u32 = 1;
const RETRY_DELAY: Duration = Duration::from_millis(1500);
/// librespot skips to the next track when one fails to load. When Spotify
/// refuses every key, that walks through the whole context, each step
/// asking for another key and prolonging the refusal; this many failures in
/// a row stop it.
const MAX_FAILED_IN_ROW: u32 = 3;
/// Volume reports from Spotify this soon after a local change are echoes of
/// older steps and would make the slider jump back.
const VOLUME_ECHO: Duration = Duration::from_millis(1500);

/// How far the player has come since login; the loading screen shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Warmup {
    /// Not logged in yet.
    Idle,
    /// Logging in to Spotify's streaming servers.
    Connecting,
    /// Bringing the last session over, paused where it was left.
    Restoring,
    Ready,
}

pub enum PlayerEvent {
    /// A different track became current.
    TrackChanged,
    /// Playback started or paused.
    StateChanged,
    /// The position jumped.
    Seeked,
}

/// A load requested before the streaming session was ready.
struct PendingLoad {
    context: Option<String>,
    tracks: Vec<String>,
    start: Start,
}

pub struct Player {
    /// The local queue in demo mode; with Connect, a mirror of what Spotify
    /// says comes next.
    pub queue: Queue,
    pub current: Option<Track>,
    pub playing: bool,
    /// The track is being fetched and nothing is audible yet.
    pub loading: bool,
    pub repeat: Repeat,
    pub volume: f32,
    pub muted: bool,
    /// Devices in the user's Connect list, and the one playing.
    pub devices: Vec<Device>,
    pub active_device: Option<String>,
    /// Position at `anchor_at`. While playing, the live position is
    /// extrapolated from it: by the audio heard since (`anchor_heard_ms`)
    /// when it plays here, by the clock for other devices and demo mode.
    anchor_ms: u32,
    anchor_at: Instant,
    anchor_heard_ms: u64,
    /// A track change librespot reported before the old track's queued end
    /// was heard; applied when it is.
    track_switch: Option<Task<()>>,
    /// The track librespot is loading to play (not a preload).
    loading_uri: Option<String>,
    /// A track that failed to load and how often it was asked for again.
    retried: Option<(String, u32)>,
    retry: Option<Task<()>>,
    /// Tracks that failed to load since one last loaded.
    failed_in_row: u32,
    /// Playback was stopped after `MAX_FAILED_IN_ROW` failures; librespot
    /// ignores Play until a track is loaded again.
    halted: bool,
    ticker: Option<Task<()>>,
    backend: Option<Backend>,
    /// The built-in player stays off the network for now (while Soloist
    /// pairs, so a phone lists one Lilac to pick, not two).
    builtin_hidden: bool,
    /// The device Lilac last moved playback to, and when: Soloist reports
    /// that it lost the session, not to whom.
    moving_to: Option<(String, Instant)>,
    /// When Lilac last started something on Soloist; a pause Soloist
    /// reports right after that was not asked for.
    soloist_started_at: Option<Instant>,
    /// The read-only session used instead of the built-in player while
    /// Soloist plays (see `soloist_only`).
    reader: Option<librespot_core::session::Session>,
    reader_starting: Option<Task<()>>,
    /// The streaming session is being set up.
    starting: Option<Task<()>>,
    pending: Option<PendingLoad>,
    listeners: Vec<Task<()>>,
    /// Context of Connect playback; `queue.context` in demo mode.
    connect_context: Option<String>,
    connect_shuffle: bool,
    /// The user's Liked Songs as a Connect context.
    liked_uri: Option<String>,
    /// Upcoming URIs the mirrored queue was built from.
    mirrored_next: Vec<String>,
    lookup: Option<Task<()>>,
    /// Announces the volume once a gesture settles.
    volume_sync: Option<Task<()>>,
    volume_changed_at: Option<Instant>,
    /// A seek waiting for the previous one to settle, and the timer for it.
    pending_seek: Option<u32>,
    seek_settle: Option<Task<()>>,
    pub warmup: Warmup,
    restore: Option<Task<()>>,
    /// Play was pressed while the last session was still on its way here.
    play_when_ready: bool,
    /// Where to go once Soloist plays this track (it starts at 0).
    seek_when_playing: Option<(String, u32)>,
    restore_lookup: Option<tokio::task::JoinHandle<RestoreLookup>>,
    queue_refresh: Option<Task<()>>,
    /// Track URIs of the list we started when it has no context URI, in
    /// order: kept with the session and used to load a track again.
    started_tracks: Vec<String>,
    /// librespot itself is the active device in this session. Spotify's
    /// device list may still name us active from an earlier run, and
    /// librespot ignores loads until it has been activated itself.
    spirc_active: bool,
}

impl EventEmitter<PlayerEvent> for Player {}

impl Player {
    pub fn new(session: &Entity<Session>, settings: &Settings, cx: &mut Context<Self>) -> Self {
        cx.subscribe(session, |this, session, event, cx| match event {
            SessionEvent::Ready if !session.read(cx).is_demo() => {
                // The loading screen covers this, so there is no reason to
                // wait for a first play: connect, join the user's devices and
                // bring the last session over right away. (With a stored
                // login this already started at launch.)
                this.liked_uri = session
                    .read(cx)
                    .user
                    .as_ref()
                    .map(|u| format!("spotify:user:{}:collection", u.id));
                if this.warmup == Warmup::Idle {
                    this.warmup = Warmup::Connecting;
                }
                this.ensure_backend(cx);
                cx.notify();
            }
            SessionEvent::Ready => {
                this.warmup = Warmup::Ready;
                cx.notify();
            }
            SessionEvent::LoggedOut => {
                this.stop(cx);
                // Soloist plays under its own login; it does not hear this.
                app::soloist(cx).update(cx, |s, cx| s.stop(cx));
                this.drop_backend();
                // The reading session is the old account's too; the next
                // login starts its own and restores from there.
                this.reader = None;
                this.reader_starting = None;
                this.restore = None;
                this.restore_lookup = None;
                this.play_when_ready = false;
                crate::spotify::streaming::set_session(None);
                this.devices.clear();
                this.active_device = None;
                this.warmup = Warmup::Idle;
                LastSession::forget();
            }
        })
        .detach();

        // Remember the position on the way out; it is the one that matters.
        cx.on_app_quit(|this, cx| {
            this.save_session(cx);
            async {}
        })
        .detach();

        // Follow the volume the desktop's mixer sets for our stream.
        #[cfg(target_os = "linux")]
        {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            crate::system_volume::start(move |volume| {
                let _ = tx.send(volume);
            });
            crate::system_volume::set(settings.volume.clamp(0.0, 1.0));
            cx.spawn(async move |this, cx| {
                while let Some(volume) = rx.recv().await {
                    if this
                        .update(cx, |this, cx| this.adopt_volume(volume, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }

        Self {
            queue: Queue::default(),
            current: None,
            playing: false,
            loading: false,
            repeat: Repeat::Off,
            volume: settings.volume.clamp(0.0, 1.0),
            muted: false,
            devices: Vec::new(),
            active_device: None,
            anchor_ms: 0,
            anchor_at: Instant::now(),
            anchor_heard_ms: 0,
            track_switch: None,
            loading_uri: None,
            retried: None,
            retry: None,
            failed_in_row: 0,
            halted: false,
            ticker: None,
            backend: None,
            builtin_hidden: false,
            moving_to: None,
            soloist_started_at: None,
            reader: None,
            reader_starting: None,
            starting: None,
            pending: None,
            listeners: Vec::new(),
            connect_context: None,
            connect_shuffle: false,
            liked_uri: None,
            mirrored_next: Vec::new(),
            lookup: None,
            volume_sync: None,
            volume_changed_at: None,
            pending_seek: None,
            seek_settle: None,
            warmup: Warmup::Idle,
            restore: None,
            play_when_ready: false,
            seek_when_playing: None,
            restore_lookup: None,
            queue_refresh: None,
            started_tracks: Vec::new(),
            spirc_active: false,
        }
    }

    // ------------------------------------------------------------------ queries

    pub fn duration_ms(&self) -> u32 {
        self.current.as_ref().map(|t| t.duration_ms).unwrap_or(0)
    }

    pub fn position_ms(&self) -> u32 {
        let position = if let Some(out) = self.heard() {
            // Only audio that came out of the speaker moves the position.
            let heard = out.played_ms().saturating_sub(self.anchor_heard_ms);
            self.anchor_ms.saturating_add(heard as u32)
        } else if self.playing && !self.loading {
            self.anchor_ms
                .saturating_add(self.anchor_at.elapsed().as_millis() as u32)
        } else {
            self.anchor_ms
        };
        position.min(self.duration_ms())
    }

    pub fn progress(&self) -> f32 {
        match self.duration_ms() {
            0 => 0.0,
            duration => self.position_ms() as f32 / duration as f32,
        }
    }

    pub fn is_current(&self, track_id: &str) -> bool {
        self.current
            .as_ref()
            .is_some_and(|t| t.id.as_ref() == track_id)
    }

    /// True when `context` is what is playing.
    pub fn is_context(&self, context: &ContextUri) -> bool {
        if self.connected() {
            return self.connect_context.is_some()
                && self.connect_context.as_deref() == self.context_uri(context).as_deref();
        }
        self.queue.context.as_ref() == Some(context)
    }

    pub fn shuffle(&self) -> bool {
        if self.connected() {
            self.connect_shuffle
        } else {
            self.queue.shuffled()
        }
    }

    pub fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// The device playing, when it is not this one.
    pub fn remote_device(&self) -> Option<&Device> {
        let ours = self.backend.as_ref().map(|b| b.device_id.as_str());
        let active = self.active_device.as_deref()?;
        if Some(active) == ours {
            return None;
        }
        self.devices.iter().find(|d| d.id == active)
    }

    /// The device playing elsewhere when its volume cannot be set from
    /// here (Spotify refuses it for many phones).
    pub fn volume_elsewhere(&self) -> Option<&Device> {
        self.remote_device().filter(|d| !d.volume)
    }

    /// This device's Connect id, once the session is up.
    pub fn device_id(&self) -> Option<&str> {
        self.backend.as_ref().map(|b| b.device_id.as_str())
    }

    /// Queued tracks can be removed or reordered. Spotify Connect offers no
    /// way to do that, so only the local queue can.
    pub fn queue_editable(&self) -> bool {
        !self.connected()
    }

    fn connected(&self) -> bool {
        self.backend.is_some() || self.starting.is_some()
    }

    /// Spotify refuses the built-in player and Soloist is not set up yet:
    /// playing here can only fail, so it is not tried. Play buttons open
    /// the setup instead. Another device playing is unaffected.
    pub fn locked(&self, cx: &App) -> bool {
        if self.simulated(cx) || self.remote_device().is_some() {
            return false;
        }
        let soloist = app::soloist(cx).read(cx);
        soloist.builtin_refused() && !soloist.usable()
    }

    /// Soloist is the engine: the built-in player (and its Connect device)
    /// is not started at all, so Spotify lists one Lilac, the one playing.
    fn soloist_only(&self, cx: &App) -> bool {
        !self.simulated(cx)
            && app::settings(cx).read(cx).engine == Engine::Soloist
            && app::soloist(cx).read(cx).usable()
    }

    /// Spotify refused the built-in player and Soloist is set up: make
    /// Soloist the engine from now on (Settings can switch back).
    pub fn prefer_soloist(cx: &mut App) {
        let soloist = app::soloist(cx).read(cx);
        if soloist.usable()
            && soloist.builtin_refused()
            && app::settings(cx).read(cx).engine == Engine::Automatic
        {
            app::update_settings(cx, |s| s.engine = Engine::Soloist);
        }
    }

    /// The engine setting changed: run the engine it names.
    pub fn apply_engine(&mut self, cx: &mut Context<Self>) {
        let session = app::session(cx).read(cx);
        if session.is_demo() || !session.streaming.has_credentials() {
            return;
        }
        if self.soloist_only(cx) {
            self.drop_backend();
        } else {
            self.reader = None;
            self.reader_starting = None;
        }
        self.ensure_backend(cx);
        cx.notify();
    }

    /// Connect the read-only session, then bring back the last session
    /// (on Soloist) as the built-in player would.
    fn ensure_reader(&mut self, cx: &mut Context<Self>) {
        if self.reader.is_some() || self.reader_starting.is_some() {
            return;
        }
        // Up before the loading screen goes, so this computer can be picked
        // in the device list even while another device plays.
        app::soloist(cx).update(cx, |s, cx| s.start(cx));
        let auth = app::session(cx).read(cx).streaming.clone();
        // On Soloist, Lilac's own record (`LastSession`, written from
        // Soloist's state) says where the user left off, and Soloist
        // reports itself once running: Spotify's Web API need not be asked.
        // Something playing on another device shows once that device
        // changes (the cluster) or the device list is opened.
        // What other devices play is only Spotify's to say; that one Web
        // API request stays. Where the user left off here is Lilac's own
        // record on Soloist, so the last played track is not asked.
        let api = app::api(cx);
        let soloist = self.plays_on_soloist(cx);
        self.restore_lookup = Some(runtime::spawn(async move {
            if soloist {
                let state = api.remote_state().await.map_err(|e| format!("{e:#}"));
                (state, None)
            } else {
                let (state, last) = tokio::join!(api.remote_state(), api.last_played());
                (state.map_err(|e| format!("{e:#}")), last.ok().flatten())
            }
        }));
        let connect = runtime::spawn(async move {
            let token = auth.token().await?;
            crate::spotify::streaming::connect_session(token).await
        });
        self.reader_starting = Some(cx.spawn(async move |this, cx| {
            let result = connect
                .await
                .unwrap_or_else(|err| Err(anyhow::anyhow!("background task failed: {err}")));
            this.update(cx, |this, cx| {
                this.reader_starting = None;
                match result {
                    Ok(session) => this.reader = Some(session),
                    Err(err) => app::toast_error(format!("{err:#}"), cx),
                }
                // The device list is the Web API's; on Soloist it is read
                // when the Devices panel opens.
                if !this.plays_on_soloist(cx) {
                    this.refresh_devices(Duration::from_millis(500), cx);
                }
                this.restore_session(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// When locked, open the setup and say so: the caller must not play.
    fn setup_instead(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.locked(cx) {
            return false;
        }
        cx.defer(crate::soloist::Soloist::open_setup);
        true
    }

    /// Keep the built-in player off Spotify's device list, or bring it back.
    pub fn hide_builtin(&mut self, hide: bool, cx: &mut Context<Self>) {
        if self.builtin_hidden == hide {
            return;
        }
        self.builtin_hidden = hide;
        if hide {
            self.drop_backend();
            self.devices
                .retain(|d| d.name != crate::soloist::DEVICE_NAME);
        } else {
            let session = app::session(cx).read(cx);
            if !session.is_demo() && session.streaming.has_credentials() {
                self.ensure_backend(cx);
            }
        }
        cx.notify();
    }

    /// Spotify may hand out keys again: forget the refusal and let the
    /// built-in player try.
    pub fn retry_builtin(&mut self, cx: &mut Context<Self>) {
        app::soloist(cx).update(cx, |s, cx| s.forget_builtin_refused(cx));
        self.halted = false;
        self.failed_in_row = 0;
        cx.emit(PlayerEvent::StateChanged);
        self.resume(cx);
    }

    // ----------------------------------------------------------------- commands

    /// Play `tracks` as a context, starting at `index`.
    pub fn play_context(
        &mut self,
        context: Option<ContextUri>,
        name: impl Into<SharedString>,
        tracks: Vec<Track>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        // Skip forward to the first playable track from the requested position.
        let index = (index..tracks.len())
            .find(|ix| tracks[*ix].playable())
            .unwrap_or(index);
        if self.simulated(cx) {
            let shuffle = self.queue.shuffled();
            match self
                .queue
                .set_context(context, name.into(), tracks, index, shuffle)
            {
                Some(track) => self.start_simulated(track, cx),
                None => app::toast("Nothing to play here", cx),
            }
            return;
        }

        if self.setup_instead(cx) {
            return;
        }
        let Some(first) = tracks.get(index).cloned() else {
            app::toast("Nothing to play here", cx);
            return;
        };
        let context_uri = context.as_ref().and_then(|c| self.context_uri(c));
        let uris: Vec<String> = tracks
            .iter()
            .filter(|t| t.playable())
            .map(|t| t.uri.to_string())
            .collect();
        let start = match &context_uri {
            Some(_) => Start::Uri(first.uri.to_string()),
            None => Start::Index(
                uris.iter()
                    .position(|u| *u == first.uri.as_ref())
                    .unwrap_or(0) as u32,
            ),
        };

        // Show the choice at once, loading; whether it plays is up to the
        // speaker here, or Spotify for another device.
        self.current = Some(first);
        self.connect_context = context_uri.clone();
        self.anchor(0);
        if self.heard().is_none() {
            self.playing = true;
        }
        self.loading = true;
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();

        // Soloist playing (or meant to play) is told directly; checked
        // before other devices, which Soloist also counts as once active.
        let elsewhere = self.remote_device().is_some() && !self.soloist_active(cx);
        if !elsewhere && (self.soloist_active(cx) || self.plays_on_soloist(cx)) {
            let offset = match &start {
                Start::Uri(uri) => Some(uri.clone()),
                Start::Index(ix) => uris.get(*ix as usize).cloned(),
            };
            self.started_tracks = uris.clone();
            self.play_on_soloist(context_uri, uris, offset, 0, cx);
            return;
        }
        if self.remote_device().is_some() {
            let api = app::api(cx);
            let offset = match &start {
                Start::Uri(uri) => Some(uri.clone()),
                Start::Index(ix) => uris.get(*ix as usize).cloned(),
            };
            // Soloist starts a list from its top, or a single track, on the
            // device playing; a start part-way needs the Web API.
            let context = remote_context(context_uri.clone());
            let from_top = match &context {
                Some(_) => offset.is_none() || uris.first() == offset.as_ref(),
                None => uris.len() == 1,
            };
            let uri = context.or(offset.clone());
            if from_top
                && let Some(uri) = uri
                && self.plays_on_soloist(cx)
                && app::soloist(cx)
                    .read(cx)
                    .command("play", json!({ "uri": uri }))
            {
                return;
            }
            // Spotify takes no starting track for an artist context; send
            // the tracks themselves.
            let context_uri = remote_context(context_uri);
            let tracks = context_uri.is_none().then_some(uris);
            remote(cx, async move {
                api.remote_play(
                    None,
                    context_uri.as_deref(),
                    tracks.as_deref(),
                    offset.as_deref(),
                    0,
                )
                .await
            });
            return;
        }
        self.started_tracks = uris.clone();
        self.load(
            PendingLoad {
                context: context_uri,
                tracks: uris,
                start,
            },
            cx,
        );
    }

    /// A stored login is enough to start connecting; checking it against the
    /// Web API (which the session does at the same time) need not come
    /// first. Called once app state exists.
    pub fn start_with_stored_login(&mut self, cx: &mut Context<Self>) {
        let session = app::session(cx).read(cx);
        if session.status == crate::session::Status::Starting
            && !session.is_demo()
            && session.streaming.has_credentials()
        {
            self.warmup = Warmup::Connecting;
            self.show_saved_session(cx);
            Self::prefer_soloist(cx);
            self.ensure_backend(cx);
        }
    }

    /// Show the track Lilac was left on straight from disk; the streaming
    /// session takes seconds to load it, and the loading screen need not
    /// wait for that.
    fn show_saved_session(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = LastSession::load() else {
            return;
        };
        if let Some(track) = saved.track() {
            self.current = Some(track);
            self.playing = false;
            // Play picks up in the same list, at the same place.
            self.connect_context = saved.context_uri.clone();
            self.started_tracks = saved.tracks.clone();
            self.anchor(saved.position_ms);
            cx.emit(PlayerEvent::TrackChanged);
            cx.notify();
        }
    }

    /// The warm-up is over, whichever way it ended; a Play pressed during it
    /// happens now.
    fn finish_warmup(&mut self, cx: &mut Context<Self>) {
        self.warmup = Warmup::Ready;
        if std::mem::take(&mut self.play_when_ready) {
            self.loading = false;
            self.resume(cx);
        }
    }

    /// Bring the account's last session to this device, paused and buffered
    /// at the position it was left at, so the first Play is instant. Not
    /// when another device is playing: that would stop it.
    fn restore_session(&mut self, cx: &mut Context<Self>) {
        self.warmup = Warmup::Restoring;
        cx.notify();
        let lookup = self.restore_lookup.take();
        self.restore = Some(cx.spawn(async move |this, cx| {
            let (state, last) = match lookup {
                Some(lookup) => lookup.await.unwrap_or((Ok(None), None)),
                None => (Ok(None), None),
            };
            let state = match state {
                Ok(state) => state,
                Err(err) => {
                    cx.update(|cx| {
                        app::toast_error(
                            format!("Couldn't retrieve what your devices are playing: {err}"),
                            cx,
                        )
                    });
                    None
                }
            };
            let restoring = this
                .update(cx, |this, cx| this.start_restore(state, last, cx))
                .unwrap_or(false);
            if restoring {
                // Ready once librespot reports the restored track (see
                // `on_engine_event`), or after the timeout regardless.
                cx.background_executor().timer(RESTORE_TIMEOUT).await;
            }
            this.update(cx, |this, cx| {
                if this.warmup != Warmup::Ready && this.current.is_none() {
                    // Nothing came back: at least warm the connections.
                    this.warm_up(cx);
                }
                this.restore = None;
                if restoring && this.warmup == Warmup::Restoring {
                    // Spotify is slow to take the load, but it is queued: a
                    // Play pressed meanwhile waits for it (see `Paused`)
                    // rather than racing it with a transfer.
                    this.warmup = Warmup::Ready;
                } else {
                    this.finish_warmup(cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Decide what to bring back, in order of preference: what another
    /// device is playing (shown, not taken over), a session Spotify still
    /// holds, the session Lilac saved itself, the last played track.
    /// Returns whether librespot was asked to load something.
    fn start_restore(
        &mut self,
        state: Option<crate::model::RemoteState>,
        last: Option<crate::model::PlayedItem>,
        cx: &mut Context<Self>,
    ) -> bool {
        let last = last.and_then(|i| {
            let played_at = resume::parse_utc(&i.played_at).unwrap_or(0);
            let context = i.context.map(|c| c.uri).filter(|u| !u.is_empty());
            Some((i.item?, context, played_at))
        });
        let ours = self.device_id().map(str::to_string);
        if self.backend().is_none() && !self.plays_on_soloist(cx) {
            return false;
        }
        if let Some(state) = state.filter(|s| s.item.is_some()) {
            let device = state.device.as_ref().and_then(|d| d.id.clone());
            if state.is_playing && device != ours {
                // Soloist comes up all the same: it is what "This computer"
                // in the device list moves playback to.
                if self.plays_on_soloist(cx) {
                    app::soloist(cx).update(cx, |s, cx| s.start(cx));
                }
                // Show what the other device plays; the controls drive it.
                // It must be in the list for that (which is not read at
                // start-up on Soloist).
                if let Some(remote) = &state.device
                    && let Some(id) = remote.id.clone()
                    && !self.devices.iter().any(|d| d.id == id)
                {
                    self.devices.push(Device {
                        id,
                        name: remote.name.clone(),
                        kind: device_kind(&remote.kind),
                        volume: remote.supports_volume,
                    });
                }
                self.active_device = device;
                self.current = state.item;
                self.playing = true;
                self.anchor(state.progress_ms.unwrap_or(0));
                cx.emit(PlayerEvent::TrackChanged);
                return false;
            }
            if self.plays_on_soloist(cx) || self.locked(cx) {
                // Bringing it to the built-in player would only ask Spotify
                // for keys it refuses; Play starts it on Soloist (or opens
                // its setup).
                self.current = state.item;
                self.playing = false;
                self.connect_context = state.context.map(|c| c.uri).filter(|u| !u.is_empty());
                self.anchor(state.progress_ms.unwrap_or(0));
                cx.emit(PlayerEvent::TrackChanged);
                if self.plays_on_soloist(cx) {
                    app::soloist(cx).update(cx, |s, cx| s.start(cx));
                }
                return false;
            }
            // Another device holds a paused session: bring it here, paused.
            // Fetching the track needs no activation, which takes librespot
            // a couple of seconds after connecting; overlap the two.
            if let (Some(backend), Some(item)) = (self.backend(), &state.item) {
                backend.preload(&item.uri);
            }
            if self.backend().is_some_and(|b| b.restore_paused().is_ok()) {
                self.current = state.item;
                self.playing = false;
                self.anchor(state.progress_ms.unwrap_or(0));
                cx.emit(PlayerEvent::TrackChanged);
                return true;
            }
        }

        if self.locked(cx) {
            // What was saved is on screen already (`show_saved_session`).
            return false;
        }
        if self.plays_on_soloist(cx) {
            // What was saved is on screen already (`show_saved_session`);
            // have Soloist up by the time Play is pressed.
            app::soloist(cx).update(cx, |s, cx| s.start(cx));
            return false;
        }
        // Our own record is exact (position included) unless something was
        // played elsewhere after it was written. Spotify lists a track as
        // played only after about 30 seconds, so the same track counts too.
        let saved = LastSession::load().filter(|saved| {
            last.as_ref().is_none_or(|(track, _, played_at)| {
                track.uri.as_ref() == saved.track_uri || saved.saved_at >= *played_at
            })
        });
        let session = match (saved, &last) {
            (Some(saved), _) => saved,
            (None, Some((track, context, _))) => {
                LastSession::new(&track.uri, context.as_deref(), &[track.uri.to_string()], 0)
            }
            (None, None) => return false,
        };
        let start = match &session.context_uri {
            Some(_) => Start::Uri(session.track_uri.clone()),
            None => Start::Index(
                session
                    .tracks
                    .iter()
                    .position(|t| *t == session.track_uri)
                    .unwrap_or(0) as u32,
            ),
        };
        let tracks = if session.tracks.is_empty() {
            vec![session.track_uri.clone()]
        } else {
            session.tracks.clone()
        };
        let Some(backend) = self.backend() else {
            return false;
        };
        backend.preload(&session.track_uri);
        if backend
            .load(
                session.context_uri.as_deref(),
                &tracks,
                start,
                self.connect_shuffle,
                false,
                Some(session.position_ms),
            )
            .is_err()
        {
            return false;
        }
        self.connect_context = session.context_uri.clone();
        self.started_tracks = tracks;
        if let Some((track, _, _)) = last.filter(|(t, _, _)| t.uri.as_ref() == session.track_uri) {
            // Show it at once; librespot confirms with its own events.
            self.current = Some(track);
            self.playing = false;
            self.anchor(session.position_ms);
            cx.emit(PlayerEvent::TrackChanged);
        }
        true
    }

    /// Remember what plays and where, for the next start.
    fn save_session(&self, cx: &App) {
        let Some(track) = &self.current else {
            return;
        };
        // What this computer plays: the built-in player, or Soloist.
        let here = if self.soloist_active(cx) {
            true
        } else {
            self.backend.is_some() && self.remote_device().is_none()
        };
        if !here {
            return;
        }
        LastSession::new(
            &track.uri,
            self.connect_context.as_deref(),
            &self.started_tracks,
            self.position_ms(),
        )
        .with_track(track)
        .save();
    }

    /// Before the first play, every connection a track needs (CDN, audio
    /// keys, storage lookups) is still cold, which adds a second or more.
    /// Preloading the last played track opens them, and makes picking up
    /// where the user left off start at once.
    fn warm_up(&mut self, cx: &mut Context<Self>) {
        if self.current.is_some() || self.plays_on_soloist(cx) || self.locked(cx) {
            return;
        }
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let Ok(recent) = runtime::run(async move { api.recently_played().await }).await else {
                return;
            };
            this.update(cx, |this, _| {
                if this.current.is_some() {
                    return;
                }
                if let (Some(backend), Some(track)) =
                    (this.backend(), recent.iter().find(|t| t.playable()))
                {
                    backend.preload(&track.uri);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The user is about to play something here (a play button or a track
    /// is being pressed). Becoming the active device costs a round trip to
    /// Spotify that librespot must finish before it loads anything, so it is
    /// started now rather than after the click. Only when no other device
    /// is playing: that one would stop.
    pub fn prepare_to_play(&mut self) {
        if self.spirc_active || self.remote_device().is_some() {
            return;
        }
        if let Some(backend) = self.backend()
            && backend.activate().is_ok()
        {
            // Spirc handles commands in order, so a load sent after this
            // finds the device active.
            self.spirc_active = true;
        }
    }

    /// More tracks of `context` arrived, e.g. the next page of a long playlist.
    /// `start` is how many tracks of the context came before them; batches
    /// that do not continue the queue exactly are ignored, so two loaders
    /// feeding the same context cannot duplicate tracks. Connect resolves
    /// whole contexts itself, so this only matters for the local queue.
    pub fn extend_context(
        &mut self,
        context: &ContextUri,
        start: usize,
        tracks: Vec<Track>,
        cx: &mut Context<Self>,
    ) {
        if self.connected()
            || tracks.is_empty()
            || !self.is_context(context)
            || self.queue.context_len() != start
        {
            return;
        }
        self.queue.extend_context(tracks);
        cx.notify();
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.current.is_none() {
            if let Some(track) = self.queue.current().cloned()
                && self.simulated(cx)
            {
                self.start_simulated(track, cx);
            }
            return;
        }
        if self.play_when_ready {
            self.play_when_ready = false;
            self.loading = false;
            cx.emit(PlayerEvent::StateChanged);
            cx.notify();
        } else if self.playing || (self.loading && self.heard().is_some()) {
            self.pause(cx);
        } else {
            self.resume(cx);
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        // A pause the user asks for is meant.
        self.soloist_started_at = None;
        if self.heard().is_some() {
            // Shown as paused when the speaker goes quiet (`on_cue`).
            if let Some(backend) = self.backend() {
                backend.pause();
            }
            if self.loading {
                self.loading = false;
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            return;
        }
        if !self.playing {
            return;
        }
        self.anchor_ms = self.position_ms();
        self.anchor_at = Instant::now();
        self.playing = false;
        self.ticker = None;
        if self.remote_device().is_some() {
            if !self.soloist_command("pause", Value::Null, cx) {
                let api = app::api(cx);
                remote(cx, async move { api.remote_pause().await });
            }
        } else if let Some(backend) = self.backend() {
            backend.pause();
        } else if let Some(pending) = self.pending.take() {
            // Paused before the session was up: drop the load, keep the selection.
            drop(pending);
        }
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if self.playing || self.current.is_none() || self.setup_instead(cx) {
            return;
        }
        if self.remote_device().is_none()
            && self.plays_on_soloist(cx)
            && let Some(uri) = self.current.as_ref().map(|t| t.uri.to_string())
        {
            // Nothing plays yet: start where we were, on Soloist.
            let context = self.connect_context.clone();
            // A list Lilac did not start (Spotify's last state) is known only
            // by its URI: no tracks, so the start is not taken for its top.
            let tracks = if self.started_tracks.contains(&uri) {
                self.started_tracks.clone()
            } else if context.is_some() {
                Vec::new()
            } else {
                vec![uri.clone()]
            };
            let position = self.position_ms();
            self.play_on_soloist(context, tracks, Some(uri), position, cx);
            return;
        }
        if (self.starting.is_some() || self.restore.is_some()) && self.remote_device().is_none() {
            // The last session is still being loaded here; Play now would
            // race it. Play it the moment it is ready.
            self.play_when_ready = true;
            self.loading = true;
            cx.emit(PlayerEvent::StateChanged);
            cx.notify();
            return;
        }
        if self.halted
            && self.remote_device().is_none()
            && let Some(uri) = self.current.as_ref().map(|t| t.uri.to_string())
        {
            self.play_again(uri, cx);
            return;
        }
        if self.heard().is_some() {
            // Playing once the speaker is (`on_cue`); loading until then.
            self.loading = true;
        } else {
            self.anchor_at = Instant::now();
            self.playing = true;
        }
        if self.simulated(cx) {
            self.start_ticker(cx);
        } else if self.remote_device().is_some() {
            if !self.soloist_command("play", Value::Null, cx) {
                let api = app::api(cx);
                remote(cx, async move {
                    api.remote_play(None, None, None, None, 0).await
                });
            }
        } else if let Some(backend) = self.backend() {
            // Spotify forgets the active device after a while paused, and
            // librespot ignores Play until it is active again.
            if !self.spirc_active
                && let Err(err) = backend.take_over()
            {
                app::toast_error(format!("{err:#}"), cx);
            }
            // librespot ignores the transfer when it still holds the session
            // after all; the Play then does it. After a transfer it is a no-op.
            backend.play();
        } else if let Some(track) = self.current.clone() {
            // The session dropped while paused: pick up where we were.
            let position = self.anchor_ms;
            self.load(
                PendingLoad {
                    context: None,
                    tracks: vec![track.uri.to_string()],
                    start: Start::Index(0),
                },
                cx,
            );
            self.seek(position, cx);
        }
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(backend) = self.backend() {
            backend.pause();
        }
        self.pending = None;
        self.queue = Queue::default();
        self.current = None;
        self.playing = false;
        self.loading = false;
        self.anchor(0);
        self.track_switch = None;
        self.ticker = None;
        self.connect_context = None;
        self.mirrored_next.clear();
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        self.retry = None;
        if self.setup_instead(cx) {
            return;
        }
        if self.simulated(cx) {
            // An explicit skip leaves "repeat one" for the next track, like the official clients.
            let repeat = if self.repeat == Repeat::Track {
                Repeat::Context
            } else {
                self.repeat
            };
            match self.advance_to_playable(repeat) {
                Some(track) => self.start_simulated(track, cx),
                None => self.finish(cx),
            }
        } else if self.remote_device().is_some() {
            if !self.soloist_command("skip_next", Value::Null, cx) {
                let api = app::api(cx);
                remote(cx, async move { api.remote_next().await });
            }
        } else if let Some(backend) = self.backend() {
            backend.next();
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        self.retry = None;
        if self.setup_instead(cx) {
            return;
        }
        if !self.simulated(cx) {
            // Spotify restarts the track or goes back itself, by the same rule.
            if self.remote_device().is_some() {
                if !self.soloist_command("skip_prev", Value::Null, cx) {
                    let api = app::api(cx);
                    remote(cx, async move { api.remote_previous().await });
                }
            } else if let Some(backend) = self.backend() {
                backend.previous();
            }
            return;
        }
        if self.position_ms() > RESTART_THRESHOLD_MS {
            self.seek(0, cx);
            return;
        }
        let repeat = if self.repeat == Repeat::Track {
            Repeat::Context
        } else {
            self.repeat
        };
        match self.queue.retreat(repeat) {
            Some(track) => self.start_simulated(track, cx),
            None => self.seek(0, cx),
        }
    }

    pub fn seek(&mut self, position_ms: u32, cx: &mut Context<Self>) {
        if self.current.is_none() {
            return;
        }
        let position_ms = position_ms.min(self.duration_ms());
        if self.heard().is_none() {
            self.anchor(position_ms);
        }
        if self.seek_settle.is_some() {
            self.pending_seek = Some(position_ms);
        } else {
            self.send_seek(position_ms, cx);
            // While seeks keep coming, send at most one per settle period.
            self.seek_settle = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(COMMAND_SETTLE).await;
                    let sent = this
                        .update(cx, |this, cx| match this.pending_seek.take() {
                            Some(position_ms) => {
                                this.send_seek(position_ms, cx);
                                true
                            }
                            None => {
                                this.seek_settle = None;
                                false
                            }
                        })
                        .unwrap_or(false);
                    if !sent {
                        break;
                    }
                }
            }));
        }
        cx.emit(PlayerEvent::Seeked);
        cx.notify();
    }

    fn send_seek(&mut self, position_ms: u32, cx: &mut Context<Self>) {
        if self.heard().is_some() {
            // The queue is dropped with the seek, so from here on what is
            // heard starts at the new position.
            self.anchor(position_ms);
        }
        if self.remote_device().is_some() {
            if !self.soloist_command("seek", json!({ "position_ms": position_ms }), cx) {
                let api = app::api(cx);
                remote(cx, async move { api.remote_seek(position_ms).await });
            }
        } else if let Some(backend) = self.backend() {
            backend.seek(position_ms);
        }
    }

    /// Seek relative to the current position.
    pub fn seek_by(&mut self, delta_ms: i32, cx: &mut Context<Self>) {
        let target = (self.position_ms() as i64 + delta_ms as i64).max(0) as u32;
        self.seek(target, cx);
    }

    /// Takes effect at once; Spotify and the settings file hear about it
    /// once the gesture settles.
    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = false;
        self.apply_volume(cx);
        cx.notify();
    }

    /// The volume was changed outside Lilac (the desktop's mixer).
    #[cfg(target_os = "linux")]
    fn adopt_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        if (self.muted && volume == 0.0) || (volume - self.effective_volume()).abs() < 0.005 {
            return;
        }
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = false;
        // Spotify and the settings file hear about it like any other change.
        self.apply_volume(cx);
        cx.notify();
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.apply_volume(cx);
        cx.notify();
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        let on = !self.shuffle();
        if self.simulated(cx) {
            self.queue.set_shuffle(on);
        } else {
            self.connect_shuffle = on;
            if self.remote_device().is_some() {
                if !self.soloist_command("set_shuffle", json!({ "enabled": on }), cx) {
                    let api = app::api(cx);
                    remote(cx, async move { api.remote_shuffle(on).await });
                }
            } else if let Some(backend) = self.backend() {
                backend.set_shuffle(on);
            }
        }
        cx.notify();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = self.repeat.next();
        let repeat = self.repeat;
        // Soloist has a switch for each kind; the one being turned off goes
        // first, as its documentation orders them.
        let (track, context) = (repeat == Repeat::Track, repeat == Repeat::Context);
        let soloist = if track {
            self.soloist_command("set_repeat_context", json!({ "enabled": false }), cx)
                && self.soloist_command("set_repeat_track", json!({ "enabled": true }), cx)
        } else {
            self.soloist_command("set_repeat_track", json!({ "enabled": false }), cx)
                && self.soloist_command("set_repeat_context", json!({ "enabled": context }), cx)
        };
        if !soloist && self.remote_device().is_some() {
            let api = app::api(cx);
            let state = match repeat {
                Repeat::Off => "off",
                Repeat::Context => "context",
                Repeat::Track => "track",
            };
            remote(cx, async move { api.remote_repeat(state).await });
        } else if !soloist && let Some(backend) = self.backend() {
            backend.set_repeat(repeat != Repeat::Off, repeat == Repeat::Track);
        }
        cx.notify();
    }

    pub fn enqueue(&mut self, track: Track, cx: &mut Context<Self>) {
        self.add_to_queue(track, false, cx);
    }

    pub fn play_next(&mut self, track: Track, cx: &mut Context<Self>) {
        self.add_to_queue(track, true, cx);
    }

    fn add_to_queue(&mut self, track: Track, front: bool, cx: &mut Context<Self>) {
        if !track.playable() {
            app::toast("This item can't be played", cx);
            return;
        }
        if self.current.is_none() && (self.queue.is_empty() || !self.simulated(cx)) {
            self.play_context(None, track.name.clone(), vec![track], 0, cx);
            return;
        }
        if !self.simulated(cx) {
            // Spotify plays queued tracks before the rest of the context, in
            // the order they were added; there is no separate "play next".
            if self.soloist_command("add_to_queue", json!({ "uri": track.uri.as_ref() }), cx) {
                app::toast("Added to queue", cx);
                return;
            }
            let api = app::api(cx);
            let uri = track.uri.to_string();
            cx.spawn(async move |_, cx| {
                let result = runtime::run(async move { api.remote_enqueue(&uri).await }).await;
                cx.update(|cx| match result {
                    Ok(()) => app::toast("Added to queue", cx),
                    Err(err) => app::toast_error(format!("{err:#}"), cx),
                });
            })
            .detach();
            return;
        }
        if front {
            self.queue.push_user_front(track);
            app::toast("Playing next", cx);
        } else {
            self.queue.push_user(track);
            app::toast("Added to queue", cx);
        }
        cx.notify();
    }

    pub fn remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.queue_editable() {
            self.queue.remove_user(index);
            cx.notify();
        }
    }

    pub fn clear_queue(&mut self, cx: &mut Context<Self>) {
        if self.queue_editable() {
            self.queue.clear_user();
            cx.notify();
        }
    }

    /// Jump to the `n`-th track of the manual queue.
    pub fn skip_to_queued(&mut self, n: usize, cx: &mut Context<Self>) {
        if !self.simulated(cx) {
            self.skip_ahead(n + 1, cx);
        } else if let Some(track) = self.queue.skip_to_user(n) {
            self.start_simulated(track, cx);
        }
    }

    /// Jump to the `n`-th upcoming track of the context.
    pub fn skip_to_upcoming(&mut self, n: usize, cx: &mut Context<Self>) {
        if !self.simulated(cx) {
            self.skip_ahead(n + 1, cx);
        } else if let Some(track) = self.queue.skip_to_upcoming(n) {
            self.start_simulated(track, cx);
        }
    }

    /// Move playback to another Connect device, or back to this one.
    pub fn transfer_to(&mut self, device_id: &str, cx: &mut Context<Self>) {
        if self.backend.is_none() && app::soloist(cx).read(cx).is_device(device_id) {
            // Soloist takes the session over itself, where it was.
            if !app::soloist(cx).read(cx).command("activate", Value::Null) {
                app::toast_error("Soloist isn't ready yet; try again in a moment", cx);
            }
            return;
        }
        if self.device_id() == Some(device_id) {
            match self.backend() {
                Some(backend) => {
                    if let Err(err) = backend.take_over() {
                        app::toast_error(format!("{err:#}"), cx);
                    }
                }
                None => self.ensure_backend(cx),
            }
            return;
        }
        self.moving_to = Some((device_id.to_string(), Instant::now()));
        let api = app::api(cx);
        let id = device_id.to_string();
        remote(cx, async move { api.transfer(&id, true).await });
    }

    /// Connect has no "jump to"; skipping in order lands on the same track.
    fn skip_ahead(&mut self, steps: usize, cx: &mut Context<Self>) {
        for _ in 0..steps.min(50) {
            self.next(cx);
        }
    }

    // ------------------------------------------------------------ demo playback

    fn simulated(&self, cx: &App) -> bool {
        app::session(cx).read(cx).is_demo()
    }

    fn advance_to_playable(&mut self, repeat: Repeat) -> Option<Track> {
        // Bounded so a context made only of unplayable tracks cannot spin forever.
        for _ in 0..=self.queue.context_len() {
            let track = self.queue.advance(repeat)?;
            if track.playable() {
                return Some(track);
            }
        }
        None
    }

    fn start_simulated(&mut self, track: Track, cx: &mut Context<Self>) {
        self.current = Some(track);
        self.anchor_ms = 0;
        self.anchor_at = Instant::now();
        self.playing = true;
        self.start_ticker(cx);
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    /// The current track played to its end.
    fn track_ended(&mut self, cx: &mut Context<Self>) {
        if self.repeat == Repeat::Track
            && let Some(track) = self.current.clone()
        {
            self.start_simulated(track, cx);
            return;
        }
        match self.advance_to_playable(self.repeat) {
            Some(track) => self.start_simulated(track, cx),
            None => self.finish(cx),
        }
    }

    /// Nothing left to play: rest at the start of the last track.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.loading = false;
        self.anchor_ms = 0;
        self.ticker = None;
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// The clock alone decides when a demo track is over.
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        self.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if !this.playing {
                            return false;
                        }
                        if this.duration_ms() > 0 && this.position_ms() >= this.duration_ms() {
                            this.track_ended(cx);
                        }
                        this.playing
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    // --------------------------------------------------------- Connect playback

    fn backend(&self) -> Option<&Backend> {
        self.backend.as_ref().filter(|b| b.is_usable())
    }

    fn drop_backend(&mut self) {
        if let Some(backend) = self.backend.take() {
            backend.shutdown();
        }
        self.listeners.clear();
        self.spirc_active = false;
        self.starting = None;
        self.pending = None;
    }

    /// The `spotify:` URI Connect knows `context` by.
    fn context_uri(&self, context: &ContextUri) -> Option<String> {
        Some(match context {
            ContextUri::Playlist(id) => format!("spotify:playlist:{id}"),
            ContextUri::Album(id) => format!("spotify:album:{id}"),
            ContextUri::Artist(id) => format!("spotify:artist:{id}"),
            ContextUri::Show(id) => format!("spotify:show:{id}"),
            ContextUri::Liked => return self.liked_uri.clone(),
            ContextUri::Tracks(_) => return None,
        })
    }

    fn load(&mut self, load: PendingLoad, cx: &mut Context<Self>) {
        // What plays next is decided anew; a switch still waiting for the
        // old queue to be heard, or a retry of a failed track, is moot.
        self.track_switch = None;
        self.retry = None;
        self.halted = false;
        self.failed_in_row = 0;
        // Quality, normalisation and gapless are fixed per librespot player.
        let options = Options::from_settings(app::settings(cx).read(cx));
        if self
            .backend
            .as_ref()
            .is_some_and(|b| b.options != options || !b.is_usable())
        {
            self.drop_backend();
        }
        match self.backend() {
            Some(backend) => {
                let shuffle = self.connect_shuffle;
                let active = self.spirc_active;
                if let Err(err) = backend.load(
                    load.context.as_deref(),
                    &load.tracks,
                    load.start,
                    shuffle,
                    active,
                    None,
                ) {
                    app::toast_error(format!("{err:#}"), cx);
                }
            }
            None => {
                self.pending = Some(load);
                self.ensure_backend(cx);
            }
        }
    }

    fn apply_volume(&mut self, cx: &mut Context<Self>) {
        self.volume_changed_at = Some(Instant::now());
        if self.remote_device().is_none()
            && let Some(backend) = self.backend()
        {
            backend.set_local_volume(self.effective_volume());
        }
        self.volume_sync = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMMAND_SETTLE).await;
            this.update(cx, |this, cx| this.sync_volume(cx)).ok();
        }));
    }

    /// Tell Spotify (and the settings file) where the volume ended up.
    fn sync_volume(&mut self, cx: &mut Context<Self>) {
        self.volume_sync = None;
        let volume = self.volume;
        app::update_settings(cx, |settings| settings.volume = volume);
        if self.remote_device().is_some() {
            let percent = (self.effective_volume() * 100.0).round() as u32;
            if !self.soloist_command("set_volume", json!({ "volume": percent }), cx) {
                let api = app::api(cx);
                remote(cx, async move { api.remote_volume(percent).await });
            }
        } else if let Some(backend) = self.backend() {
            backend.set_volume(self.effective_volume());
        }
    }

    /// Connect the streaming session in the background, then run any pending load.
    fn ensure_backend(&mut self, cx: &mut Context<Self>) {
        if self.soloist_only(cx) {
            self.ensure_reader(cx);
            return;
        }
        if self.backend.is_some() || self.starting.is_some() || self.builtin_hidden {
            return;
        }
        let options = Options::from_settings(app::settings(cx).read(cx));
        let auth = app::session(cx).read(cx).streaming.clone();
        let volume = self.effective_volume();
        // What to restore is asked while the streaming session connects.
        let api = app::api(cx);
        self.restore_lookup = Some(runtime::spawn(async move {
            let (state, last) = tokio::join!(api.remote_state(), api.last_played());
            (state.map_err(|e| format!("{e:#}")), last.ok().flatten())
        }));
        // Spawned here rather than inside the UI task: that is first polled
        // only after the window is up, and connecting need not wait for it.
        let start = runtime::spawn(Backend::start(auth, options, volume));
        self.starting = Some(cx.spawn(async move |this, cx| {
            let result = start
                .await
                .unwrap_or_else(|err| Err(anyhow::anyhow!("background task failed: {err}")));
            this.update(cx, |this, cx| {
                this.starting = None;
                match result {
                    Ok((backend, channels)) => {
                        this.listen(channels, cx);
                        this.backend = Some(backend);
                        // Our own registration needs a moment to show up in the list.
                        this.refresh_devices(Duration::from_millis(1500), cx);
                        this.restore_session(cx);
                        if let Some(load) = this.pending.take() {
                            this.load(load, cx);
                        }
                        cx.notify();
                    }
                    Err(err) => {
                        let had_request = this.pending.take().is_some();
                        this.loading = false;
                        this.playing = false;
                        this.anchor_at = Instant::now();
                        // Joining Connect quietly at login may fail offline; only a
                        // failed play is worth interrupting the user for.
                        if had_request {
                            app::toast_error(format!("{err:#}"), cx);
                        }
                        this.play_when_ready = false;
                        this.warmup = Warmup::Ready;
                        cx.emit(PlayerEvent::StateChanged);
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn listen(&mut self, channels: Channels, cx: &mut Context<Self>) {
        let Channels {
            mut engine,
            mut cluster,
            mut cues,
        } = channels;
        self.listeners = vec![
            cx.spawn(async move |this, cx| {
                while let Some(event) = engine.recv().await {
                    if this
                        .update(cx, |this, cx| this.on_engine_event(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                while let Some(cue) = cues.recv().await {
                    if this.update(cx, |this, cx| this.on_cue(cue, cx)).is_err() {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                while let Some(update) = cluster.recv().await {
                    if this
                        .update(cx, |this, cx| this.on_cluster(update, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }),
        ];
    }

    /// Events from this device's own player.
    fn on_engine_event(&mut self, event: EngineEvent, cx: &mut Context<Self>) {
        // Only the active device plays; Spotify will not tell us so itself.
        if matches!(
            event,
            EngineEvent::Loading { .. } | EngineEvent::Playing { .. }
        ) {
            self.mark_active();
        }
        if self.remote_device().is_some() {
            return;
        }
        match event {
            EngineEvent::TrackChanged { audio_item } => {
                // librespot reports the next track when it starts decoding
                // it; the end of the old one is still queued for the speaker.
                let wait = self.heard().map(|out| out.queued_ms()).unwrap_or(0);
                if self.playing && wait > 20 {
                    self.track_switch = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(wait as u64))
                            .await;
                        this.update(cx, |this, cx| this.track_changed(&audio_item, cx))
                            .ok();
                    }));
                } else {
                    self.track_switch = None;
                    self.track_changed(&audio_item, cx);
                }
            }
            EngineEvent::Playing { position_ms, .. } => {
                self.failed_in_row = 0;
                self.anchor_reported(position_ms);
                if self.heard().is_none() {
                    self.loading = false;
                    self.playing = true;
                }
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Paused { position_ms, .. } => {
                let restored = self.warmup == Warmup::Restoring;
                self.failed_in_row = 0;
                self.loading = false;
                self.playing = false;
                self.anchor_reported(position_ms);
                if restored {
                    // The restored track is loaded and buffered, and this
                    // device holds the session: Play needs no transfer.
                    self.mark_active();
                    self.restore = None;
                    self.finish_warmup(cx);
                } else if std::mem::take(&mut self.play_when_ready) {
                    // The restore arrived after its timeout, with Play pressed.
                    self.loading = false;
                    self.resume(cx);
                }
                self.save_session(cx);
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Seeked { position_ms, .. } => {
                self.anchor_reported(position_ms);
                self.save_session(cx);
                cx.notify();
            }
            EngineEvent::PositionCorrection { position_ms, .. }
            | EngineEvent::PositionChanged { position_ms, .. } => {
                self.anchor_reported(position_ms);
                cx.notify();
            }
            EngineEvent::Stopped { .. } => {
                self.playing = false;
                self.loading = false;
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Loading { .. } if self.halted => {
                // Spirc's skip past the last failure can arrive after the
                // stop; stop it too.
                if let Some(backend) = self.backend() {
                    backend.stop();
                }
            }
            EngineEvent::Loading { track_id, .. } => {
                self.loading_uri = track_id.to_uri().ok();
            }
            EngineEvent::Unavailable { track_id, .. } => {
                // librespot reports a failed preload of the upcoming track
                // the same way; Spotify skips that one when it comes up, and
                // what plays now is fine. Only a track asked to play is news.
                let uri = track_id.to_uri().unwrap_or_default();
                if self.loading_uri.as_deref() != Some(uri.as_str()) {
                    return;
                }
                self.loading_uri = None;
                self.failed_in_row += 1;
                if self.failed_in_row >= MAX_FAILED_IN_ROW {
                    self.halt(cx);
                    return;
                }
                let attempts = match &self.retried {
                    Some((last, n)) if *last == uri => *n,
                    _ => 0,
                };
                // A paused load (a restored session) is not retried: the retry
                // would start playing what nobody asked to hear.
                let wanted = self.loading || self.play_when_ready;
                if wanted && attempts < UNAVAILABLE_RETRIES {
                    // Spotify refuses a track's decryption key now and then
                    // and librespot gives up on the first refusal; asking
                    // again usually works.
                    self.retried = Some((uri.clone(), attempts + 1));
                    self.retry = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(RETRY_DELAY * (attempts + 1))
                            .await;
                        this.update(cx, |this, cx| this.play_again(uri, cx)).ok();
                    }));
                    return;
                }
                self.retried = None;
                let name = self
                    .current
                    .iter()
                    .chain(self.queue.current())
                    .chain(self.queue.user.iter())
                    .chain(self.queue.upcoming())
                    .find(|t| t.uri.as_ref() == uri)
                    .map(|t| format!("\u{201c}{}\u{201d}", t.name))
                    .unwrap_or_else(|| "A track".into());
                // Spotify moves on to the next track by itself.
                app::toast_error(format!("{name} isn't available, skipping it"), cx);
            }
            EngineEvent::ShuffleChanged { shuffle } => {
                self.connect_shuffle = shuffle;
                cx.notify();
            }
            EngineEvent::RepeatChanged { context, track } => {
                self.repeat = match (context, track) {
                    (_, true) => Repeat::Track,
                    (true, false) => Repeat::Context,
                    (false, false) => Repeat::Off,
                };
                cx.notify();
            }
            EngineEvent::VolumeChanged { .. } if self.warmup != Warmup::Ready => {
                // On activation librespot applies the volume Spotify stored
                // for this device last time; Lilac's own setting wins.
                if let Some(backend) = self.backend() {
                    backend.set_local_volume(self.effective_volume());
                    backend.set_volume(self.effective_volume());
                }
            }
            EngineEvent::VolumeChanged { volume } => {
                // Our own changes come back here too, late and possibly stale.
                let echo = self.volume_sync.is_some()
                    || self
                        .volume_changed_at
                        .is_some_and(|at| at.elapsed() < VOLUME_ECHO);
                if !echo && !(self.muted && volume == 0) {
                    self.volume = backend::from_mixer(volume);
                    self.muted = false;
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    /// Stop librespot from skipping on through tracks that will not load.
    fn halt(&mut self, cx: &mut Context<Self>) {
        // Whether someone is waiting for sound, rather than a quiet restore.
        let wanted = self.loading || self.play_when_ready;
        if let Some(backend) = self.backend() {
            backend.stop();
        }
        self.halted = true;
        self.failed_in_row = 0;
        self.retried = None;
        self.retry = None;
        self.loading = false;
        self.playing = false;
        self.play_when_ready = false;
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();

        let soloist = app::soloist(cx);
        soloist.update(cx, |s, cx| s.record_builtin_refused(cx));
        let engine = app::settings(cx).read(cx).engine;
        let uri = self.current.as_ref().map(|t| t.uri.to_string());
        if engine != Engine::Builtin
            && soloist.read(cx).usable()
            && let Some(uri) = uri
        {
            // From now on Soloist plays and the built-in player stays off.
            Self::prefer_soloist(cx);
            let player = cx.entity().downgrade();
            cx.defer(move |cx| {
                player.update(cx, |p, cx| p.apply_engine(cx)).ok();
            });
            app::toast(
                "Spotify refused the built-in player; playing through Soloist",
                cx,
            );
            let context = self.connect_context.clone();
            let tracks = if self.started_tracks.contains(&uri) {
                self.started_tracks.clone()
            } else {
                vec![uri.clone()]
            };
            self.play_on_soloist(context, tracks, Some(uri), 0, cx);
            return;
        }
        // Locked now: the banner says so; someone pressing Play gets the
        // setup at once.
        if wanted {
            cx.defer(crate::soloist::Soloist::open_setup);
        }
    }

    /// Play the current track through Soloist, where it is (used when the
    /// setup finishes).
    pub fn play_here_on_soloist(&mut self, cx: &mut Context<Self>) {
        let Some(uri) = self.current.as_ref().map(|t| t.uri.to_string()) else {
            return;
        };
        let context = self.connect_context.clone();
        let tracks = if self.started_tracks.contains(&uri) {
            self.started_tracks.clone()
        } else {
            vec![uri.clone()]
        };
        let position = if self.halted { 0 } else { self.position_ms() };
        if let Some(backend) = self.backend() {
            backend.stop();
        }
        self.play_on_soloist(context, tracks, Some(uri), position, cx);
    }

    /// Soloist plays for this computer and is not logged in yet; the
    /// loading screen waits for it.
    pub fn waits_for_soloist(&self, cx: &App) -> bool {
        self.plays_on_soloist(cx) && app::soloist(cx).read(cx).coming_up()
    }

    /// Whether this computer plays through Soloist rather than the
    /// built-in player.
    fn plays_on_soloist(&self, cx: &App) -> bool {
        if self.simulated(cx) {
            return false;
        }
        let soloist = app::soloist(cx).read(cx);
        soloist.usable()
            && match app::settings(cx).read(cx).engine {
                Engine::Soloist => true,
                Engine::Builtin => false,
                Engine::Automatic => soloist.builtin_refused(),
            }
    }

    /// Play on Soloist, starting it first if it is not up.
    fn play_on_soloist(
        &mut self,
        context: Option<String>,
        tracks: Vec<String>,
        offset: Option<String>,
        position_ms: u32,
        cx: &mut Context<Self>,
    ) {
        let context = remote_context(context);
        self.soloist_started_at = Some(Instant::now());
        self.halted = false;
        self.loading = true;
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
        let api = app::api(cx);
        let player = cx.entity().downgrade();
        app::soloist(cx).update(cx, |soloist, cx| {
            soloist.when_ready(
                move |device_id, cx| {
                    if let Some(player) = player.upgrade() {
                        player.update(cx, |p, cx| {
                            // Soloist is the device playing from here on;
                            // the controls follow it.
                            if !p.devices.iter().any(|d| d.id == device_id) {
                                p.devices.push(Device {
                                    id: device_id.clone(),
                                    name: crate::soloist::DEVICE_NAME.into(),
                                    kind: DeviceKind::Computer,
                                    volume: true,
                                });
                            }
                            p.active_device = Some(device_id.clone());
                            cx.notify();
                        });
                    }
                    // Soloist is told directly: always for a list from its
                    // start (it plays the album or playlist itself), and for
                    // a start further in whenever Spotify cannot be asked.
                    // Only Spotify can start a list part-way; Soloist alone
                    // plays that track and queues the rest.
                    let from_top = context.is_some()
                        && offset.as_ref().is_none_or(|o| tracks.first() == Some(o));
                    if from_top
                        || device_id == crate::soloist::LOCAL_DEVICE
                        || crate::spotify::api::penalty_left().is_some()
                    {
                        // Soloist starts a URI at its beginning; the place
                        // is sought once that track plays.
                        if position_ms > 0
                            && let (Some(player), Some(uri)) = (player.upgrade(), offset.clone())
                        {
                            player
                                .update(cx, |p, _| p.seek_when_playing = Some((uri, position_ms)));
                        }
                        play_locally(context.as_deref(), &tracks, offset.as_deref(), cx);
                        return;
                    }
                    let local_tracks = tracks.clone();
                    let local_context = context.clone();
                    let local_offset = offset.clone();
                    let tracks = context.is_none().then_some(tracks);
                    cx.spawn(async move |cx| {
                        let call = async move {
                            api.remote_play(
                                Some(&device_id),
                                context.as_deref(),
                                tracks.as_deref(),
                                offset.as_deref(),
                                position_ms,
                            )
                            .await
                        };
                        if runtime::run(call).await.is_err() {
                            cx.update(|cx| {
                                play_locally(
                                    local_context.as_deref(),
                                    &local_tracks,
                                    local_offset.as_deref(),
                                    cx,
                                )
                            });
                        }
                    })
                    .detach();
                },
                cx,
            )
        });
    }

    /// Spotify listed Soloist: swap its stand-in id for the real one.
    pub fn soloist_id_known(&mut self, id: String, cx: &mut Context<Self>) {
        if self.active_device.as_deref() == Some(crate::soloist::LOCAL_DEVICE) {
            self.active_device = Some(id.clone());
        }
        for device in &mut self.devices {
            if device.id == crate::soloist::LOCAL_DEVICE {
                device.id = id.clone();
            }
        }
        cx.notify();
    }

    /// Soloist's own report of what it plays. While Soloist is the device
    /// playing, this is what the controls show: no lookup, no waiting for
    /// Spotify, and no guess left hanging when changes come fast.
    pub fn on_soloist_state(&mut self, state: crate::soloist::ws::State, cx: &mut Context<Self>) {
        let Some(device) = app::soloist(cx).read(cx).device_id.clone() else {
            return;
        };
        if !state.is_active {
            self.mirror_through_soloist(state, cx);
            return;
        }
        self.active_device = Some(device);
        self.halted = false;
        self.play_when_ready = false;
        // Something (a session handover at start-up) can pause Soloist a
        // moment after Lilac started it; nobody asked for that pause.
        if state.status == "paused"
            && self
                .soloist_started_at
                .take()
                .is_some_and(|at| at.elapsed() < Duration::from_secs(4))
            && self.soloist_command("play", Value::Null, cx)
        {
            return;
        }
        if state.status == "playing"
            && let Some((uri, position_ms)) = self.seek_when_playing.take()
        {
            let playing = state.track.as_ref().and_then(|t| t["uri"].as_str());
            if playing == Some(uri.as_str()) {
                self.soloist_command("seek", json!({ "position_ms": position_ms }), cx);
            }
        }
        self.show_soloist_state(state, true, cx);
        // Lilac's own record of what played, so a restart can show it
        // without asking Spotify.
        self.save_session(cx);
        cx.notify();
    }

    /// Soloist follows the account's session while another device plays
    /// it, and reports that device's playback as its own, marked inactive.
    /// It does not say which device that is; the Web API is asked once.
    fn mirror_through_soloist(&mut self, state: crate::soloist::ws::State, cx: &mut Context<Self>) {
        if !self.plays_on_soloist(cx) {
            return;
        }
        if !self.mirrors_through_soloist(cx) {
            // A paused session nobody plays (Soloist reports one when it
            // starts) is the restore's to show, not another device's.
            if !matches!(state.status.as_str(), "playing" | "buffering") {
                return;
            }
            self.playing_elsewhere(cx);
        }
        self.show_soloist_state(state, false, cx);
        cx.notify();
    }

    /// Soloist took the session over, or another device took it from
    /// Soloist. Soloist sends no new state with that, so it is asked for one.
    pub fn on_soloist_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if active {
            let Some(device) = app::soloist(cx).read(cx).device_id.clone() else {
                return;
            };
            self.active_device = Some(device);
            self.devices.retain(|d| d.id != OTHER_DEVICE);
        } else if self.soloist_active(cx) && self.plays_on_soloist(cx) {
            self.playing_elsewhere(cx);
        } else {
            return;
        }
        app::soloist(cx).read(cx).command("get_state", Value::Null);
        self.refresh_queue(cx);
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// Another device plays: the one Lilac just moved playback to, or one
    /// Spotify is yet to name.
    fn playing_elsewhere(&mut self, cx: &mut Context<Self>) {
        if let Some((id, at)) = self.moving_to.take()
            && at.elapsed() < Duration::from_secs(15)
            && self.devices.iter().any(|d| d.id == id)
        {
            self.devices.retain(|d| d.id != OTHER_DEVICE);
            self.active_device = Some(id);
            return;
        }
        self.active_device = Some(OTHER_DEVICE.to_string());
        if !self.devices.iter().any(|d| d.id == OTHER_DEVICE) {
            self.devices.push(Device {
                id: OTHER_DEVICE.to_string(),
                name: "another device".into(),
                kind: DeviceKind::Other,
                volume: true,
            });
        }
        self.name_other_device(cx);
    }

    /// Another device plays and Soloist reports it (see `mirror_through_soloist`).
    fn mirrors_through_soloist(&self, cx: &App) -> bool {
        self.plays_on_soloist(cx) && self.remote_device().is_some() && !self.soloist_active(cx)
    }

    /// Learn which device plays in place of `OTHER_DEVICE`. Spotify may
    /// still list Soloist as active for a moment, so it is asked again.
    fn name_other_device(&mut self, cx: &mut Context<Self>) {
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            for delay in [0u64, 1500, 4000] {
                cx.background_executor()
                    .timer(Duration::from_millis(delay))
                    .await;
                let api = api.clone();
                let state = runtime::run(async move { api.remote_state().await }).await;
                let device = state.ok().flatten().and_then(|s| s.device);
                let done = this
                    .update(cx, |this, cx| {
                        if this.active_device.as_deref() != Some(OTHER_DEVICE) {
                            return true;
                        }
                        let Some(device) = device else { return false };
                        let Some(id) = device.id.clone() else {
                            return false;
                        };
                        if app::soloist(cx).read(cx).is_device(&id) {
                            return false;
                        }
                        this.devices.retain(|d| d.id != OTHER_DEVICE);
                        if !this.devices.iter().any(|d| d.id == id) {
                            this.devices.push(Device {
                                id: id.clone(),
                                name: device.name.clone(),
                                kind: device_kind(&device.kind),
                                volume: device.supports_volume,
                            });
                        }
                        this.active_device = Some(id);
                        cx.emit(PlayerEvent::StateChanged);
                        cx.notify();
                        true
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
        })
        .detach();
    }

    /// Show what Soloist reports, as this computer's playback (`here`) or
    /// another device's.
    fn show_soloist_state(
        &mut self,
        state: crate::soloist::ws::State,
        here: bool,
        cx: &mut Context<Self>,
    ) {
        self.playing = state.status == "playing";
        self.loading = state.status == "buffering";
        self.connect_shuffle = state.shuffle;
        self.repeat = match state.repeat.as_str() {
            "track" => Repeat::Track,
            "context" => Repeat::Context,
            _ => Repeat::Off,
        };
        if let Some(context) = state.context_uri.filter(|c| !c.is_empty()) {
            self.connect_context = Some(context);
        }
        self.anchor(state.position.at(unix_ms()) as u32);
        let track: Option<Track> = state.track.and_then(|t| serde_json::from_value(t).ok());
        match track {
            Some(track) if self.current.as_ref().is_none_or(|c| c.uri != track.uri) => {
                // Shown at once; the Web API's copy follows, with names in
                // the user's language (Soloist's are untranslated).
                let (uri, duration) = (track.uri.to_string(), track.duration_ms);
                if here && self.playing {
                    crate::history::record(&track);
                }
                self.current = Some(track);
                cx.emit(PlayerEvent::TrackChanged);
                self.look_up_current(uri, duration, cx);
                self.refresh_queue(cx);
            }
            _ => cx.emit(PlayerEvent::StateChanged),
        }
    }

    pub fn on_soloist_position(
        &mut self,
        position: crate::soloist::ws::Position,
        cx: &mut Context<Self>,
    ) {
        if !self.soloist_active(cx) && !self.mirrors_through_soloist(cx) {
            return;
        }
        self.anchor(position.at(unix_ms()) as u32);
        cx.emit(PlayerEvent::Seeked);
        cx.notify();
    }

    /// Soloist's queue: what plays after the current track.
    pub fn on_soloist_queue(&mut self, upcoming: Vec<Value>, cx: &mut Context<Self>) {
        if !self.soloist_active(cx) && !self.mirrors_through_soloist(cx) {
            return;
        }
        let upcoming: Vec<Track> = upcoming
            .into_iter()
            .filter_map(|t| serde_json::from_value(t).ok())
            .collect();
        // Recommendations (autoplay) come as bare URIs: shown once named.
        let unnamed: Vec<String> = upcoming
            .iter()
            .filter(|t| t.name.is_empty())
            .map(|t| t.uri.to_string())
            .collect();
        let named = upcoming.iter().filter(|t| !t.name.is_empty()).cloned();
        self.queue = Queue::mirror(self.current.clone(), named.collect());
        cx.notify();
        if unnamed.is_empty() {
            self.queue_refresh = None;
            return;
        }
        self.queue_refresh = Some(cx.spawn(async move |this, cx| {
            let found =
                runtime::run(
                    async move { crate::spotify::streaming::tracks_by_uri(&unnamed).await },
                )
                .await
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                let upcoming = upcoming
                    .into_iter()
                    .filter_map(|t| {
                        if !t.name.is_empty() {
                            return Some(t);
                        }
                        found.iter().find(|f| f.uri == t.uri).cloned()
                    })
                    .collect();
                this.queue = Queue::mirror(this.current.clone(), upcoming);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Soloist is the device playing.
    fn soloist_active(&self, cx: &App) -> bool {
        self.active_device
            .as_deref()
            .is_some_and(|d| app::soloist(cx).read(cx).is_device(d))
    }

    /// Send a transport command straight to Soloist: no round trip
    /// through Spotify's Web API. Soloist passes it on to whichever device
    /// plays, so on Soloist it drives other devices too; volume excepted,
    /// which stays Soloist's own. False when Soloist cannot take it.
    fn soloist_command(&self, name: &str, fields: Value, cx: &App) -> bool {
        let Some(active) = self.active_device.as_deref() else {
            return false;
        };
        let soloist = app::soloist(cx).read(cx);
        let reaches =
            soloist.is_device(active) || (name != "set_volume" && self.plays_on_soloist(cx));
        reaches && soloist.command(name, fields)
    }

    /// Load `uri` again in the context it failed in (librespot has moved on
    /// to the next track by now).
    fn play_again(&mut self, uri: String, cx: &mut Context<Self>) {
        self.retry = None;
        if self.remote_device().is_some() {
            return;
        }
        let load = match &self.connect_context {
            Some(context) => PendingLoad {
                context: Some(context.clone()),
                tracks: Vec::new(),
                start: Start::Uri(uri),
            },
            None => match self.started_tracks.iter().position(|t| *t == uri) {
                Some(ix) => PendingLoad {
                    context: None,
                    tracks: self.started_tracks.clone(),
                    start: Start::Index(ix as u32),
                },
                None => PendingLoad {
                    context: None,
                    tracks: vec![uri],
                    start: Start::Index(0),
                },
            },
        };
        self.loading = true;
        self.load(load, cx);
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// A new track is current: the speaker is playing it (or will next).
    fn track_changed(&mut self, audio_item: &AudioItem, cx: &mut Context<Self>) {
        self.track_switch = None;
        // It loaded after all; a later failure of it starts counting anew.
        if self
            .retried
            .as_ref()
            .is_some_and(|(uri, _)| *uri == audio_item.uri)
        {
            self.retried = None;
        }
        // Prefer what the Web API told us about this track (it has album and
        // artist ids) over librespot's thinner metadata.
        let known = self
            .current
            .iter()
            .chain(self.queue.upcoming().take(5))
            .find(|t| t.uri.as_ref() == audio_item.uri)
            .cloned();
        self.current = Some(known.unwrap_or_else(|| track_from_audio_item(audio_item)));
        if let Some(track) = &self.current {
            crate::history::record(track);
        }
        self.anchor(0);
        self.save_session(cx);
        // Spotify does not push our own changes; read what comes next.
        self.refresh_queue(cx);
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    /// Spotify's view of all devices, pushed on every change.
    fn on_cluster(&mut self, update: Cluster, cx: &mut Context<Self>) {
        // Another device took over. (None is not that: a paused session,
        // like a restored one, is listed with no active device.)
        if update
            .active_device
            .as_deref()
            .is_some_and(|id| Some(id) != self.device_id())
        {
            self.spirc_active = false;
        }
        self.devices = update.devices;
        self.active_device = update.active_device;
        let Some(playback) = update.playback else {
            cx.notify();
            return;
        };

        if self.remote_device().is_some() {
            // Another device plays: mirror it.
            self.playing = playback.playing;
            self.loading = false;
            self.connect_shuffle = playback.shuffle;
            self.repeat = match (playback.repeat_context, playback.repeat_track) {
                (_, true) => Repeat::Track,
                (true, false) => Repeat::Context,
                (false, false) => Repeat::Off,
            };
            self.anchor(playback.position_ms);
            self.connect_context = Some(playback.context_uri.clone()).filter(|c| !c.is_empty());
            let known = self
                .current
                .as_ref()
                .is_some_and(|t| t.uri.as_ref() == playback.track_uri);
            if !known {
                self.look_up_current(playback.track_uri.clone(), playback.duration_ms, cx);
            }
            cx.emit(PlayerEvent::StateChanged);
        }
        if playback.next != self.mirrored_next {
            self.mirrored_next = playback.next;
            self.refresh_queue(cx);
        }
        cx.notify();
    }

    /// This device is the one playing.
    fn mark_active(&mut self) {
        self.spirc_active = true;
        let Some(id) = self.device_id().map(str::to_string) else {
            return;
        };
        self.active_device = Some(id.clone());
        if !self.devices.iter().any(|d| d.id == id) {
            self.devices.push(Device {
                id,
                name: "Lilac".into(),
                kind: DeviceKind::Computer,
                volume: true,
            });
        }
    }

    /// Read the device list once. Later changes by other devices are pushed
    /// through the cluster, but Spotify never pushes the state we caused,
    /// and nothing at all until some other device changes.
    pub fn refresh_devices(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if self.backend.is_none() && self.reader.is_none() {
            return;
        }
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(list) = runtime::run(async move { api.remote_devices().await }).await else {
                return;
            };
            this.update(cx, |this, cx| {
                let mut devices: Vec<Device> = list
                    .devices
                    .iter()
                    .filter(|d| !d.is_restricted)
                    .filter_map(|d| {
                        Some(Device {
                            id: d.id.clone()?,
                            name: d.name.clone(),
                            kind: device_kind(&d.kind),
                            volume: d.supports_volume,
                        })
                    })
                    .collect();
                devices.sort_by(|a, b| a.name.cmp(&b.name));
                this.devices = devices;
                this.active_device = list
                    .devices
                    .iter()
                    .find(|d| d.is_active)
                    .and_then(|d| d.id.clone())
                    .or(this.active_device.take());
                if this.spirc_active {
                    this.mark_active();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn anchor(&mut self, position_ms: u32) {
        self.anchor_ms = position_ms;
        self.anchor_at = Instant::now();
        self.anchor_heard_ms = self.heard().map(|out| out.played_ms()).unwrap_or(0);
    }

    /// Anchor at a position librespot reported. librespot counts what it
    /// has decoded; the speaker is behind it by what is still queued.
    fn anchor_reported(&mut self, position_ms: u32) {
        let queued = self.heard().map(|out| out.queued_ms()).unwrap_or(0);
        self.anchor(position_ms.saturating_sub(queued));
    }

    /// The clock of the audio this device plays, while it plays here.
    fn heard(&self) -> Option<&OutputControl> {
        if self.remote_device().is_some() {
            return None;
        }
        self.backend.as_ref()?.output()
    }

    /// The speaker started or stopped: the only thing that shows this device
    /// as playing or paused.
    fn on_cue(&mut self, cue: Cue, cx: &mut Context<Self>) {
        if self.heard().is_none() {
            return;
        }
        match cue {
            Cue::Started => {
                self.playing = true;
                self.loading = false;
            }
            Cue::Stopped => self.playing = false,
        }
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// Fill in the details of a track another device started.
    fn look_up_current(&mut self, uri: String, duration_ms: u32, cx: &mut Context<Self>) {
        let api = app::api(cx);
        self.lookup = Some(cx.spawn(async move |this, cx| {
            let lookup_uri = uri.clone();
            // The streaming session (in the user's language); the Web API
            // only when that fails.
            let result = runtime::run(async move {
                match crate::spotify::streaming::track(&lookup_uri).await {
                    Ok(track) => Ok(track),
                    Err(_) => api.item(&lookup_uri).await,
                }
            })
            .await;
            this.update(cx, |this, cx| {
                let shown = this.current.as_ref().is_some_and(|t| t.uri.as_ref() == uri);
                if result.is_err() && shown {
                    // What is shown already (from Soloist) beats a blank.
                    return;
                }
                let mut track = result.unwrap_or_else(|_| Track {
                    uri: uri.clone().into(),
                    id: uri
                        .rsplit(':')
                        .next()
                        .unwrap_or_default()
                        .to_string()
                        .into(),
                    ..Track::default()
                });
                if track.duration_ms == 0 {
                    track.duration_ms = duration_ms;
                }
                if this.soloist_active(cx) && this.playing {
                    crate::history::record(&track);
                }
                this.current = Some(track);
                cx.emit(PlayerEvent::TrackChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Rebuild the mirrored "Next up" list from Spotify's queue.
    fn refresh_queue(&mut self, cx: &mut Context<Self>) {
        // Soloist answers with a queue_changed event (`on_soloist_queue`).
        if self.soloist_command("get_queue", json!({ "limit": QUEUE_SHOWN }), cx) {
            return;
        }
        let api = app::api(cx);
        self.queue_refresh = Some(cx.spawn(async move |this, cx| {
            // The Web API learns about a track change seconds after the
            // player does; until then it reports the previous track's queue.
            for attempt in 0..6 {
                if attempt > 0 {
                    cx.background_executor()
                        .timer(Duration::from_millis(800))
                        .await;
                }
                let api = api.clone();
                let Ok(remote) = runtime::run(async move { api.remote_queue().await }).await else {
                    return;
                };
                let done = this
                    .update(cx, |this, cx| {
                        let expected = this.current.as_ref().map(|t| t.uri.clone());
                        let reported = remote.currently_playing.as_ref().map(|t| t.uri.clone());
                        if expected.is_some() && reported != expected && attempt < 5 {
                            return false;
                        }
                        this.apply_remote_queue(remote, cx);
                        true
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
        }));
    }

    fn apply_remote_queue(&mut self, remote: crate::model::RemoteQueue, cx: &mut Context<Self>) {
        // librespot's metadata has no album or artist ids; the Web API's has,
        // which the title and artist links and the Now Playing panel need.
        if let (Some(current), Some(full)) = (&self.current, &remote.currently_playing)
            && current.uri == full.uri
            && current.album.as_ref().is_none_or(|a| a.id.is_empty())
        {
            self.current = Some(full.clone());
            cx.emit(PlayerEvent::TrackChanged);
        }
        self.queue = Queue::mirror(remote.currently_playing, remote.queue);
        cx.notify();
    }
}

fn device_kind(kind: &str) -> DeviceKind {
    match kind.to_ascii_lowercase().as_str() {
        "computer" => DeviceKind::Computer,
        "smartphone" | "tablet" => DeviceKind::Phone,
        "speaker" | "avr" | "stb" | "audiodongle" => DeviceKind::Speaker,
        "tv" | "castvideo" => DeviceKind::Tv,
        _ => DeviceKind::Other,
    }
}

/// What Spotify says plays where (or why it could not say), and the last
/// played track when that was asked.
type RestoreLookup = (
    Result<Option<crate::model::RemoteState>, String>,
    Option<crate::model::PlayedItem>,
);

/// Queue entries asked of Soloist for "Next up".
const QUEUE_SHOWN: u32 = 50;

/// Run a Web API remote-control call, surfacing failures as a toast.
fn remote<F>(cx: &mut Context<Player>, call: F)
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    cx.spawn(async move |_, cx| {
        if let Err(err) = runtime::run(call).await {
            cx.update(|cx| app::toast_error(format!("{err:#}"), cx));
        }
    })
    .detach();
}

/// Our track model from what librespot knows about the item it loaded.
fn track_from_audio_item(item: &AudioItem) -> Track {
    let id = item.uri.rsplit(':').next().unwrap_or_default().to_string();
    let mut covers: Vec<_> = item.covers.iter().collect();
    covers.sort_by_key(|c| c.width);
    let images = Images {
        small: covers.first().map(|c| c.url.clone().into()),
        medium: covers.get(covers.len() / 2).map(|c| c.url.clone().into()),
        large: covers.last().map(|c| c.url.clone().into()),
    };
    let mut track = Track {
        id: id.into(),
        uri: item.uri.clone().into(),
        name: item.name.clone().into(),
        duration_ms: item.duration_ms,
        explicit: item.is_explicit,
        is_playable: Some(true),
        ..Track::default()
    };
    match &item.unique_fields {
        UniqueFields::Track {
            artists,
            album,
            number,
            disc_number,
            ..
        } => {
            track.artists = artists
                .0
                .iter()
                .map(|a| ArtistRef {
                    id: a
                        .id
                        .to_uri()
                        .ok()
                        .and_then(|uri| uri.rsplit(':').next().map(str::to_string))
                        .unwrap_or_default()
                        .into(),
                    name: a.name.clone().into(),
                })
                .collect();
            track.album = Some(AlbumRef {
                name: album.clone().into(),
                images: images.clone(),
                ..AlbumRef::default()
            });
            track.track_number = *number;
            track.disc_number = *disc_number;
        }
        UniqueFields::Episode { show_name, .. } => {
            track.kind = ItemKind::Episode;
            track.show = Some(ShowRef {
                name: show_name.clone().into(),
                images: images.clone(),
                ..ShowRef::default()
            });
            track.images = images;
        }
        UniqueFields::Local { .. } => track.is_local = true,
    }
    track
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The context to hand Spotify for another device. Its Web API refuses a
/// starting track within an artist ("Can't have offset for context type:
/// ARTIST"), so artist pages send their track list instead.
fn remote_context(context: Option<String>) -> Option<String> {
    context.filter(|c| !c.starts_with("spotify:artist:"))
}

/// Start playback through Soloist's local socket, without Spotify's Web
/// API. Soloist plays a URI from its start, so a context is played as a
/// whole when it starts at its first track; otherwise the chosen track
/// plays and the ones after it are queued.
fn play_locally(context: Option<&str>, tracks: &[String], offset: Option<&str>, cx: &mut App) {
    let soloist = app::soloist(cx).read(cx);
    let from_start = offset.is_none() || tracks.first().map(String::as_str) == offset;
    let sent = match context {
        Some(context) if from_start => soloist.command("play", json!({ "uri": context })),
        _ => {
            let start = offset
                .map(str::to_string)
                .or_else(|| tracks.first().cloned());
            let Some(start) = start else {
                return;
            };
            let after = tracks.iter().skip_while(|t| **t != start).skip(1).take(30);
            let sent = soloist.command("play", json!({ "uri": start }));
            for track in after {
                soloist.command("add_to_queue", json!({ "uri": track }));
            }
            sent
        }
    };
    if !sent {
        app::toast_error("Soloist isn't ready to play yet; try again in a moment", cx);
    }
}
