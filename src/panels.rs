//! The right-hand panel: now playing, queue or devices.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::menus::TrackMenuContext;
use crate::model::Track;
use crate::nav::Route;
use crate::theme;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};
use crate::ui_state::RightPanel;

pub struct RightPanelView {
    scroll: ScrollHandle,
}

impl RightPanelView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.observe(&app::player(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::ui(cx), |_, _, cx| cx.notify()).detach();
        Self {
            scroll: ScrollHandle::new(),
        }
    }

    fn header(title: impl Into<SharedString>) -> Div {
        div()
            .flex_none()
            .h(px(52.0))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .truncate()
                    .font_weight(FontWeight::BOLD)
                    .child(title.into()),
            )
            .child(
                ui::icon_button("close-panel", icons::X).on_click(|_, _, cx| {
                    app::ui(cx).update(cx, |ui, cx| ui.toggle_panel(ui.right_panel, cx))
                }),
            )
    }

    fn now_playing(&self, window: &Window, cx: &mut App) -> Div {
        let player = app::player(cx);
        let (track, context_name, next) = {
            let p = player.read(cx);
            (
                p.current.clone(),
                p.queue.context_name.clone(),
                p.queue.peek(p.repeat).cloned(),
            )
        };
        let Some(track) = track else {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(Self::header("Now playing"))
                .child(ui::empty_state(
                    icons::MUSIC,
                    "Nothing playing",
                    "Pick something to listen to.",
                ));
        };
        let liked = app::library(cx).read(cx).is_liked(&track.id);
        let edge = theme::RIGHT_PANEL_WIDTH - 34.0;
        let artist = track.artists.first().cloned();
        let like_track = track.clone();

        let mut body = div()
            .px_4()
            .pb_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(ui::cover(
                track.cover().for_size(320),
                edge,
                theme::RADIUS_CARD,
                Placeholder::Music,
                window,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child(track.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_color(theme::text_muted())
                                    .child(track.artist_line()),
                            ),
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
                            .on_click(move |_, _, cx| {
                                app::library(cx)
                                    .update(cx, |library, cx| library.toggle_like(&like_track, cx))
                            }),
                        )
                    }),
            );

        if let Some(artist) = artist.filter(|a| !a.id.is_empty()) {
            let id = artist.id.clone();
            body = body.child(
                div()
                    .id("about-artist")
                    .p_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(px(theme::RADIUS_CARD))
                    .cursor_pointer()
                    .bg(theme::glass_fill_strong())
                    .hover(|s| s.bg(theme::active_fill()))
                    .child(
                        div()
                            .size(px(40.0))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(theme::accent_alpha(0.18))
                            .child(
                                svg()
                                    .path(icons::USER)
                                    .size(px(18.0))
                                    .text_color(theme::accent_soft()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("About the artist"),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(artist.name),
                            ),
                    )
                    .child(
                        svg()
                            .path(icons::CHEVRON_RIGHT)
                            .size(px(16.0))
                            .text_color(theme::text_muted()),
                    )
                    .on_click(move |_, _, cx| app::navigate(Route::Artist(id.clone()), cx)),
            );
        }

        if let Some(next) = next {
            body = body.child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(px(theme::RADIUS_CARD))
                    .bg(theme::glass_fill_strong())
                    .child(
                        div()
                            .px_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Next in queue"),
                            )
                            .child(
                                div()
                                    .id("open-queue")
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme::text()))
                                    .child("Open queue")
                                    .on_click(|_, _, cx| {
                                        app::ui(cx).update(cx, |ui, cx| {
                                            ui.toggle_panel(RightPanel::Queue, cx)
                                        })
                                    }),
                            ),
                    )
                    .child(
                        compact_row("next", &next, window, cx)
                            .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.next(cx))),
                    ),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(Self::header(if context_name.is_empty() {
                "Now playing".into()
            } else {
                context_name
            }))
            .child(
                div()
                    .id("now-playing")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }

    fn queue(&self, window: &Window, cx: &mut App) -> Div {
        let player = app::player(cx);
        let (current, user, upcoming, context_name) = {
            let p = player.read(cx);
            (
                p.current.clone(),
                p.queue.user.iter().cloned().collect::<Vec<Track>>(),
                // Enough to fill the panel; the full context can be thousands long.
                p.queue.upcoming().take(60).cloned().collect::<Vec<Track>>(),
                p.queue.context_name.clone(),
            )
        };

        let label = |text: SharedString| {
            div()
                .px_4()
                .pt_3()
                .pb_1()
                .text_sm()
                .font_weight(FontWeight::BOLD)
                .child(text)
        };
        let mut body = div().pb_4().flex().flex_col();

        if let Some(current) = &current {
            body = body.child(label("Now playing".into())).child(track_row(
                "current",
                current,
                RowOptions {
                    show_cover: true,
                    ..Default::default()
                },
                |_, cx| app::player(cx).update(cx, |p, cx| p.toggle(cx)),
                window,
                cx,
            ));
        }
        if !user.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(label("Next in queue".into()))
                    .child(
                        div()
                            .id("clear")
                            .px_4()
                            .pb_1()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::text()))
                            .child("Clear queue")
                            .on_click(|_, _, cx| {
                                app::player(cx).update(cx, |p, cx| p.clear_queue(cx))
                            }),
                    ),
            );
            for (ix, track) in user.iter().enumerate() {
                body = body.child(track_row(
                    ("queued", ix),
                    track,
                    RowOptions {
                        show_cover: true,
                        menu: TrackMenuContext {
                            queue_index: Some(ix),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    move |_, cx| app::player(cx).update(cx, |p, cx| p.skip_to_queued(ix, cx)),
                    window,
                    cx,
                ));
            }
        }
        if !upcoming.is_empty() {
            let title = if context_name.is_empty() {
                "Next up".to_string()
            } else {
                format!("Next from: {context_name}")
            };
            body = body.child(label(title.into()));
            for (ix, track) in upcoming.iter().enumerate() {
                body = body.child(track_row(
                    ("upcoming", ix),
                    track,
                    RowOptions {
                        show_cover: true,
                        ..Default::default()
                    },
                    move |_, cx| app::player(cx).update(cx, |p, cx| p.skip_to_upcoming(ix, cx)),
                    window,
                    cx,
                ));
            }
        }
        if current.is_none() && user.is_empty() && upcoming.is_empty() {
            body = body.child(ui::empty_state(
                icons::LIST_MUSIC,
                "Your queue is empty",
                "Add songs with “Add to queue”.",
            ));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(Self::header("Queue"))
            .child(
                div()
                    .id("queue")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(body),
            )
    }

    fn devices(&self, _: &Window, _: &mut App) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(Self::header("Connect to a device"))
            .child(
                div().px_4().child(
                    div()
                        .p_4()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(px(theme::RADIUS_CARD))
                        .bg(theme::accent_alpha(0.14))
                        .child(
                            svg()
                                .path(icons::LAPTOP)
                                .size(px(28.0))
                                .text_color(theme::accent()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_color(theme::accent())
                                        .font_weight(FontWeight::BOLD)
                                        .child("This computer"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::text_muted())
                                        .child("Current device"),
                                ),
                        ),
                ),
            )
    }
}

/// A small non-interactive track summary.
fn compact_row(id: &'static str, track: &Track, window: &Window, cx: &mut App) -> Stateful<Div> {
    div()
        .id(id)
        .p_1()
        .flex()
        .items_center()
        .gap_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .cursor_pointer()
        .hover(|s| s.bg(theme::hover_fill()))
        .child(ui::cover(
            track.cover().for_size(44),
            44.0,
            6.0,
            Placeholder::Music,
            window,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(track.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(track.artist_line()),
                ),
        )
}

impl Render for RightPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match app::ui(cx).read(cx).right_panel {
            RightPanel::Hidden => div(),
            RightPanel::NowPlaying => self.now_playing(window, cx),
            RightPanel::Queue => self.queue(window, cx),
            RightPanel::Devices => self.devices(window, cx),
        }
    }
}
