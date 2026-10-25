//! Playback state: what is playing, where in the track we are, and what comes next.
//!
//! With an account, playback runs through Spotify Connect: lilac is one
//! device among the user's others, Spotify decides what plays next, and this
//! entity mirrors that state for the UI. When another device is active, the
//! same controls drive that device instead. In demo mode there is nothing to
//! stream, so a local queue and a clock stand in.

mod backend;
pub mod queue;

use std::time::{Duration, Instant};

use gpui_kit::*;
use librespot_metadata::audio::{AudioItem, UniqueFields};

use crate::app;
use crate::model::{AlbumRef, ArtistRef, ContextUri, Images, ItemKind, ShowRef, Track};
use crate::runtime;
use crate::session::{Session, SessionEvent};
use crate::settings::Settings;

use backend::{Backend, Channels, Cluster, EngineEvent, Options, Start};
pub use backend::{Device, DeviceKind};
pub use queue::{Queue, Repeat};

/// Restart the current track instead of going to the previous one past this point.
const RESTART_THRESHOLD_MS: u32 = 3000;
/// Connect the streaming session this long after login, once the first
/// screen has what it needs from the network.
const CONNECT_DELAY: Duration = Duration::from_secs(2);

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
    /// Position at `anchor_at`. While playing, the live position is extrapolated from it.
    anchor_ms: u32,
    anchor_at: Instant,
    ticker: Option<Task<()>>,
    backend: Option<Backend>,
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
}

impl EventEmitter<PlayerEvent> for Player {}

impl Player {
    pub fn new(session: &Entity<Session>, settings: &Settings, cx: &mut Context<Self>) -> Self {
        cx.subscribe(session, |this, session, event, cx| match event {
            SessionEvent::Ready if !session.read(cx).is_demo() => {
                // Join the user's Connect devices without waiting for a first play.
                this.starting = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(CONNECT_DELAY).await;
                    this.update(cx, |this, cx| {
                        this.starting = None;
                        this.ensure_backend(cx);
                    })
                    .ok();
                }));
            }
            SessionEvent::Ready => {}
            SessionEvent::LoggedOut => {
                this.stop(cx);
                this.drop_backend();
                this.devices.clear();
                this.active_device = None;
            }
        })
        .detach();

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
            backend: None,
            starting: None,
            pending: None,
            listeners: Vec::new(),
            connect_context: None,
            connect_shuffle: false,
            liked_uri: None,
            mirrored_next: Vec::new(),
            lookup: None,
        }
    }

    // ------------------------------------------------------------------ queries

    pub fn duration_ms(&self) -> u32 {
        self.current.as_ref().map(|t| t.duration_ms).unwrap_or(0)
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

    /// The device playing, when it is not this one.
    pub fn remote_device(&self) -> Option<&Device> {
        let ours = self.backend.as_ref().map(|b| b.device_id.as_str());
        let active = self.active_device.as_deref()?;
        if Some(active) == ours {
            return None;
        }
        self.devices.iter().find(|d| d.id == active)
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

        // Show the choice at once; Spotify confirms it a moment later.
        self.current = Some(first);
        self.connect_context = context_uri.clone();
        self.anchor_ms = 0;
        self.anchor_at = Instant::now();
        self.playing = true;
        self.loading = true;
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();

        if self.remote_device().is_some() {
            let api = app::api(cx);
            let offset = match &start {
                Start::Uri(uri) => Some(uri.clone()),
                Start::Index(ix) => uris.get(*ix as usize).cloned(),
            };
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
        self.load(
            PendingLoad {
                context: context_uri,
                tracks: uris,
                start,
            },
            cx,
        );
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
        if !self.playing {
            return;
        }
        self.anchor_ms = self.position_ms();
        self.anchor_at = Instant::now();
        self.playing = false;
        self.ticker = None;
        if self.remote_device().is_some() {
            let api = app::api(cx);
            remote(cx, async move { api.remote_pause().await });
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
        if self.playing || self.current.is_none() {
            return;
        }
        self.anchor_at = Instant::now();
        self.playing = true;
        if self.simulated(cx) {
            self.start_ticker(cx);
        } else if self.remote_device().is_some() {
            let api = app::api(cx);
            remote(cx, async move {
                api.remote_play(None, None, None, None, 0).await
            });
        } else if let Some(backend) = self.backend() {
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
        self.anchor_ms = 0;
        self.ticker = None;
        self.connect_context = None;
        self.mirrored_next.clear();
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
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
            let api = app::api(cx);
            remote(cx, async move { api.remote_next().await });
        } else if let Some(backend) = self.backend() {
            backend.next();
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
        if !self.simulated(cx) {
            // Spotify restarts the track or goes back itself, by the same rule.
            if self.remote_device().is_some() {
                let api = app::api(cx);
                remote(cx, async move { api.remote_previous().await });
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
        self.anchor_ms = position_ms;
        self.anchor_at = Instant::now();
        if self.remote_device().is_some() {
            let api = app::api(cx);
            remote(cx, async move { api.remote_seek(position_ms).await });
        } else if let Some(backend) = self.backend() {
            backend.seek(position_ms);
        }
        cx.emit(PlayerEvent::Seeked);
        cx.notify();
    }

    /// Seek relative to the current position.
    pub fn seek_by(&mut self, delta_ms: i32, cx: &mut Context<Self>) {
        let target = (self.position_ms() as i64 + delta_ms as i64).max(0) as u32;
        self.seek(target, cx);
    }

    pub fn set_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        self.volume = volume.clamp(0.0, 1.0);
        self.muted = false;
        self.apply_volume(cx);
        cx.notify();
    }

    /// Write the volume to disk. Called when a drag ends rather than on every step.
    pub fn persist_volume(&self, cx: &mut Context<Self>) {
        let volume = self.volume;
        app::update_settings(cx, |settings| settings.volume = volume);
        if self.remote_device().is_some() {
            // One request per gesture, not per drag step.
            let api = app::api(cx);
            let percent = (self.effective_volume() * 100.0).round() as u32;
            remote(cx, async move { api.remote_volume(percent).await });
        }
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
                let api = app::api(cx);
                remote(cx, async move { api.remote_shuffle(on).await });
            } else if let Some(backend) = self.backend() {
                backend.set_shuffle(on);
            }
        }
        cx.notify();
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = self.repeat.next();
        let repeat = self.repeat;
        if self.remote_device().is_some() {
            let api = app::api(cx);
            let state = match repeat {
                Repeat::Off => "off",
                Repeat::Context => "context",
                Repeat::Track => "track",
            };
            remote(cx, async move { api.remote_repeat(state).await });
        } else if let Some(backend) = self.backend() {
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
                if let Err(err) =
                    backend.load(load.context.as_deref(), &load.tracks, load.start, shuffle)
                {
                    app::toast_error(format!("{err:#}"), cx);
                }
            }
            None => {
                self.pending = Some(load);
                self.ensure_backend(cx);
            }
        }
    }

    fn apply_volume(&mut self, _cx: &mut Context<Self>) {
        if self.remote_device().is_none()
            && let Some(backend) = self.backend()
        {
            backend.set_volume(self.effective_volume());
        }
    }

    /// Connect the streaming session in the background, then run any pending load.
    fn ensure_backend(&mut self, cx: &mut Context<Self>) {
        if self.backend.is_some() || self.starting.is_some() {
            return;
        }
        let options = Options::from_settings(app::settings(cx).read(cx));
        let auth = app::session(cx).read(cx).streaming.clone();
        let volume = self.effective_volume();
        self.liked_uri = app::session(cx)
            .read(cx)
            .user
            .as_ref()
            .map(|u| format!("spotify:user:{}:collection", u.id));
        self.starting = Some(cx.spawn(async move |this, cx| {
            let result = runtime::run(Backend::start(auth, options, volume)).await;
            this.update(cx, |this, cx| {
                this.starting = None;
                match result {
                    Ok((backend, channels)) => {
                        this.listen(channels, cx);
                        this.backend = Some(backend);
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
        if self.remote_device().is_some() {
            return;
        }
        match event {
            EngineEvent::TrackChanged { audio_item } => {
                self.current = Some(track_from_audio_item(&audio_item));
                self.anchor_ms = 0;
                self.anchor_at = Instant::now();
                cx.emit(PlayerEvent::TrackChanged);
                cx.notify();
            }
            EngineEvent::Playing { position_ms, .. } => {
                self.loading = false;
                self.playing = true;
                self.anchor(position_ms);
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Paused { position_ms, .. } => {
                self.loading = false;
                self.playing = false;
                self.anchor(position_ms);
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Seeked { position_ms, .. }
            | EngineEvent::PositionCorrection { position_ms, .. }
            | EngineEvent::PositionChanged { position_ms, .. } => {
                self.anchor(position_ms);
                cx.notify();
            }
            EngineEvent::Stopped { .. } => {
                self.playing = false;
                self.loading = false;
                cx.emit(PlayerEvent::StateChanged);
                cx.notify();
            }
            EngineEvent::Unavailable { .. } => {
                let name = self
                    .current
                    .as_ref()
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                // Spotify moves on to the next track by itself.
                app::toast_error(format!("\u{201c}{name}\u{201d} isn't available"), cx);
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
            EngineEvent::VolumeChanged { volume } => {
                self.volume = backend::from_mixer(volume);
                self.muted = false;
                cx.notify();
            }
            _ => {}
        }
    }

    /// Spotify's view of all devices, pushed on every change.
    fn on_cluster(&mut self, update: Cluster, cx: &mut Context<Self>) {
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

    fn anchor(&mut self, position_ms: u32) {
        self.anchor_ms = position_ms;
        self.anchor_at = Instant::now();
    }

    /// Fill in the details of a track another device started.
    fn look_up_current(&mut self, uri: String, duration_ms: u32, cx: &mut Context<Self>) {
        let api = app::api(cx);
        self.lookup = Some(cx.spawn(async move |this, cx| {
            let lookup_uri = uri.clone();
            let result = runtime::run(async move { api.item(&lookup_uri).await }).await;
            this.update(cx, |this, cx| {
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
                this.current = Some(track);
                cx.emit(PlayerEvent::TrackChanged);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Rebuild the mirrored "Next up" list from Spotify's queue.
    fn refresh_queue(&mut self, cx: &mut Context<Self>) {
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let Ok(remote) = runtime::run(async move { api.remote_queue().await }).await else {
                return;
            };
            this.update(cx, |this, cx| {
                this.queue = Queue::mirror(remote.currently_playing, remote.queue);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

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
