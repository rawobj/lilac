//! The right-hand panel: now playing, queue, devices or friend activity.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::menus::TrackMenuContext;
use crate::model::Track;
use crate::nav::Route;
use crate::player::{Device, DeviceKind, Player};
use crate::theme;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};
use crate::ui_state::RightPanel;

pub struct RightPanelView {
    scroll: ScrollHandle,
    friends_scroll: ScrollHandle,
    shown: u64,
    panel: RightPanel,
}

/// A fingerprint of the player state the panel draws.
fn shown_state(player: &Player) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    player.current.as_ref().map(|t| &t.uri).hash(&mut h);
    (player.playing, player.loading).hash(&mut h);
    player.queue.context_name.hash(&mut h);
    for track in player
        .queue
        .user
        .iter()
        .chain(player.queue.upcoming().take(60))
    {
        track.uri.hash(&mut h);
    }
    for device in &player.devices {
        (&device.id, &device.name).hash(&mut h);
    }
    player.active_device.hash(&mut h);
    player.device_id().hash(&mut h);
    std::mem::discriminant(&player.repeat).hash(&mut h);
    player.remote_device().is_some().hash(&mut h);
    player.queue_editable().hash(&mut h);
    h.finish()
}

impl RightPanelView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // The player changes on every volume step and position report; only
        // re-render when something this panel shows is different.
        cx.observe(&app::player(cx), |this: &mut Self, player, cx| {
            let shown = shown_state(player.read(cx));
            if shown != this.shown {
                this.shown = shown;
                cx.notify();
            }
        })
        .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::ui(cx), |this: &mut Self, ui, cx| {
            let panel = ui.read(cx).right_panel;
            if panel == RightPanel::Devices && this.panel != panel {
                app::player(cx)
                    .update(cx, |p, cx| p.refresh_devices(std::time::Duration::ZERO, cx));
            }
            this.panel = panel;
            cx.notify()
        })
        .detach();
        cx.observe(&app::friends(cx), |_, _, cx| cx.notify())
            .detach();
        Self {
            scroll: ScrollHandle::new(),
            friends_scroll: ScrollHandle::new(),
            shown: 0,
            panel: RightPanel::Hidden,
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
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("now-playing")
                            .size_full()
                            .overflow_y_scroll()
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.scroll)
                            // Room for the scrollbar.
                            .pr(px(8.0))
                            .child(body),
                    )
                    .group("scrollable")
                    .child(ui::vertical_scrollbar(&self.scroll)),
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
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("queue")
                            .size_full()
                            .overflow_y_scroll()
                            .restrict_scroll_to_axis()
                            .track_scroll(&self.scroll)
                            // Room for the scrollbar.
                            .pr(px(8.0))
                            .child(body),
                    )
                    .group("scrollable")
                    .child(ui::vertical_scrollbar(&self.scroll)),
            )
    }

    fn devices(&self, _: &Window, cx: &mut App) -> Div {
        let (devices, active, ours) = {
            let player = app::player(cx);
            let player = player.read(cx);
            (
                player.devices.clone(),
                player.active_device.clone(),
                player.device_id().map(str::to_string),
            )
        };
        // This computer counts as current when nothing else is playing.
        let current = active.clone().or_else(|| ours.clone());
        let row = |ix: usize, device: Option<&Device>| -> Stateful<Div> {
            let (id, name, kind) = match device {
                Some(d) => (Some(d.id.clone()), d.name.clone(), d.kind),
                None => (
                    ours.clone(),
                    "This computer".to_string(),
                    DeviceKind::Computer,
                ),
            };
            let is_current = if device.is_none() && current.is_none() {
                true
            } else {
                id.is_some() && id == current
            };
            let icon = match kind {
                DeviceKind::Computer | DeviceKind::Other => icons::LAPTOP,
                DeviceKind::Phone => icons::SMARTPHONE,
                DeviceKind::Speaker => icons::SPEAKER,
                DeviceKind::Tv => icons::TV,
            };
            let color = if is_current {
                theme::accent()
            } else {
                theme::text()
            };
            div()
                .id(("device", ix))
                .p_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(theme::RADIUS_CARD))
                .when(is_current, |d| d.bg(theme::accent_alpha(0.14)))
                .when(!is_current, |d| {
                    d.cursor_pointer().hover(|s| s.bg(theme::hover_fill()))
                })
                .child(
                    svg()
                        .path(icon)
                        .size(px(24.0))
                        .flex_none()
                        .text_color(color),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color)
                                .child(name),
                        )
                        .when(is_current, |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_muted())
                                    .child("Current device"),
                            )
                        }),
                )
                .when_some(id.filter(|_| !is_current), |d, id| {
                    d.on_click(move |_, _, cx| {
                        let id = id.clone();
                        app::player(cx).update(cx, |player, cx| player.transfer_to(&id, cx));
                    })
                })
        };

        let mut list = div().px_2().flex().flex_col().gap_1();
        let this_device = devices.iter().find(|d| Some(&d.id) == ours.as_ref());
        list = list.child(row(0, this_device));
        let others: Vec<&Device> = devices
            .iter()
            .filter(|d| Some(&d.id) != ours.as_ref())
            .collect();
        if !others.is_empty() {
            list = list.child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child("Select another device"),
            );
            for (ix, device) in others.into_iter().enumerate() {
                list = list.child(row(ix + 1, Some(device)));
            }
        } else if ours.is_none() && !app::session(cx).read(cx).is_demo() {
            list = list.child(
                div()
                    .px_3()
                    .pt_2()
                    .text_xs()
                    .text_color(theme::text_muted())
                    .child("Connecting to Spotify…"),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(Self::header("Connect to a device"))
            .child(list)
    }
}

impl RightPanelView {
    fn friends(&self, window: &Window, cx: &mut App) -> Div {
        use crate::friends::Status;
        let (list, status) = {
            let friends = app::friends(cx);
            let friends = friends.read(cx);
            (friends.list.clone(), friends.status.clone())
        };
        let panel = div()
            .size_full()
            .flex()
            .flex_col()
            .child(Self::header("Friend Activity"));
        let empty = |title: &'static str, detail: SharedString| {
            div()
                .px_4()
                .child(ui::empty_state(icons::USERS, title, detail))
        };
        match status {
            Status::Loading if list.is_empty() => panel.child(ui::loading_state()),
            Status::Unavailable => panel.child(empty(
                "Friend activity unavailable",
                "Spotify only shares it with its own apps for this account.".into(),
            )),
            Status::Failed(err) => panel.child(empty("Couldn't load friend activity", err)),
            _ if list.is_empty() => panel.child(empty(
                "No friend activity yet",
                "Follow friends on Spotify to see what they play.".into(),
            )),
            _ => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let mut rows = div().px_2().flex().flex_col().gap_1();
                for (ix, friend) in list.iter().enumerate() {
                    rows = rows.child(friend_row(ix, friend, now, window, cx));
                }
                panel.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            div()
                                .id("friends-scroll")
                                .size_full()
                                .overflow_y_scroll()
                                .restrict_scroll_to_axis()
                                .track_scroll(&self.friends_scroll)
                                .pr(px(8.0))
                                .child(rows),
                        )
                        .group("scrollable")
                        .child(ui::vertical_scrollbar(&self.friends_scroll)),
                )
            }
        }
    }
}

/// Listening right now, give or take a long track.
const LIVE_MS: u64 = 10 * 60 * 1000;

fn ago(now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    match minutes {
        0 => "now".into(),
        1..=59 => format!("{minutes} min"),
        60..=1439 => format!("{} hr", minutes / 60),
        _ => format!("{} d", minutes / 1440),
    }
}

fn friend_row(
    ix: usize,
    friend: &crate::spotify::buddylist::Friend,
    now_ms: u64,
    window: &Window,
    cx: &mut App,
) -> Stateful<Div> {
    let live = now_ms.saturating_sub(friend.timestamp_ms) < LIVE_MS;
    let open_track = friend.track_uri.clone();
    let open_artist = friend.artist_uri.clone();
    let open_context = friend.context_uri.clone();
    let go = |uri: String| {
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            cx.stop_propagation();
            if let Some(route) = Route::from_link(&uri) {
                app::navigate(route, cx);
            }
        }
    };
    div()
        .id(("friend", ix))
        .p_2()
        .flex()
        .gap_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .hover(|s| s.bg(theme::hover_fill()))
        .child(div().relative().flex_none().child(ui::cover(
            friend.image.clone().map(Into::into),
            40.0,
            20.0,
            Placeholder::User,
            window,
            cx,
        )))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(friend.name.clone()),
                        )
                        .child(if live {
                            svg()
                                .path(icons::AUDIO_LINES)
                                .size(px(14.0))
                                .flex_none()
                                .text_color(theme::accent())
                                .into_any_element()
                        } else {
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme::text_subtle())
                                .child(ago(now_ms, friend.timestamp_ms))
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(
                            div()
                                .id(("friend-track", ix))
                                .truncate()
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(go(open_track))
                                .child(friend.track.clone()),
                        )
                        .child(div().flex_none().px_1().child("•"))
                        .child(
                            div()
                                .id(("friend-artist", ix))
                                .truncate()
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(go(open_artist))
                                .child(friend.artist.clone()),
                        ),
                )
                .child(
                    div()
                        .id(("friend-context", ix))
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .text_color(theme::text_subtle())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::text()))
                        .on_click(go(open_context))
                        .child(
                            svg()
                                .path(icons::MUSIC)
                                .size(px(11.0))
                                .flex_none()
                                .text_color(theme::text_subtle()),
                        )
                        .child(div().truncate().child(friend.context.clone())),
                ),
        )
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
            RightPanel::Friends => self.friends(window, cx),
        }
    }
}
