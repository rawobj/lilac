//! Full-page lyrics that follow the song.
//!
//! The artwork, blurred and slowly drifting, fills the page behind a dark
//! glass; the line being sung sits in the middle of the page, bright and
//! glowing in the artwork's color, with a few lines around it fading out.
//! Scrolling by hand stops the following until "Sync" is pressed.

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::images::ImageCache;
use crate::model::{Lyrics, Track};
use crate::player::PlayerEvent;
use crate::runtime;
use crate::settings::LyricsStyle;
use crate::spotify::lyrics::{self, active_line};
use crate::theme;
use crate::ui::{self, Placeholder, icons};

/// Time for the lines to glide to the next one.
const GLIDE: Duration = Duration::from_millis(420);
const FRAME: Duration = Duration::from_millis(16);
/// A full drift of the backdrop.
const DRIFT: Duration = Duration::from_secs(28);

enum State {
    Idle,
    Loading,
    Ready(Lyrics),
    Missing,
}

pub struct LyricsPage {
    track: Option<Track>,
    state: State,
    active: Option<usize>,
    scroll: ScrollHandle,
    /// Keeping the active line centered; off once the user scrolls.
    following: bool,
    /// Runs only while playing, so a paused or idle app does no work.
    clock: Option<Task<()>>,
    glide: Option<Task<()>>,
    /// Center the active line once the lines are laid out (next frame);
    /// `jump` places it without gliding, for freshly loaded lyrics.
    center_pending: bool,
    jump: bool,
}

impl LyricsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.subscribe(
            &app::player(cx),
            |this: &mut Self, _, _: &PlayerEvent, cx| this.sync(cx),
        )
        .detach();
        cx.observe(&app::settings(cx), |_, _, cx| cx.notify())
            .detach();

        let mut this = Self {
            track: None,
            state: State::Idle,
            active: None,
            scroll: ScrollHandle::new(),
            following: true,
            clock: None,
            glide: None,
            center_pending: false,
            jump: true,
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
                        .timer(Duration::from_millis(120))
                        .await;
                    if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// Load lyrics when the current track changes.
    fn sync(&mut self, cx: &mut Context<Self>) {
        self.sync_clock(cx);
        self.tick(cx);
        let Some(track) = app::player(cx).read(cx).current.clone() else {
            self.track = None;
            self.state = State::Idle;
            cx.notify();
            return;
        };
        if self.track.as_ref().is_some_and(|t| t.id == track.id) {
            return;
        }
        self.track = Some(track.clone());
        self.active = None;
        self.following = true;
        self.jump = true;
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
                if this.track.as_ref().is_none_or(|t| t.id != id) {
                    return;
                }
                this.state = match result {
                    Ok(lyrics) if !lyrics.lines.is_empty() => State::Ready(lyrics),
                    _ => State::Missing,
                };
                this.active = None;
                this.jump = true;
                this.tick(cx);
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
            if self.following {
                self.glide_to_active(cx);
            }
            cx.notify();
        }
    }

    /// The scroll offset that puts line `ix` in the middle of the view.
    fn centered_offset(&self, ix: usize) -> Option<Pixels> {
        let view = self.scroll.bounds();
        let line = self.scroll.bounds_for_item(ix)?;
        let target = view.top() + (view.size.height - line.size.height) / 2.0 - line.top();
        let lowest = -self.scroll.max_offset().y;
        Some(target.min(px(0.0)).max(lowest))
    }

    /// Glide the lines so the active one is centered.
    /// Center the active line after the next layout, when the lines'
    /// positions are known.
    fn glide_to_active(&mut self, cx: &mut Context<Self>) {
        self.center_pending = true;
        cx.notify();
    }

    fn glide_now(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        let Some(target) = self.centered_offset(ix) else {
            return;
        };
        if std::mem::take(&mut self.jump) {
            self.glide = None;
            self.scroll.set_offset(point(px(0.0), target));
            cx.notify();
            return;
        }
        let start = self.scroll.offset().y;
        let scroll = self.scroll.clone();
        self.glide = Some(cx.spawn(async move |this, cx| {
            let steps = (GLIDE.as_millis() / FRAME.as_millis()) as u32;
            for step in 1..=steps {
                cx.background_executor().timer(FRAME).await;
                let t = step as f32 / steps as f32;
                // Ease out: quick to start, gentle to land.
                let eased = 1.0 - (1.0 - t).powi(3);
                let y = start + (target - start) * eased;
                scroll.set_offset(point(px(0.0), y));
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        self.following = true;
        self.center_pending = true;
        self.glide_to_active(cx);
        cx.notify();
    }

    /// `glow` is the active line's halo color; `None` draws none.
    fn lyrics_view(
        &self,
        lyrics: &Lyrics,
        glow: Option<Hsla>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let synced = lyrics.synced;
        let mut column = div()
            .id("lyrics")
            .size_full()
            .overflow_y_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.scroll)
            // Room for the first and last lines to reach the middle.
            .pt(relative(0.42))
            .pb(relative(0.5))
            .px_10()
            .flex()
            .flex_col()
            .gap_2()
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| {
                if this.following {
                    this.following = false;
                    this.glide = None;
                    cx.notify();
                }
            }));
        for (ix, line) in lyrics.lines.iter().enumerate() {
            let active = synced && self.active == Some(ix);
            let alpha = match self.active {
                _ if !synced => 0.85,
                Some(now) if ix == now => 1.0,
                // Sung already: quiet. Coming up: dimmer the further away.
                Some(now) if ix < now => 0.28,
                Some(now) => (0.52 - (ix - now - 1) as f32 * 0.07).max(0.26),
                None => 0.45,
            };
            let start = line.start_ms;
            let text: SharedString = if line.text.trim().is_empty() {
                "• • •".into()
            } else {
                line.text.clone()
            };
            let line = div()
                .id(("line", ix))
                .relative()
                .py(px(6.0))
                .px_3()
                .rounded(px(14.0))
                .text_size(px(30.0))
                .line_height(px(40.0))
                .font_weight(FontWeight::EXTRA_BOLD)
                .text_color(theme::white(alpha))
                // The glow: a soft band of light behind the words, fading
                // out to both sides, in a light tint of the art's color (its
                // own shade would vanish into the backdrop).
                .when_some(glow.filter(|_| active), |d, glow| {
                    let width = text_width(&text).clamp(80.0, 960.0);
                    let tint = Hsla {
                        l: 0.8,
                        s: glow.s * 0.75,
                        ..glow
                    };
                    let half = |angle: f32| {
                        div().flex_1().h_full().bg(linear_gradient(
                            angle,
                            linear_color_stop(tint.opacity(0.0), 0.0),
                            linear_color_stop(tint.opacity(0.13), 1.0),
                        ))
                    };
                    d.child(
                        div()
                            .absolute()
                            .top(px(2.0))
                            .bottom(px(2.0))
                            .left(px(-24.0))
                            .w(px(width + 48.0))
                            .rounded_full()
                            .overflow_hidden()
                            .flex()
                            .child(half(90.0))
                            .child(half(270.0)),
                    )
                })
                .when(synced, |d| {
                    d.cursor_pointer()
                        .hover(|s| s.bg(theme::white(0.05)).text_color(theme::white(0.92)))
                        .on_click(move |_, _, cx| {
                            app::player(cx).update(cx, |p, cx| p.seek(start, cx))
                        })
                })
                .child(text);
            column = column.child(if active && glow.is_some() {
                // The new line brightens in rather than switching.
                line.with_animation(
                    ("glow", ix),
                    Animation::new(Duration::from_millis(380)).with_easing(ease_out_quint()),
                    |d, t| d.opacity(0.55 + 0.45 * t),
                )
                .into_any_element()
            } else {
                line.into_any_element()
            });
        }
        column
            .child(
                div()
                    .pt_10()
                    .px_3()
                    .text_xs()
                    .text_color(theme::white(0.45))
                    .child(format!("Lyrics provided by {}", lyrics.provider)),
            )
            .into_any_element()
    }
}

/// Rough width of a lyric line at 30 px: wide (CJK, kana, Hangul) glyphs
/// take about twice the room of Latin ones.
fn text_width(text: &str) -> f32 {
    text.chars()
        .map(|c| match c as u32 {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFF60 => 30.0,
            _ if c.is_whitespace() => 8.0,
            _ => 16.0,
        })
        .sum()
}

/// The artwork, blurred, larger than the page and drifting slowly while
/// music plays, under a dark glass that keeps the text readable.
fn backdrop(
    art: Option<&SharedString>,
    playing: bool,
    window: &Window,
    cx: &mut App,
) -> (Div, Hsla) {
    let blurred = art.and_then(|url| ImageCache::backdrop(url, cx));
    let glow = blurred.as_ref().map(|b| b.glow).unwrap_or(theme::accent());
    // Light artwork needs more darkening for white text to stand out.
    let luma = blurred.as_ref().map(|b| b.luma).unwrap_or(0.2);
    let size = window.viewport_size();
    let edge = f32::from(size.width.max(size.height)) * 1.5;
    let mut layer = div()
        .absolute()
        .size_full()
        .overflow_hidden()
        .bg(theme::backdrop_opaque());
    if let Some(image) = blurred.map(|b| b.image) {
        for (ix, (phase, opacity)) in [(0.0f32, 1.0f32), (0.5, 0.55)].into_iter().enumerate() {
            let art = img(ImageSource::Render(image.clone()))
                .absolute()
                .size(px(edge))
                .opacity(opacity)
                .object_fit(ObjectFit::Cover);
            let place = move |el: Img, t: f32| {
                let a = (t + phase) * std::f32::consts::TAU;
                // Two slow circles in opposite directions.
                let (dx, dy) = if ix == 0 {
                    (a.cos(), a.sin())
                } else {
                    (-a.sin(), a.cos())
                };
                el.left(px(-edge * 0.25 + dx * edge * 0.06))
                    .top(px(-edge * 0.3 + dy * edge * 0.06))
            };
            layer = if playing {
                layer.child(art.with_animation(
                    ("drift", ix),
                    Animation::new(DRIFT).repeat(),
                    place,
                ))
            } else {
                layer.child(place(art, 0.0))
            };
        }
    }
    let glass = div().absolute().size_full().bg(linear_gradient(
        180.0,
        linear_color_stop(theme::black(0.3 + 0.42 * luma), 0.0),
        linear_color_stop(theme::black(0.16 + 0.42 * luma), 1.0),
    ));
    (layer.child(glass), glow)
}

/// A message in place of the lyrics (nothing playing, none found,
/// loading), on a glass card tinted like the artwork so it belongs to the
/// page rather than sitting on it.
fn notice(icon: Option<&'static str>, title: &str, detail: &str, tint: Hsla) -> Div {
    let badge = div()
        .size(px(64.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(tint.opacity(0.22))
        .border_1()
        .border_color(tint.opacity(0.5))
        .shadow(vec![BoxShadow {
            color: tint.opacity(0.35),
            offset: point(px(0.0), px(0.0)),
            blur_radius: px(36.0),
            spread_radius: px(-6.0),
            inset: false,
        }]);
    let badge = match icon {
        Some(icon) => badge.child(
            svg()
                .path(icon)
                .size(px(28.0))
                .text_color(theme::white(0.92)),
        ),
        None => badge.child(ui::spinner(26.0)),
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p_8()
        .child(
            div()
                .max_w(px(420.0))
                .px_10()
                .py_8()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .rounded(px(24.0))
                .bg(theme::white(0.06))
                .border_1()
                .border_color(theme::white(0.12))
                .child(badge)
                .child(
                    div()
                        .pt_1()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::white(0.95))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_center()
                        .text_color(theme::white(0.72))
                        .child(detail.to_string()),
                ),
        )
}

/// Song, artist and cover beside the lyrics on wide windows.
fn now_playing(track: &Track, window: &Window, cx: &mut App) -> Div {
    let edge = (f32::from(window.viewport_size().height) * 0.36).clamp(200.0, 340.0);
    div()
        .flex_none()
        .w(px(edge + 40.0))
        .h_full()
        .pl_10()
        .flex()
        .flex_col()
        .justify_center()
        .gap_4()
        .child(
            div()
                .rounded(px(16.0))
                .shadow(vec![BoxShadow {
                    color: theme::black(0.5),
                    offset: point(px(0.0), px(24.0)),
                    blur_radius: px(60.0),
                    spread_radius: px(-8.0),
                    inset: false,
                }])
                .child(ui::cover(
                    track.cover().for_size(edge as u32),
                    edge,
                    16.0,
                    Placeholder::Music,
                    window,
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::BOLD)
                        .line_clamp(2)
                        .child(track.name.clone()),
                )
                .child(ui::artist_line_links(
                    "lyrics",
                    track,
                    theme::white(0.7),
                    theme::white(1.0),
                )),
        )
}

impl Render for LyricsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.center_pending) && matches!(self.state, State::Ready(_)) {
            cx.on_next_frame(window, |this, _, cx| this.glide_now(cx));
        }
        let playing = app::player(cx).read(cx).playing;
        let vivid = app::settings(cx).read(cx).lyrics_style == LyricsStyle::Vivid;
        let (backdrop, glow) = if vivid {
            let art = self.track.as_ref().and_then(|t| t.cover().for_size(640));
            backdrop(art.as_ref(), playing, window, cx)
        } else {
            let seed = self
                .track
                .as_ref()
                .map(|t| theme::seed_of(&t.id))
                .unwrap_or(7);
            (
                div()
                    .absolute()
                    .size_full()
                    .bg(theme::placeholder_tint(seed).opacity(0.45)),
                theme::accent(),
            )
        };

        let lyrics = match &self.state {
            State::Idle => notice(
                Some(icons::MIC),
                "Nothing playing",
                "Play a song to see its lyrics here.",
                glow,
            )
            .into_any_element(),
            State::Loading => {
                notice(None, "Finding the lyrics", "One moment…", glow).into_any_element()
            }
            State::Missing => notice(
                Some(icons::MIC),
                "No lyrics for this song",
                "Spotify and the lyrics services don't have the words for it yet.",
                glow,
            )
            .into_any_element(),
            State::Ready(lyrics) => {
                let lyrics = lyrics.clone();
                div()
                    .relative()
                    .size_full()
                    .child(self.lyrics_view(&lyrics, vivid.then_some(glow), cx))
                    .group("scrollable")
                    .child(ui::vertical_scrollbar(&self.scroll))
                    .into_any_element()
            }
        };
        let synced = matches!(&self.state, State::Ready(l) if l.synced);
        let wide = vivid && f32::from(window.viewport_size().width) >= 1280.0;

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(backdrop)
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .when_some(self.track.as_ref().filter(|_| wide), |row, track| {
                        row.child(now_playing(track, window, cx))
                    })
                    .child(div().relative().flex_1().min_w_0().h_full().child(lyrics)),
            )
            // Lines fade in and out at the edges instead of being cut off.
            .when(vivid, |d| {
                d.child(div().absolute().top_0().left_0().right_0().h(px(90.0)).bg(
                    linear_gradient(
                        180.0,
                        linear_color_stop(theme::black(0.3), 0.0),
                        linear_color_stop(theme::black(0.0), 1.0),
                    ),
                ))
            })
            .when(vivid, |d| {
                d.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .h(px(120.0))
                        .bg(linear_gradient(
                            0.0,
                            linear_color_stop(theme::black(0.2), 0.0),
                            linear_color_stop(theme::black(0.0), 1.0),
                        )),
                )
            })
            .when(synced && !self.following, |d| {
                d.child(
                    div().absolute().bottom_6().right_6().child(
                        div()
                            .id("lyrics-sync")
                            .occlude()
                            .h(px(40.0))
                            .px_4()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_full()
                            .bg(theme::white(0.92))
                            .text_color(theme::backdrop_opaque())
                            .font_weight(FontWeight::SEMIBOLD)
                            .shadow(vec![BoxShadow {
                                color: theme::black(0.4),
                                offset: point(px(0.0), px(8.0)),
                                blur_radius: px(24.0),
                                spread_radius: px(0.0),
                                inset: false,
                            }])
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::white(1.0)))
                            .child(
                                svg()
                                    .path(icons::REFRESH)
                                    .size(px(16.0))
                                    .text_color(theme::backdrop_opaque()),
                            )
                            .child("Sync")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.follow(cx)
                            })),
                    ),
                )
            })
    }
}
