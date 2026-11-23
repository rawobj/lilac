//! Playback state: what is playing, where in the track we are, and what comes next.
//!
//! With a Premium account, Soloist (Spotify's own player) plays on this
//! computer: Lilac is one Connect device among the user's others, Spotify
//! decides what plays next, and this entity mirrors that state for the UI.
//! When another device is active, the same controls drive that device
//! instead. A free account plays each track's 30-second preview here, and
//! demo mode has nothing to stream; both keep a local queue and a clock.

mod preview;
pub mod queue;
mod resume;

use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::app;
use crate::model::{ContextUri, Track};
use crate::runtime;
use crate::session::{Session, SessionEvent};
use crate::settings::Settings;
use serde_json::{Value, json};

pub use queue::{Queue, Repeat};
use resume::LastSession;

/// A device in the user's Spotify Connect list.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
    /// Its volume can be set from here.
    pub volume: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Computer,
    Phone,
    Speaker,
    Tv,
    Other,
}

/// Restart the current track instead of going to the previous one past this point.
/// Stands in for the device playing until Spotify names it (see
/// `Player::name_other_device`).
pub const OTHER_DEVICE: &str = "lilac-other-device";
const RESTART_THRESHOLD_MS: u32 = 3000;
/// A burst of seeks or volume steps would queue up round trips to the
/// device. Bursts are collapsed: the first change goes out at once, the
/// last one after this much quiet.
const COMMAND_SETTLE: Duration = Duration::from_millis(400);

/// How far the player has come since login; the loading screen shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Warmup {
    /// Not logged in yet.
    Idle,
    /// Logging in to Spotify's streaming servers.
    Connecting,
    /// Finding out what to bring back: what the account's devices play.
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

pub struct Player {
    /// The local queue for previews and demo mode; with Connect, a mirror
    /// of what Spotify says comes next.
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
    /// extrapolated from it by the clock.
    anchor_ms: u32,
    anchor_at: Instant,
    ticker: Option<Task<()>>,
    /// A free account's preview: the sound output, the clip playing, the
    /// fetch of the next one, and the clip's length (0 when not previewing).
    output: Option<preview::Output>,
    clip: Option<preview::Clip>,
    /// The current track's clip, kept while paused so Play needs no fetch.
    clip_bytes: Option<(String, Vec<u8>)>,
    clip_fetch: Option<Task<()>>,
    clip_ms: u32,
    /// The device Lilac last moved playback to, and when: Soloist reports
    /// that it lost the session, not to whom.
    moving_to: Option<(String, Instant)>,
    /// When Lilac last started something on Soloist; a pause Soloist
    /// reports right after that was not asked for.
    soloist_started_at: Option<Instant>,
    /// The streaming session Lilac reads through (artists, lyrics,
    /// previews); Soloist does the playing.
    reader: Option<librespot_core::session::Session>,
    reader_starting: Option<Task<()>>,
    /// Playback follows Spotify Connect (Soloist and other devices), not
    /// the local queue: a Premium account, logged in.
    connect: bool,
    /// Context of Connect playback; `queue.context` for previews and demo.
    connect_context: Option<String>,
    connect_shuffle: bool,
    /// The user's Liked Songs as a Connect context.
    liked_uri: Option<String>,
    /// Upcoming URIs the mirrored queue was built from.
    mirrored_next: Vec<String>,
    lookup: Option<Task<()>>,
    /// Announces the volume once a gesture settles.
    volume_sync: Option<Task<()>>,
    /// A seek waiting for the previous one to settle, and the timer for it.
    pending_seek: Option<u32>,
    seek_settle: Option<Task<()>>,
    pub warmup: Warmup,
    restore: Option<Task<()>>,
    /// Where to go once Soloist plays this track (it starts at 0).
    seek_when_playing: Option<(String, u32)>,
    /// The track of the paused session Spotify still holds, as Soloist
    /// reports it when it starts. Play on that track takes the session
    /// over: its list, queue and shuffle as they were.
    soloist_held: Option<String>,
    restore_lookup: Option<tokio::task::JoinHandle<RestoreLookup>>,
    queue_refresh: Option<Task<()>>,
    /// Track URIs of the list we started when it has no context URI, in
    /// order: kept with the session and used to load a track again.
    started_tracks: Vec<String>,
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
                // The account's type may be known only now.
                this.connect = !session.read(cx).is_free();
                this.ensure_reader(cx);
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
                // The reading session is the old account's too; the next
                // login starts its own and restores from there.
                this.reader = None;
                this.reader_starting = None;
                this.connect = false;
                this.restore = None;
                this.restore_lookup = None;
                this.output = None;
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
            ticker: None,
            output: None,
            clip: None,
            clip_bytes: None,
            clip_fetch: None,
            clip_ms: 0,
            moving_to: None,
            soloist_started_at: None,
            reader: None,
            reader_starting: None,
            connect: false,
            connect_context: None,
            connect_shuffle: false,
            liked_uri: None,
            mirrored_next: Vec::new(),
            lookup: None,
            volume_sync: None,
            pending_seek: None,
            seek_settle: None,
            warmup: Warmup::Idle,
            restore: None,
            seek_when_playing: None,
            soloist_held: None,
            restore_lookup: None,
            queue_refresh: None,
            started_tracks: Vec::new(),
        }
    }

    // ------------------------------------------------------------------ queries

    /// The track's length, or the preview's while one plays.
    pub fn duration_ms(&self) -> u32 {
        if self.clip_ms > 0 {
            return self.clip_ms;
        }
        self.current.as_ref().map(|t| t.duration_ms).unwrap_or(0)
    }

    /// A free account plays 30-second previews.
    pub fn previewing(&self) -> bool {
        self.clip_ms > 0
    }

    pub fn position_ms(&self) -> u32 {
        let position = if self.playing && !self.loading {
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

    /// The device playing, when Lilac's controls go to a device (Soloist
    /// counts, once it is the one playing).
    pub fn remote_device(&self) -> Option<&Device> {
        let active = self.active_device.as_deref()?;
        self.devices.iter().find(|d| d.id == active)
    }

    /// The device playing elsewhere when its volume cannot be set from
    /// here (Spotify refuses it for many phones).
    pub fn volume_elsewhere(&self) -> Option<&Device> {
        self.remote_device().filter(|d| !d.volume)
    }

    /// Queued tracks can be removed or reordered. Spotify Connect offers no
    /// way to do that, so only the local queue can.
    pub fn queue_editable(&self) -> bool {
        !self.connected()
    }

    fn connected(&self) -> bool {
        self.connect
    }

    /// Soloist is not set up yet: playing here can only fail, so it is not
    /// tried. Play buttons open the setup instead. Another device playing
    /// is unaffected.
    pub fn locked(&self, cx: &App) -> bool {
        if self.simulated(cx) || self.remote_device().is_some() {
            return false;
        }
        !app::soloist(cx).read(cx).usable()
    }

    /// Soloist was set up: playing here works now.
    pub fn soloist_paired(&mut self, cx: &mut Context<Self>) {
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// Connect the streaming session Lilac reads through, then bring back
    /// the last session.
    fn ensure_reader(&mut self, cx: &mut Context<Self>) {
        if self.reader.is_some() || self.reader_starting.is_some() {
            return;
        }
        let free = app::session(cx).read(cx).is_free();
        self.connect = !free;
        if !free {
            // Up before the loading screen goes, so this computer can be
            // picked in the device list even while another device plays.
            app::soloist(cx).update(cx, |s, cx| s.start(cx));
            // Lilac's own record (`LastSession`, written from Soloist's
            // state) says where the user left off here; what other devices
            // play is only Spotify's to say, which costs this one request.
            let api = app::api(cx);
            self.restore_lookup = Some(runtime::spawn(async move {
                api.remote_state().await.map_err(|e| format!("{e:#}"))
            }));
        }
        let auth = app::session(cx).read(cx).streaming.clone();
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
                if app::session(cx).read(cx).is_free() {
                    this.warmup = Warmup::Ready;
                } else {
                    this.restore_session(cx);
                }
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
            // A preview is the chosen track alone.
            let (tracks, index) = match tracks.get(index) {
                Some(track) if self.previews(cx) => (vec![track.clone()], 0),
                _ => (tracks, index),
            };
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
        let offset = Some(first.uri.to_string());

        // Show the choice at once, loading; whether it plays is up to
        // Spotify.
        self.current = Some(first);
        self.connect_context = context_uri.clone();
        self.anchor(0);
        self.playing = true;
        self.loading = true;
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();

        // Another device playing is driven where it is; otherwise Soloist
        // plays (it counts as a device itself once active).
        let elsewhere = self.remote_device().is_some() && !self.soloist_active(cx);
        if elsewhere {
            let api = app::api(cx);
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
        self.play_on_soloist(context_uri, uris, offset, 0, cx);
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
            self.ensure_reader(cx);
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

    /// Show what to pick up: what another device plays, or the paused
    /// session Spotify holds (Lilac's own record is on screen already).
    /// Nothing is taken over before Play.
    fn restore_session(&mut self, cx: &mut Context<Self>) {
        self.warmup = Warmup::Restoring;
        cx.notify();
        let lookup = self.restore_lookup.take();
        self.restore = Some(cx.spawn(async move |this, cx| {
            let state = match lookup {
                Some(lookup) => lookup.await.unwrap_or(Ok(None)),
                None => Ok(None),
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
            this.update(cx, |this, cx| {
                this.start_restore(state, cx);
                this.restore = None;
                this.warmup = Warmup::Ready;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Decide what to show, in order of preference: what another device
    /// is playing (shown, not taken over), a session Spotify still holds,
    /// the session Lilac saved itself (on screen already).
    fn start_restore(&mut self, state: Option<crate::model::RemoteState>, cx: &mut Context<Self>) {
        let Some(state) = state.filter(|s| s.item.is_some()) else {
            return;
        };
        // Lilac's own Soloist from the last run (ended with Lilac) may still
        // be listed as playing; this one has only just started, so that is
        // a paused session to pick up, not another device playing.
        let ours = state.device.as_ref().is_some_and(|d| {
            d.name == crate::soloist::DEVICE_NAME
                || d.id
                    .as_deref()
                    .is_some_and(|id| app::soloist(cx).read(cx).is_device(id))
        });
        if state.is_playing && !ours {
            // Show what the other device plays; the controls drive it. It
            // must be in the list for that (which is read only when the
            // Devices panel opens).
            let device = state.device.as_ref().and_then(|d| d.id.clone());
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
            return;
        }
        // A paused session another device holds: Play starts it on Soloist
        // (or opens its setup).
        self.current = state.item;
        self.playing = false;
        self.connect_context = state.context.map(|c| c.uri).filter(|u| !u.is_empty());
        self.anchor(state.progress_ms.unwrap_or(0));
        cx.emit(PlayerEvent::TrackChanged);
    }

    /// Remember what plays and where, for the next start.
    fn save_session(&self, cx: &App) {
        let Some(track) = &self.current else {
            return;
        };
        // Only what this computer plays (Soloist), not other devices.
        if !self.soloist_active(cx) {
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
        if self.playing {
            self.pause(cx);
        } else {
            self.resume(cx);
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        // A pause the user asks for is meant.
        self.soloist_started_at = None;
        if !self.playing {
            return;
        }
        self.anchor_ms = self.position_ms();
        self.anchor_at = Instant::now();
        self.playing = false;
        self.ticker = None;
        if self.clip.is_some() {
            // The sound output goes too: open, it keeps waking the CPU to
            // play silence. Play opens it again.
            self.clip = None;
            self.output = None;
        } else if self.remote_device().is_some() && !self.soloist_command("pause", Value::Null, cx)
        {
            let api = app::api(cx);
            remote(cx, async move { api.remote_pause().await });
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
            let position = self.position_ms();
            if self.take_over_held(&uri, position, cx) {
                return;
            }
            // Nothing plays yet: start where we were, on Soloist.
            let context = remote_context(self.connect_context.clone());
            // A list Lilac did not start (Spotify's last state) is known only
            // by its URI: no tracks, so the start is not taken for its top.
            let tracks = if self.started_tracks.contains(&uri) {
                self.started_tracks.clone()
            } else if context.is_some() {
                Vec::new()
            } else {
                vec![uri.clone()]
            };
            self.play_on_soloist(context, tracks, Some(uri), position, cx);
            return;
        }
        if self.previews(cx) {
            let Some(track) = self.current.clone() else {
                return;
            };
            let from = if self.position_ms() + 500 >= self.duration_ms() {
                0
            } else {
                self.position_ms()
            };
            match self.clip_bytes.clone() {
                Some((uri, bytes)) if uri == track.uri.as_ref() => {
                    self.playing = true;
                    self.play_clip(bytes, from, cx);
                }
                _ => self.start_preview(track, from, cx),
            }
            return;
        }
        self.anchor_at = Instant::now();
        self.playing = true;
        if self.simulated(cx) {
            self.start_ticker(cx);
        } else if self.remote_device().is_some() && !self.soloist_command("play", Value::Null, cx) {
            let api = app::api(cx);
            remote(cx, async move {
                api.remote_play(None, None, None, None, 0).await
            });
        }
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.end_preview();
        self.queue = Queue::default();
        self.current = None;
        self.playing = false;
        self.loading = false;
        self.anchor(0);
        self.ticker = None;
        self.connect_context = None;
        self.mirrored_next.clear();
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        if self.previews(cx) || self.setup_instead(cx) {
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
        } else if self.remote_device().is_some()
            && !self.soloist_command("skip_next", Value::Null, cx)
        {
            let api = app::api(cx);
            remote(cx, async move { api.remote_next().await });
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if self.previews(cx) {
            self.seek(0, cx);
            return;
        }
        if self.setup_instead(cx) {
            return;
        }
        if !self.simulated(cx) {
            // Spotify restarts the track or goes back itself, by the same rule.
            if self.remote_device().is_some() && !self.soloist_command("skip_prev", Value::Null, cx)
            {
                let api = app::api(cx);
                remote(cx, async move { api.remote_previous().await });
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
        self.anchor(position_ms);
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
        if let Some(clip) = &self.clip {
            clip.seek(position_ms);
        } else if self.remote_device().is_some()
            && !self.soloist_command("seek", json!({ "position_ms": position_ms }), cx)
        {
            let api = app::api(cx);
            remote(cx, async move { api.remote_seek(position_ms).await });
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
        if self.previews(cx) {
            return;
        }
        let on = !self.shuffle();
        if self.simulated(cx) {
            self.queue.set_shuffle(on);
        } else {
            self.connect_shuffle = on;
            if self.remote_device().is_some()
                && !self.soloist_command("set_shuffle", json!({ "enabled": on }), cx)
            {
                let api = app::api(cx);
                remote(cx, async move { api.remote_shuffle(on).await });
            }
        }
        cx.notify();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        if self.previews(cx) {
            return;
        }
        self.repeat = self.repeat.next();
        let repeat = self.repeat;
        if self.simulated(cx) {
            cx.notify();
            return;
        }
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
        if self.previews(cx) && self.current.is_some() {
            app::toast("Preview mode plays one song at a time", cx);
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
        if app::soloist(cx).read(cx).is_device(device_id) {
            // Soloist takes the session over itself, where it was.
            if !app::soloist(cx).read(cx).command("activate", Value::Null) {
                app::toast_error("Soloist isn't ready yet; try again in a moment", cx);
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

    // ------------------------------------------- previews and demo playback

    /// The local queue and a clock run playback: demo mode, and a free
    /// account's previews.
    fn simulated(&self, cx: &App) -> bool {
        let session = app::session(cx).read(cx);
        session.is_demo() || session.is_free()
    }

    /// A free account: one track at a time, as its preview clip. No
    /// queue, no skipping, no other devices.
    pub fn previews(&self, cx: &App) -> bool {
        app::session(cx).read(cx).is_free()
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
        if self.previews(cx) {
            self.start_preview(track, 0, cx);
            return;
        }
        self.current = Some(track);
        self.anchor_ms = 0;
        self.anchor_at = Instant::now();
        self.playing = true;
        self.start_ticker(cx);
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    /// Fetch `track`'s preview and play it from `from_ms`; shown loading
    /// meanwhile.
    fn start_preview(&mut self, track: Track, from_ms: u32, cx: &mut Context<Self>) {
        self.end_preview();
        // A place past the clip (a whole song's, restored) starts it over.
        let from_ms = if from_ms + 500 >= preview::CLIP_MS {
            0
        } else {
            from_ms
        };
        let uri = track.uri.to_string();
        let changed = self.current.as_ref().is_none_or(|t| t.uri != track.uri);
        self.current = Some(track);
        self.clip_ms = preview::CLIP_MS;
        self.anchor(from_ms);
        self.playing = true;
        self.loading = true;
        if changed {
            cx.emit(PlayerEvent::TrackChanged);
        } else {
            cx.emit(PlayerEvent::StateChanged);
        }
        cx.notify();
        self.clip_fetch = Some(cx.spawn(async move |this, cx| {
            let fetch_uri = uri.clone();
            let clip =
                runtime::run(
                    async move { crate::spotify::streaming::preview_clip(&fetch_uri).await },
                )
                .await;
            this.update(cx, |this, cx| this.preview_fetched(uri, clip, from_ms, cx))
                .ok();
        }));
    }

    fn preview_fetched(
        &mut self,
        uri: String,
        clip: anyhow::Result<Option<Vec<u8>>>,
        from_ms: u32,
        cx: &mut Context<Self>,
    ) {
        self.clip_fetch = None;
        if self.current.as_ref().is_none_or(|t| t.uri.as_ref() != uri) || !self.playing {
            return;
        }
        let name = self
            .current
            .as_ref()
            .map(|t| format!("\u{201c}{}\u{201d}", t.name))
            .unwrap_or_default();
        let bytes = match clip {
            Ok(Some(bytes)) => {
                self.clip_bytes = Some((uri, bytes.clone()));
                bytes
            }
            Ok(None) => {
                app::toast(format!("Spotify has no preview of {name}"), cx);
                self.finish(cx);
                return;
            }
            Err(err) => {
                app::toast_error(format!("Couldn't play the preview of {name}: {err:#}"), cx);
                self.finish(cx);
                return;
            }
        };
        self.play_clip(bytes, from_ms, cx);
    }

    /// Play the current track's clip from `from_ms`.
    fn play_clip(&mut self, bytes: Vec<u8>, from_ms: u32, cx: &mut Context<Self>) {
        if self.output.is_none() {
            match preview::Output::open() {
                Ok(output) => self.output = Some(output),
                Err(err) => {
                    app::toast_error(format!("{err:#}"), cx);
                    self.finish(cx);
                    return;
                }
            }
        }
        let output = self.output.as_ref().expect("opened above");
        match output.play(bytes, from_ms, self.effective_volume()) {
            Ok((clip, length)) => {
                let fresh = self.clip_ms == 0 || from_ms == 0;
                self.clip = Some(clip);
                self.clip_ms = length;
                self.loading = false;
                self.anchor(from_ms);
                if fresh && let Some(track) = &self.current {
                    crate::history::record(track);
                }
                self.start_ticker(cx);
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            Err(err) => {
                app::toast_error(format!("{err:#}"), cx);
                self.finish(cx);
            }
        }
    }

    /// Stop the preview playing, if any, and forget it.
    fn end_preview(&mut self) {
        self.clip = None;
        self.clip_fetch = None;
        self.clip_bytes = None;
        self.clip_ms = 0;
    }

    /// The current track played to its end.
    fn track_ended(&mut self, cx: &mut Context<Self>) {
        if self.previews(cx) {
            // A preview plays once; nothing follows it.
            self.finish(cx);
            return;
        }
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
        // The clip and the sound output are let go; Play starts it again.
        self.clip = None;
        self.clip_fetch = None;
        self.output = None;
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

    fn apply_volume(&mut self, cx: &mut Context<Self>) {
        if let Some(clip) = &self.clip {
            clip.set_volume(self.effective_volume());
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
        let position = self.position_ms();
        self.play_on_soloist(context, tracks, Some(uri), position, cx);
    }

    /// Soloist plays for this computer and is not logged in yet; the
    /// loading screen waits for it.
    pub fn waits_for_soloist(&self, cx: &App) -> bool {
        self.plays_on_soloist(cx) && app::soloist(cx).read(cx).coming_up()
    }

    /// Whether this computer can play (through Soloist).
    fn plays_on_soloist(&self, cx: &App) -> bool {
        !self.simulated(cx) && app::soloist(cx).read(cx).usable()
    }

    /// Play `uri` by taking over the paused session Spotify holds for it:
    /// Soloist takes it over paused and silent, so the seek is not heard,
    /// and Spotify's copy of the position is where it was last paused,
    /// not ours. False when Spotify holds no session for this track.
    fn take_over_held(&mut self, uri: &str, position_ms: u32, cx: &mut Context<Self>) -> bool {
        if self.soloist_held.as_deref() != Some(uri) {
            return false;
        }
        let soloist = app::soloist(cx).read(cx);
        let Some(device_id) = soloist.device_id.clone() else {
            return false;
        };
        let sent = soloist.command("activate", Value::Null)
            && soloist.command("seek", json!({ "position_ms": position_ms }))
            && soloist.command("play", Value::Null);
        if !sent {
            return false;
        }
        self.soloist_plays(device_id, cx);
        self.soloist_held = None;
        self.soloist_started_at = Some(Instant::now());
        self.loading = true;
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
        true
    }

    /// Soloist is the device playing from here on; the controls follow it.
    fn soloist_plays(&mut self, device_id: String, cx: &mut Context<Self>) {
        if !self.devices.iter().any(|d| d.id == device_id) {
            self.devices.push(Device {
                id: device_id.clone(),
                name: crate::soloist::DEVICE_NAME.into(),
                kind: DeviceKind::Computer,
                volume: true,
            });
        }
        self.active_device = Some(device_id);
        cx.notify();
    }

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
        self.loading = true;
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
        let api = app::api(cx);
        let player = cx.entity().downgrade();
        app::soloist(cx).update(cx, |soloist, cx| {
            soloist.when_ready(
                move |device_id, cx| {
                    if let Some(player) = player.upgrade() {
                        player.update(cx, |p, cx| p.soloist_plays(device_id.clone(), cx));
                    }
                    // Soloist is told directly: always for a list from its
                    // start (it plays the album or playlist itself), and for
                    // a start further in whenever Spotify cannot be asked.
                    // Only Spotify can start a list part-way; Soloist alone
                    // plays that track and queues the rest.
                    let from_top = context.is_some()
                        && offset.as_ref().is_none_or(|o| tracks.first() == Some(o));
                    // Soloist starts a URI at its beginning; the place is
                    // sought once that track plays.
                    let seek = (position_ms > 0)
                        .then(|| offset.clone().map(|uri| (uri, position_ms)))
                        .flatten();
                    if from_top
                        || device_id == crate::soloist::LOCAL_DEVICE
                        || crate::spotify::api::penalty_left().is_some()
                    {
                        if let Some(player) = player.upgrade() {
                            player.update(cx, |p, _| p.seek_when_playing = seek);
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
                                if let Some(player) = player.upgrade() {
                                    player.update(cx, |p, _| p.seek_when_playing = seek);
                                }
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
        self.soloist_held = None;
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
                self.soloist_held = state
                    .track
                    .as_ref()
                    .and_then(|t| t["uri"].as_str())
                    .map(str::to_string);
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

    /// Read the device list (when the Devices panel opens): Soloist reports
    /// what other devices play, not which devices there are.
    pub fn refresh_devices(&mut self, delay: Duration, cx: &mut Context<Self>) {
        if self.reader.is_none() || self.simulated(cx) {
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
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn anchor(&mut self, position_ms: u32) {
        self.anchor_ms = position_ms;
        self.anchor_at = Instant::now();
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
        // Soloist's track details have no album or artist ids; the Web
        // API's have, which the title and artist links and the Now Playing
        // panel need.
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

/// What Spotify says plays where (or why it could not say).
type RestoreLookup = Result<Option<crate::model::RemoteState>, String>;

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
    // Soloist names the seed track as the context of autoplay after it, and
    // a list of tracks Spotify was handed as `spotify:webapi:…`; the Web API
    // takes neither as a context.
    context.filter(|c| {
        !["spotify:artist:", "spotify:track:", "spotify:webapi:"]
            .iter()
            .any(|kind| c.starts_with(kind))
    })
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
