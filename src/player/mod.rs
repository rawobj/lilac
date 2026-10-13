//! Playback state: what is playing, where in the track we are, and what comes next.

mod backend;
pub mod queue;

use std::time::{Duration, Instant};

use gpui_kit::*;

use crate::app;
use crate::model::{ContextUri, Track};
use crate::session::{Session, SessionEvent};
use crate::settings::Settings;

pub use queue::{Queue, Repeat};

/// Restart the current track instead of going to the previous one past this point.
const RESTART_THRESHOLD_MS: u32 = 3000;

pub enum PlayerEvent {
    /// A different track became current.
    TrackChanged,
    /// Playback started or paused.
    StateChanged,
}

pub struct Player {
    pub queue: Queue,
    pub current: Option<Track>,
    pub playing: bool,
    /// The track is being fetched and nothing is audible yet.
    pub loading: bool,
    pub repeat: Repeat,
    pub volume: f32,
    pub muted: bool,
    /// Position at `anchor_at`. While playing, the live position is extrapolated from it.
    anchor_ms: u32,
    anchor_at: Instant,
    ticker: Option<Task<()>>,
}

impl EventEmitter<PlayerEvent> for Player {}

impl Player {
    pub fn new(session: &Entity<Session>, settings: &Settings, cx: &mut Context<Self>) -> Self {
        cx.subscribe(session, |this, _, event, cx| {
            if let SessionEvent::LoggedOut = event {
                this.stop(cx);
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
            anchor_ms: 0,
            anchor_at: Instant::now(),
            ticker: None,
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

    /// True when `context` is what the queue is playing from.
    pub fn is_context(&self, context: &ContextUri) -> bool {
        self.queue.context.as_ref() == Some(context)
    }

    pub fn shuffle(&self) -> bool {
        self.queue.shuffled()
    }

    pub fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
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
        let shuffle = self.queue.shuffled();
        match self
            .queue
            .set_context(context, name.into(), tracks, index, shuffle)
        {
            Some(track) => self.start(track, 0, cx),
            None => app::toast("Nothing to play here", cx),
        }
    }

    /// Play a context from a random position with shuffle switched on.
    pub fn shuffle_context(
        &mut self,
        context: Option<ContextUri>,
        name: impl Into<SharedString>,
        tracks: Vec<Track>,
        cx: &mut Context<Self>,
    ) {
        if tracks.is_empty() {
            return;
        }
        self.queue.set_shuffle(true);
        let start = (self.anchor_at.elapsed().subsec_nanos() as usize) % tracks.len();
        self.play_context(context, name, tracks, start, cx);
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.current.is_none() {
            if let Some(track) = self.queue.current().cloned() {
                self.start(track, 0, cx);
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
        self.backend_pause(cx);
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        if self.playing || self.current.is_none() {
            return;
        }
        self.anchor_at = Instant::now();
        self.playing = true;
        self.backend_resume(cx);
        self.start_ticker(cx);
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.backend_stop(cx);
        self.queue = Queue::default();
        self.current = None;
        self.playing = false;
        self.loading = false;
        self.anchor_ms = 0;
        self.ticker = None;
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    pub fn next(&mut self, cx: &mut Context<Self>) {
        // An explicit skip leaves "repeat one" for the next track, like the official clients.
        let repeat = if self.repeat == Repeat::Track {
            Repeat::Context
        } else {
            self.repeat
        };
        match self.advance_to_playable(repeat) {
            Some(track) => self.start(track, 0, cx),
            None => self.finish(cx),
        }
    }

    pub fn previous(&mut self, cx: &mut Context<Self>) {
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
            Some(track) => self.start(track, 0, cx),
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
        self.backend_seek(position_ms, cx);
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
        self.backend_volume(cx);
        cx.notify();
    }

    /// Write the volume to disk. Called when a drag ends rather than on every step.
    pub fn persist_volume(&self, cx: &mut Context<Self>) {
        let volume = self.volume;
        app::update_settings(cx, |settings| settings.volume = volume);
    }

    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        self.backend_volume(cx);
        cx.notify();
    }

    pub fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        let on = !self.queue.shuffled();
        self.queue.set_shuffle(on);
        self.backend_modes(cx);
        self.queue_changed(cx);
    }

    pub fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.repeat = self.repeat.next();
        self.backend_modes(cx);
        self.queue_changed(cx);
    }

    pub fn enqueue(&mut self, track: Track, cx: &mut Context<Self>) {
        if !track.playable() {
            app::toast("This item can't be played", cx);
            return;
        }
        if self.current.is_none() && self.queue.is_empty() {
            self.play_context(None, track.name.clone(), vec![track], 0, cx);
            return;
        }
        self.backend_enqueue(&track, cx);
        self.queue.push_user(track);
        app::toast("Added to queue", cx);
        self.queue_changed(cx);
    }

    pub fn play_next(&mut self, track: Track, cx: &mut Context<Self>) {
        if !track.playable() {
            app::toast("This item can't be played", cx);
            return;
        }
        if self.current.is_none() && self.queue.is_empty() {
            self.play_context(None, track.name.clone(), vec![track], 0, cx);
            return;
        }
        self.queue.push_user_front(track);
        app::toast("Playing next", cx);
        self.queue_changed(cx);
    }

    pub fn remove_queued(&mut self, index: usize, cx: &mut Context<Self>) {
        self.queue.remove_user(index);
        self.queue_changed(cx);
    }

    pub fn clear_queue(&mut self, cx: &mut Context<Self>) {
        self.queue.clear_user();
        self.queue_changed(cx);
    }

    /// Jump to the `n`-th track of the manual queue.
    pub fn skip_to_queued(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(track) = self.queue.skip_to_user(n) {
            self.start(track, 0, cx);
        }
    }

    /// Jump to the `n`-th upcoming track of the context.
    pub fn skip_to_upcoming(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(track) = self.queue.skip_to_upcoming(n) {
            self.start(track, 0, cx);
        }
    }

    // ---------------------------------------------------------------- internals

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

    fn start(&mut self, track: Track, position_ms: u32, cx: &mut Context<Self>) {
        self.current = Some(track.clone());
        self.anchor_ms = position_ms;
        self.anchor_at = Instant::now();
        self.playing = true;
        self.backend_start(&track, position_ms, cx);
        self.start_ticker(cx);
        self.queue_changed(cx);
        cx.emit(PlayerEvent::TrackChanged);
        cx.notify();
    }

    /// The current track played to its end.
    fn track_ended(&mut self, cx: &mut Context<Self>) {
        if self.repeat == Repeat::Track {
            if let Some(track) = self.current.clone() {
                self.start(track, 0, cx);
                return;
            }
        }
        match self.advance_to_playable(self.repeat) {
            Some(track) => self.start(track, 0, cx),
            None => self.finish(cx),
        }
    }

    /// Nothing left to play: rest at the start of the last track.
    fn finish(&mut self, cx: &mut Context<Self>) {
        self.playing = false;
        self.loading = false;
        self.anchor_ms = 0;
        self.ticker = None;
        self.backend_pause(cx);
        self.backend_seek(0, cx);
        cx.emit(PlayerEvent::StateChanged);
        cx.notify();
    }

    /// Something about the upcoming tracks changed.
    fn queue_changed(&mut self, cx: &mut Context<Self>) {
        self.backend_preload(cx);
        cx.notify();
    }

    /// Without an audio backend the clock alone decides when a track is over.
    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if !self.simulated() {
            self.ticker = None;
            return;
        }
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

    // ------------------------------------------------------------------ backend
    //
    // Hooks where an audio backend plugs in. Until one is attached, playback
    // is simulated by the clock above.

    fn simulated(&self) -> bool {
        true
    }

    fn backend_start(&mut self, _track: &Track, _position_ms: u32, _cx: &mut Context<Self>) {}

    fn backend_pause(&mut self, _cx: &mut Context<Self>) {}

    fn backend_resume(&mut self, _cx: &mut Context<Self>) {}

    fn backend_stop(&mut self, _cx: &mut Context<Self>) {}

    fn backend_seek(&mut self, _position_ms: u32, _cx: &mut Context<Self>) {}

    fn backend_volume(&mut self, _cx: &mut Context<Self>) {}

    fn backend_modes(&mut self, _cx: &mut Context<Self>) {}

    fn backend_enqueue(&mut self, _track: &Track, _cx: &mut Context<Self>) {}

    fn backend_preload(&mut self, _cx: &mut Context<Self>) {}
}
