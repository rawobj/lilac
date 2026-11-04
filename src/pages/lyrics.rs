//! Full-page lyrics that follow the song.

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::model::Lyrics;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::spotify::lyrics::{self, active_line};
use crate::theme;
use crate::ui::{self, icons};

enum State {
    Idle,
    Loading,
    Ready(Lyrics),
    Missing,
}

pub struct LyricsPage {
    track_id: SharedString,
    state: State,
    active: Option<usize>,
    scroll: ScrollHandle,
    /// Runs only while playing, so a paused or idle app does no work.
    clock: Option<Task<()>>,
}

impl LyricsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.subscribe(
            &app::player(cx),
            |this: &mut Self, _, _: &PlayerEvent, cx| this.sync(cx),
        )
        .detach();

        let mut this = Self {
            track_id: SharedString::default(),
            state: State::Idle,
            active: None,
            scroll: ScrollHandle::new(),
            clock: None,
        };
        this.sync(cx);
        this
    }

    /// Advance the highlighted line while playing. Cheap: it only repaints
    /// when the line changes.
    fn sync_clock(&mut self, cx: &mut Context<Self>) {
        let playing = app::player(cx).read(cx).playing;
        if !playing {
            self.clock = None;
        } else if self.clock.is_none() {
            self.clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(200))
                        .await;
                    if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    /// Load lyrics when the current track changes.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.sync_clock(cx);
        self.tick(cx);
        let Some(track) = app::player(cx).read(cx).current.clone() else {
            self.track_id = SharedString::default();
            self.state = State::Idle;
            cx.notify();
            return;
        };
        if track.id == self.track_id {
            return;
        }
        self.track_id = track.id.clone();
        self.active = None;
        self.state = State::Loading;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();

        let auth = app::api(cx).auth.clone();
        let third_party = app::settings(cx).read(cx).lrclib_lyrics;
        let id = track.id.clone();
        cx.spawn(async move |this, cx| {
            let result =
                runtime::run(async move { lyrics::fetch(&auth, &track, third_party).await }).await;
            this.update(cx, |this, cx| {
                if this.track_id != id {
                    return;
                }
                this.state = match result {
                    Ok(lyrics) if !lyrics.lines.is_empty() => State::Ready(lyrics),
                    _ => State::Missing,
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let State::Ready(lyrics) = &self.state else {
            return;
        };
        if !lyrics.synced {
            return;
        }
        let position = app::player(cx).read(cx).position_ms();
        let active = active_line(&lyrics.lines, position);
        if active != self.active {
            self.active = active;
            if let Some(ix) = active {
                // Keep a couple of previous lines in view above the active one.
                self.scroll.scroll_to_item(ix.saturating_sub(2));
            }
            cx.notify();
        }
    }
}

impl Render for LyricsPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let seed = theme::seed_of(&self.track_id);
        let content = match &self.state {
            State::Idle => ui::empty_state(
                icons::MIC,
                "Nothing playing",
                "Play a song to see its lyrics.",
            )
            .into_any_element(),
            State::Loading => ui::loading_state().into_any_element(),
            State::Missing => ui::empty_state(
                icons::MIC,
                "No lyrics for this one",
                "We couldn't find lyrics for the current song.",
            )
            .into_any_element(),
            State::Ready(lyrics) => {
                let synced = lyrics.synced;
                let mut column = div()
                    .id("lyrics")
                    .size_full()
                    .overflow_y_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.scroll)
                    .px_12()
                    .py_10()
                    .flex()
                    .flex_col()
                    .gap_3();
                for (ix, line) in lyrics.lines.iter().enumerate() {
                    let state = match self.active {
                        _ if !synced => 0,
                        Some(active) if ix == active => 0,
                        Some(active) if ix < active => 1,
                        _ => 2,
                    };
                    let color = match state {
                        0 => theme::text(),
                        1 => theme::white(0.45),
                        _ => theme::white(0.7),
                    };
                    let start = line.start_ms;
                    let text: SharedString = if line.text.is_empty() {
                        "♪".into()
                    } else {
                        line.text.clone()
                    };
                    column = column.child(
                        div()
                            .id(("line", ix))
                            .text_size(px(28.0))
                            .line_height(px(38.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color)
                            .when(synced, |d| {
                                d.cursor_pointer()
                                    .hover(|s| s.text_color(theme::text()))
                                    .on_click(move |_, _, cx| {
                                        app::player(cx).update(cx, |p, cx| p.seek(start, cx))
                                    })
                            })
                            .child(text),
                    );
                }
                column
                    .child(
                        div()
                            .pt_8()
                            .text_xs()
                            .text_color(theme::white(0.5))
                            .child(format!("Lyrics provided by {}", lyrics.provider)),
                    )
                    .into_any_element()
            }
        };

        div()
            .relative()
            .size_full()
            .child(
                div()
                    .absolute()
                    .size_full()
                    .bg(theme::placeholder_tint(seed).opacity(0.45)),
            )
            .child(div().relative().size_full().child(content))
    }
}
