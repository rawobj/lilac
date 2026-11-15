//! The loading screen shown while the player warms up after login.
//!
//! It stays until the song Lilac was left on is in the player bar, which is
//! usually as soon as the opening animation has played: the track is read
//! from disk at launch. Loading it into the streaming session and buffering
//! it carries on underneath (see `Player::restore_session`); a Play pressed
//! before that is done starts the song when it is.

use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::player::Warmup;
use crate::theme;
use crate::ui::icons;

/// Long enough for the opening animation to finish.
const MIN_SHOWN: Duration = Duration::from_millis(1100);
/// Never longer than this, whatever the network does.
const MAX_SHOWN: Duration = Duration::from_millis(4000);
const FADE: Duration = Duration::from_millis(280);
/// The badge pops in, then its petals turn into place.
const POP: Duration = Duration::from_millis(520);
const UNFOLD: Duration = Duration::from_millis(900);
/// Animations do not need the display's full refresh rate.
const FPS: f32 = 60.0;

/// Edge of the app badge, in pixels.
const BADGE: f32 = 112.0;

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

fn badge() -> impl IntoElement {
    div()
        .size(px(BADGE))
        .rounded(px(BADGE * 0.3))
        .flex()
        .items_center()
        .justify_center()
        .bg(linear_gradient(
            150.0,
            linear_color_stop(theme::accent_soft(), 0.0),
            linear_color_stop(theme::accent_strong(), 1.0),
        ))
        .with_animation("pop", Animation::new(POP).with_max_fps(FPS), |el, t| {
            let s = ease_out_back(t);
            el.size(px(BADGE * s.max(0.0)))
                .rounded(px(BADGE * 0.3 * s.max(0.0)))
                .opacity(t.min(1.0))
        })
        .child(
            svg()
                .path(icons::LILAC)
                .size(px(BADGE * 0.5))
                .text_color(theme::backdrop_opaque())
                .with_animation(
                    "unfold",
                    Animation::new(UNFOLD).with_max_fps(FPS),
                    |el, t| {
                        // Petals turn a quarter round into place.
                        let turn = (1.0 - ease_out_back(t)) * TAU / 4.0;
                        el.with_transformation(Transformation::rotate(radians(turn)))
                    },
                ),
        )
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
                    .size(px(BADGE + 160.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    // A soft glow that breathes behind the badge.
                    .child(
                        div()
                            .absolute()
                            .size(px(BADGE * 1.1))
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
                    .child(badge()),
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
