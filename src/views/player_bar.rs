//! The transport bar along the bottom of the window.

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::model::format_duration;
use crate::nav::Route;
use crate::player::Repeat;
use crate::theme;
use crate::ui::slider::{SliderChange, slider};
use crate::ui::{self, Placeholder, WithTooltip, icons};
use crate::ui_state::RightPanel;

pub struct PlayerBar {
    /// Seek position previewed while the thumb is dragged, as a fraction.
    scrub: Option<f32>,
    clock: Option<Task<()>>,
}

impl PlayerBar {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe(&app::player(cx), |this: &mut Self, _, cx| {
            this.sync_clock(cx);
            cx.notify();
        })
        .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::ui(cx), |_, _, cx| cx.notify()).detach();
        cx.observe(&app::nav(cx), |_, _, cx| cx.notify()).detach();
        Self {
            scrub: None,
            clock: None,
        }
    }

    /// Repaint twice a second while playing, and not at all while paused.
    /// Ticks land on the track's own second boundaries, so the elapsed time
    /// turns over when the audio does rather than up to half a second late.
    fn sync_clock(&mut self, cx: &mut Context<Self>) {
        let playing = app::player(cx).read(cx).playing;
        if !playing {
            self.clock = None;
        } else if self.clock.is_none() {
            self.clock = Some(cx.spawn(async move |this, cx| {
                loop {
                    let Ok(position) =
                        this.update(cx, |_, cx| app::player(cx).read(cx).position_ms())
                    else {
                        break;
                    };
                    let mut wait = 1000 - position % 1000;
                    if wait > 500 {
                        wait -= 500;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(wait as u64 + 5))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }

    fn now_playing(&self, window: &Window, cx: &mut App) -> Div {
        let Some(track) = app::player(cx).read(cx).current.clone() else {
            return div().flex_1();
        };
        let liked = app::library(cx).read(cx).is_liked(&track.id);
        // Soloist plays for this computer; it is not "elsewhere".
        let soloist = app::soloist(cx).read(cx);
        let listening_on = app::player(cx)
            .read(cx)
            .remote_device()
            .filter(|d| !soloist.is_device(&d.id))
            .map(|d| d.name.clone());
        let album_id = track
            .album
            .as_ref()
            .map(|a| a.id.clone())
            .filter(|id| !id.is_empty());
        let show_id = track
            .show
            .as_ref()
            .map(|s| s.id.clone())
            .filter(|id| !id.is_empty());

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("cover")
                    .cursor_pointer()
                    .child(ui::cover(
                        track.cover().for_size(56),
                        56.0,
                        8.0,
                        Placeholder::Music,
                        window,
                        cx,
                    ))
                    .on_click(|_, _, cx| {
                        app::ui(cx).update(cx, |ui, cx| ui.toggle_panel(RightPanel::NowPlaying, cx))
                    }),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .id("title")
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::accent_soft()))
                            .child(track.name.clone())
                            .on_click(move |_, _, cx| {
                                if let Some(id) = album_id.clone() {
                                    app::navigate(Route::Album(id), cx);
                                } else if let Some(id) = show_id.clone() {
                                    app::navigate(Route::Show(id), cx);
                                }
                            }),
                    )
                    .child(
                        ui::artist_line_links("bar", &track, theme::text_muted(), theme::text())
                            .text_xs(),
                    )
                    .children(listening_on.map(|name| {
                        div()
                            .id("listening-on")
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .child(
                                svg()
                                    .path(icons::DEVICES)
                                    .size(px(12.0))
                                    .text_color(theme::accent()),
                            )
                            .child(div().truncate().child(format!("Listening on {name}")))
                            .on_click(|_, _, cx| {
                                app::ui(cx)
                                    .update(cx, |ui, cx| ui.toggle_panel(RightPanel::Devices, cx))
                            })
                    })),
            )
            .when(!track.is_episode() && !track.is_local, |d| {
                d.child(
                    ui::toggle_icon_button(
                        "like",
                        if liked {
                            icons::HEART_FILL
                        } else {
                            icons::HEART
                        },
                        liked,
                    )
                    .tip(if liked {
                        "Remove from Liked Songs"
                    } else {
                        "Save to Liked Songs"
                    })
                    .on_click(move |_, _, cx| {
                        app::library(cx).update(cx, |library, cx| library.toggle_like(&track, cx))
                    }),
                )
            })
    }

    fn transport(&mut self, cx: &mut Context<Self>) -> Div {
        let player = app::player(cx);
        let (playing, loading, shuffle, repeat, position, duration, has_track) = {
            let p = player.read(cx);
            (
                p.playing,
                p.loading,
                p.shuffle(),
                p.repeat,
                p.position_ms(),
                p.duration_ms(),
                p.current.is_some(),
            )
        };

        let shown = match self.scrub {
            Some(fraction) => (fraction * duration as f32) as u32,
            None => position,
        };
        let fraction = if duration == 0 {
            0.0
        } else {
            shown as f32 / duration as f32
        };

        let buttons = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                ui::toggle_icon_button("shuffle", icons::SHUFFLE, shuffle)
                    .tip(if shuffle {
                        "Disable shuffle"
                    } else {
                        "Enable shuffle"
                    })
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.toggle_shuffle(cx))),
            )
            .child(
                ui::icon_button("previous", icons::SKIP_BACK)
                    .tip("Previous")
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.previous(cx))),
            )
            .child(
                div()
                    .id("play")
                    .size(px(36.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(theme::text())
                    .when(!has_track, |d| d.opacity(0.5))
                    .hover(|s| s.bg(theme::accent_soft()))
                    .active(|s| s.opacity(0.85))
                    .child(if loading {
                        ui::spinner(16.0).into_any_element()
                    } else {
                        svg()
                            .path(if playing { icons::PAUSE } else { icons::PLAY })
                            .size(px(16.0))
                            .text_color(theme::backdrop_opaque())
                            .into_any_element()
                    })
                    .tip(if playing { "Pause" } else { "Play" })
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.toggle(cx))),
            )
            .child(
                ui::icon_button("next", icons::SKIP_FORWARD)
                    .tip("Next")
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.next(cx))),
            )
            .child(
                ui::toggle_icon_button(
                    "repeat",
                    if repeat == Repeat::Track {
                        icons::REPEAT_ONE
                    } else {
                        icons::REPEAT
                    },
                    repeat != Repeat::Off,
                )
                .tip(match repeat {
                    Repeat::Off => "Enable repeat",
                    Repeat::Context => "Enable repeat one",
                    Repeat::Track => "Disable repeat",
                })
                .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.cycle_repeat(cx))),
            );

        let time = |ms: u32| {
            div()
                .flex_none()
                .w(px(40.0))
                .text_xs()
                .text_color(theme::text_muted())
                .child(format_duration(ms))
        };

        let seek = div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .child(time(shown).flex().justify_end())
            .child(
                div().flex_1().child(slider(
                    "seek",
                    fraction,
                    cx.listener(|this, change: &SliderChange, _, cx| match *change {
                        SliderChange::Preview(value) => {
                            this.scrub = Some(value);
                            cx.notify();
                        }
                        SliderChange::Commit(value) => {
                            this.scrub = None;
                            app::player(cx).update(cx, |p, cx| {
                                let target = (value * p.duration_ms() as f32) as u32;
                                p.seek(target, cx)
                            });
                        }
                    })
                    .pipe(|listener| {
                        move |change, window: &mut Window, cx: &mut App| {
                            listener(&change, window, cx)
                        }
                    }),
                )),
            )
            .child(time(duration));

        div()
            .flex_none()
            .w(px(520.0))
            .max_w(relative(0.42))
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(buttons)
            .child(seek)
    }

    fn extras(&self, cx: &mut App) -> Div {
        let (volume, muted) = {
            let player = app::player(cx);
            let p = player.read(cx);
            (p.volume, p.muted)
        };
        let panel = app::ui(cx).read(cx).right_panel;
        let lyrics_open = *app::nav(cx).read(cx).current() == Route::Lyrics;
        let level = if muted { 0.0 } else { volume };
        let volume_icon = if level == 0.0 {
            icons::VOLUME_MUTE
        } else if level < 0.34 {
            icons::VOLUME_LOW
        } else if level < 0.67 {
            icons::VOLUME_MID
        } else {
            icons::VOLUME_HIGH
        };

        let panel_toggle =
            |id: &'static str, icon: &'static str, tip: &'static str, which: RightPanel| {
                ui::toggle_icon_button(id, icon, panel == which)
                    .tip(tip)
                    .on_click(move |_, _, cx| {
                        app::ui(cx).update(cx, |ui, cx| ui.toggle_panel(which, cx))
                    })
            };

        div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .child(panel_toggle(
                "now-playing",
                icons::PANEL_RIGHT,
                "Now playing view",
                RightPanel::NowPlaying,
            ))
            .child(
                ui::toggle_icon_button("lyrics", icons::MIC, lyrics_open)
                    .tip("Lyrics")
                    .on_click(|_, _, cx| {
                        app::nav(cx).update(cx, |nav, cx| nav.toggle(Route::Lyrics, cx))
                    }),
            )
            .child(panel_toggle(
                "queue",
                icons::LIST_MUSIC,
                "Queue",
                RightPanel::Queue,
            ))
            .child(panel_toggle(
                "devices",
                icons::DEVICES,
                "Connect to a device",
                RightPanel::Devices,
            ))
            .child(
                ui::icon_button("mute", volume_icon)
                    .tip(if muted { "Unmute" } else { "Mute" })
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.toggle_mute(cx))),
            )
            .child(
                div()
                    .w(px(96.0))
                    .child(slider("volume", level, |change, _, cx| {
                        let (SliderChange::Preview(value) | SliderChange::Commit(value)) = change;
                        app::player(cx).update(cx, |p, cx| p.set_volume(value, cx))
                    })),
            )
            .child(
                ui::icon_button("immersive", icons::MAXIMIZE)
                    .tip("Immersive mode")
                    .on_click(|_, _, cx| app::ui(cx).update(cx, |ui, cx| ui.toggle_immersive(cx))),
            )
    }
}

/// Tiny helper so a value can be adapted inline.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}

impl<T> Pipe for T {}

impl Render for PlayerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let transport = self.transport(cx);
        div()
            .size_full()
            .px_4()
            .flex()
            .items_center()
            .gap_4()
            .child(self.now_playing(window, cx))
            .child(transport)
            .child(self.extras(cx))
    }
}
