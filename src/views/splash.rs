//! The loading screen shown while the player warms up after login.
//!
//! It stays until the song Lilac was left on is in the player bar, which is
//! usually as soon as the bloom has played: the track is read from disk at
//! launch. Loading it into the streaming session and buffering it carries
//! on underneath (see `Player::restore_session`); a Play pressed before that
//! is done starts the song when it is.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::player::Warmup;
use crate::theme;
use crate::ui::icons;

/// Long enough for the bloom to finish.
const MIN_SHOWN: Duration = Duration::from_millis(1100);
/// Never longer than this, whatever the network does.
const MAX_SHOWN: Duration = Duration::from_millis(4000);
const FADE: Duration = Duration::from_millis(280);
/// The bloom: stem first, then florets from the bottom up.
const BLOOM_START: f32 = 0.12;
const BLOOM_STAGGER: f32 = 0.045;
const BLOOM_GROW: f32 = 0.42;
/// Animations do not need the display's full refresh rate.
const FPS: f32 = 60.0;

/// Edge of the sprig drawing, in pixels.
const SPRIG: f32 = 220.0;
/// Florets of the app icon in its 1024 frame: x, y, radius (top to bottom).
const FLORETS: [(f32, f32, f32); 14] = [
    (497.0, 205.0, 44.0),
    (430.0, 272.0, 66.0),
    (548.0, 262.0, 64.0),
    (372.0, 352.0, 64.0),
    (478.0, 348.0, 70.0),
    (588.0, 344.0, 64.0),
    (402.0, 440.0, 62.0),
    (508.0, 434.0, 66.0),
    (614.0, 430.0, 58.0),
    (448.0, 520.0, 58.0),
    (556.0, 516.0, 60.0),
    (484.0, 596.0, 52.0),
    (580.0, 590.0, 50.0),
    (536.0, 664.0, 44.0),
];

pub enum SplashEvent {
    Done,
}

pub struct Splash {
    shown_at: Instant,
    /// Set once the fade-out started.
    leaving: Option<Instant>,
    /// Progress shown by the bar, eased towards the current stage.
    from: f32,
    target: f32,
    target_at: Instant,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<SplashEvent> for Splash {}

impl Splash {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe(&app::player(cx), |this, player, cx| {
            let player = player.read(cx);
            this.set_target(stage_progress(player.warmup));
            if can_leave(player) {
                this.leave_after(MIN_SHOWN.saturating_sub(this.shown_at.elapsed()), cx);
            }
            cx.notify();
        })
        .detach();
        let deadline = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MAX_SHOWN).await;
            this.update(cx, |this, cx| this.leave_after(Duration::ZERO, cx))
                .ok();
        });
        let now = Instant::now();
        let mut this = Self {
            shown_at: now,
            leaving: None,
            from: 0.0,
            target: 0.08,
            target_at: now,
            _tasks: vec![deadline],
        };
        let player = app::player(cx).read(cx);
        this.set_target(stage_progress(player.warmup));
        if can_leave(player) {
            this.leave_after(MIN_SHOWN, cx);
        }
        this
    }

    fn set_target(&mut self, target: f32) {
        if target > self.target {
            self.from = self.bar();
            self.target = target;
            self.target_at = Instant::now();
        }
    }

    /// The bar eases towards the stage it is in, so it never jumps.
    fn bar(&self) -> f32 {
        let t = (self.target_at.elapsed().as_secs_f32() / 0.6).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.target - self.from) * eased
    }

    fn leave_after(&mut self, wait: Duration, cx: &mut Context<Self>) {
        if self.leaving.is_some() || self._tasks.len() > 1 {
            return;
        }
        self._tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            this.update(cx, |this, cx| {
                this.target = 1.0;
                this.leaving = Some(Instant::now());
                cx.notify();
            })
            .ok();
            cx.background_executor().timer(FADE).await;
            this.update(cx, |_, cx| cx.emit(SplashEvent::Done)).ok();
        }));
    }

    fn status(&self, cx: &App) -> (SharedString, Option<SharedString>) {
        let player = app::player(cx).read(cx);
        let song = player.current.as_ref().map(|t| {
            let artist = t.artist_line();
            if artist.is_empty() {
                t.name.clone()
            } else {
                format!("{} · {}", t.name, artist).into()
            }
        });
        let text = match player.warmup {
            Warmup::Ready if song.is_some() => "Ready when you are",
            Warmup::Ready => "Ready",
            _ if song.is_some() => "Picking up where you left off",
            Warmup::Idle => "Starting Lilac",
            Warmup::Connecting => "Connecting to Spotify",
            Warmup::Restoring => "Picking up where you left off",
        };
        (text.into(), song)
    }
}

/// The app underneath is ready to use once it shows what to play.
fn can_leave(player: &crate::player::Player) -> bool {
    player.warmup == Warmup::Ready || player.current.is_some()
}

fn stage_progress(warmup: Warmup) -> f32 {
    match warmup {
        Warmup::Idle => 0.12,
        Warmup::Connecting => 0.4,
        Warmup::Restoring => 0.75,
        Warmup::Ready => 1.0,
    }
}

/// Overshoots a little before settling, like a flower opening.
fn ease_out_back(t: f32) -> f32 {
    let c1 = 1.6;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn sprig() -> impl IntoElement {
    let k = SPRIG / 1024.0;
    let tints = [
        theme::white(1.0),
        theme::accent_soft(),
        rgba8_hsla(0xe9ddff),
    ];
    let mut sprig = div().relative().size(px(SPRIG)).child(
        svg()
            .absolute()
            .size(px(SPRIG))
            .path(icons::LILAC_STEM)
            .text_color(theme::accent_soft())
            .with_animation(
                "stem",
                Animation::new(Duration::from_millis(300)).with_easing(ease_in_out),
                |stem, t| stem.opacity(t),
            ),
    );
    // Bloom from the bottom up.
    let order: Vec<usize> = (0..FLORETS.len()).rev().collect();
    let total = BLOOM_START + BLOOM_STAGGER * FLORETS.len() as f32 + BLOOM_GROW;
    for (rank, &ix) in order.iter().enumerate() {
        let (x, y, r) = FLORETS[ix];
        let (x, y, r) = (x * k, y * k, r * k);
        let delay = BLOOM_START + BLOOM_STAGGER * rank as f32;
        let tint = tints[ix % tints.len()];
        let turn = ix as f32 * 0.7;
        let phase = ix as f32 * 0.13;
        let flower = svg()
            .path(icons::LILAC_FLORET)
            .text_color(tint)
            .with_transformation(Transformation::rotate(radians(turn)));
        sprig = sprig.child(
            div()
                .absolute()
                .left(px(x - r))
                .top(px(y - r))
                .size(px(2.0 * r))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .with_animation(
                            ("bloom", ix),
                            Animation::new(Duration::from_secs_f32(total)).with_max_fps(FPS),
                            move |el, t| {
                                let local = ((t * total - delay) / BLOOM_GROW).clamp(0.0, 1.0);
                                let grow = if local == 0.0 {
                                    0.0
                                } else {
                                    ease_out_back(local)
                                };
                                el.size(px(2.0 * r * grow.max(0.0))).opacity(local.min(1.0))
                            },
                        )
                        .child(
                            flower.size_full().with_animation(
                                ("breathe", ix),
                                Animation::new(Duration::from_millis(2800))
                                    .repeat()
                                    .with_max_fps(FPS),
                                move |el, t| {
                                    let s = 1.0 + 0.06 * (TAU * (t + phase)).sin();
                                    el.with_transformation(
                                        Transformation::rotate(radians(turn))
                                            .with_scaling(size(s, s)),
                                    )
                                },
                            ),
                        ),
                ),
        );
    }
    sprig
}

fn rgba8_hsla(hex: u32) -> Hsla {
    theme::rgba8(hex, 1.0).into()
}

impl Render for Splash {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, song) = self.status(cx);
        let leaving = self.leaving.is_some();

        div()
            .id("splash")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .bg(theme::backdrop_opaque())
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(
                div()
                    .relative()
                    .size(px(SPRIG + 120.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    // A soft glow that breathes behind the flower.
                    .child(
                        div()
                            .absolute()
                            .size(px(SPRIG * 0.9))
                            .rounded_full()
                            .bg(theme::accent_alpha(0.10))
                            .with_animation(
                                "glow",
                                Animation::new(Duration::from_millis(3200))
                                    .repeat()
                                    .with_max_fps(FPS),
                                |glow, t| {
                                    let pulse = 0.5 + 0.5 * (TAU * t).sin();
                                    glow.shadow(vec![BoxShadow {
                                        color: theme::accent_alpha(0.25 + 0.2 * pulse),
                                        offset: point(px(0.0), px(0.0)),
                                        blur_radius: px(70.0 + 30.0 * pulse),
                                        spread_radius: px(10.0 + 12.0 * pulse),
                                        inset: false,
                                    }])
                                },
                            ),
                    )
                    .child(sprig()),
            )
            .child(
                div()
                    .text_3xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme::text())
                    .child("Lilac")
                    .with_animation(
                        "wordmark",
                        Animation::new(Duration::from_millis(650)).with_easing(ease_in_out),
                        |el, t| el.opacity(((t - 0.35) / 0.65).clamp(0.0, 1.0)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(180.0))
                            .h(px(3.0))
                            .rounded_full()
                            .bg(theme::white(0.08))
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme::accent())
                                    .with_animation(
                                        "bar",
                                        Animation::new(Duration::from_secs(1))
                                            .repeat()
                                            .with_max_fps(FPS),
                                        {
                                            let (from, target, at) =
                                                (self.from, self.target, self.target_at);
                                            move |el, _| {
                                                let t = (at.elapsed().as_secs_f32() / 0.6).min(1.0);
                                                let eased = 1.0 - (1.0 - t).powi(3);
                                                el.w(relative(from + (target - from) * eased))
                                            }
                                        },
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme::text_muted())
                            .child(status),
                    )
                    // The line is kept when empty so nothing shifts when the song arrives.
                    .child(
                        div()
                            .h(px(16.0))
                            .max_w(px(360.0))
                            .truncate()
                            .text_xs()
                            .text_color(theme::accent_soft())
                            .child(song.unwrap_or_default()),
                    ),
            )
            .map(|el| {
                if leaving {
                    el.with_animation("leave", Animation::new(FADE).with_max_fps(FPS), |el, t| {
                        el.opacity(1.0 - t)
                    })
                    .into_any_element()
                } else {
                    el.into_any_element()
                }
            })
    }
}
