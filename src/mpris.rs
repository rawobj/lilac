//! MPRIS on the session bus, so media keys, desktop widgets and
//! `playerctl` can see and drive playback. Linux only.
//!
//! The D-Bus object lives on the UI thread: requests from the bus become
//! messages handled with the app at hand, and player changes are pushed to
//! the bus as they happen rather than polled.

use std::rc::Rc;

use gpui_kit::*;
use mpris_server::{LoopStatus, Metadata, PlaybackStatus, Player as Bus, Time, TrackId, Volume};

use crate::app;
use crate::player::{PlayerEvent, Repeat};

enum Request {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    SeekBy(i64),
    SetPosition(i64),
    Volume(f64),
    Shuffle(bool),
    Loop(LoopStatus),
    Raise,
}

pub fn init(cx: &mut App) {
    if !app::settings(cx).read(cx).media_keys {
        return;
    }
    cx.spawn(async move |cx| {
        let Ok(bus) = Bus::builder("lilac")
            .identity("lilac")
            .desktop_entry("lilac")
            .can_raise(true)
            .can_play(true)
            .can_pause(true)
            .can_go_next(true)
            .can_go_previous(true)
            .can_seek(true)
            .can_control(true)
            .build()
            .await
        else {
            // No session bus, e.g. in a bare container. Nothing else depends on it.
            return;
        };
        let bus = Rc::new(bus);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Request>();
        connect(&bus, tx);
        cx.foreground_executor().spawn(bus.run()).detach();

        // Push player changes to the bus.
        let pushed = bus.clone();
        let subscription = cx.update(|cx| {
            let player = app::player(cx);
            cx.subscribe(&player, move |player, event: &PlayerEvent, cx| {
                let state = Snapshot::of(&player, cx);
                let bus = pushed.clone();
                let seeked = matches!(event, PlayerEvent::Seeked);
                cx.foreground_executor()
                    .spawn(async move { state.push(&bus, seeked).await })
                    .detach();
            })
        });
        // Held for as long as the bridge runs.
        let _subscription = subscription;

        while let Some(request) = rx.recv().await {
            cx.update(|cx| handle(request, cx));
        }
    })
    .detach();
}

fn connect(bus: &Bus, tx: tokio::sync::mpsc::UnboundedSender<Request>) {
    macro_rules! on {
        ($method:ident, $request:expr) => {{
            let tx = tx.clone();
            bus.$method(move |_| {
                let _ = tx.send($request);
            });
        }};
    }
    on!(connect_play_pause, Request::PlayPause);
    on!(connect_play, Request::Play);
    on!(connect_pause, Request::Pause);
    on!(connect_stop, Request::Stop);
    on!(connect_next, Request::Next);
    on!(connect_previous, Request::Previous);
    on!(connect_raise, Request::Raise);
    let t = tx.clone();
    bus.connect_seek(move |_, offset| {
        let _ = t.send(Request::SeekBy(offset.as_millis()));
    });
    let t = tx.clone();
    bus.connect_set_position(move |_, _, position| {
        let _ = t.send(Request::SetPosition(position.as_millis()));
    });
    let t = tx.clone();
    bus.connect_set_volume(move |_, volume| {
        let _ = t.send(Request::Volume(volume));
    });
    let t = tx.clone();
    bus.connect_set_shuffle(move |_, on| {
        let _ = t.send(Request::Shuffle(on));
    });
    bus.connect_set_loop_status(move |_, status| {
        let _ = tx.send(Request::Loop(status));
    });
}

fn handle(request: Request, cx: &mut App) {
    if let Request::Raise = request {
        if let Some(window) = cx.windows().first() {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        return;
    }
    let player = app::player(cx);
    player.update(cx, |p, cx| match request {
        Request::PlayPause => p.toggle(cx),
        Request::Play => p.resume(cx),
        Request::Pause => p.pause(cx),
        Request::Stop => p.pause(cx),
        Request::Next => p.next(cx),
        Request::Previous => p.previous(cx),
        Request::SeekBy(ms) => p.seek_by(ms.clamp(i32::MIN as i64, i32::MAX as i64) as i32, cx),
        Request::SetPosition(ms) => p.seek(ms.max(0) as u32, cx),
        Request::Volume(v) => {
            p.set_volume(v as f32, cx);
            p.persist_volume(cx);
        }
        Request::Shuffle(on) => {
            if p.shuffle() != on {
                p.toggle_shuffle(cx);
            }
        }
        Request::Loop(status) => {
            let target = match status {
                LoopStatus::None => Repeat::Off,
                LoopStatus::Playlist => Repeat::Context,
                LoopStatus::Track => Repeat::Track,
            };
            // Repeat cycles Off → Context → Track; step until it matches.
            for _ in 0..3 {
                if p.repeat == target {
                    break;
                }
                p.cycle_repeat(cx);
            }
        }
        Request::Raise => {}
    });
}

/// What the bus needs to know, copied out so it can be sent after the borrow ends.
struct Snapshot {
    status: PlaybackStatus,
    metadata: Metadata,
    position: Time,
    volume: Volume,
    shuffle: bool,
    loop_status: LoopStatus,
}

impl Snapshot {
    fn of(player: &Entity<crate::player::Player>, cx: &App) -> Self {
        let p = player.read(cx);
        let status = match (&p.current, p.playing) {
            (None, _) => PlaybackStatus::Stopped,
            (Some(_), true) => PlaybackStatus::Playing,
            (Some(_), false) => PlaybackStatus::Paused,
        };
        let mut metadata = Metadata::new();
        if let Some(track) = &p.current {
            // MPRIS track ids are D-Bus object paths; base62 ids are valid path elements.
            if let Ok(id) = TrackId::try_from(format!("/app/lilac/track/{}", track.id)) {
                metadata.set_trackid(Some(id));
            }
            metadata.set_title(Some(track.name.to_string()));
            metadata.set_artist(Some(track.artists.iter().map(|a| a.name.to_string())));
            if let Some(album) = &track.album {
                metadata.set_album(Some(album.name.to_string()));
            } else if let Some(show) = &track.show {
                metadata.set_album(Some(show.name.to_string()));
            }
            metadata.set_length(Some(Time::from_millis(track.duration_ms as i64)));
            if let Some(art) = track.cover().for_size(640) {
                metadata.set_art_url(Some(art.to_string()));
            }
            metadata.set_url(Some(format!(
                "https://open.spotify.com/{}/{}",
                if track.is_episode() {
                    "episode"
                } else {
                    "track"
                },
                track.id
            )));
        }
        Self {
            status,
            metadata,
            position: Time::from_millis(p.position_ms() as i64),
            volume: p.effective_volume() as f64,
            shuffle: p.shuffle(),
            loop_status: match p.repeat {
                Repeat::Off => LoopStatus::None,
                Repeat::Context => LoopStatus::Playlist,
                Repeat::Track => LoopStatus::Track,
            },
        }
    }

    async fn push(self, bus: &Bus, seeked: bool) {
        bus.set_position(self.position);
        let _ = bus.set_metadata(self.metadata).await;
        let _ = bus.set_playback_status(self.status).await;
        let _ = bus.set_volume(self.volume).await;
        let _ = bus.set_shuffle(self.shuffle).await;
        let _ = bus.set_loop_status(self.loop_status).await;
        if seeked {
            let _ = bus.seeked(self.position).await;
        }
    }
}
