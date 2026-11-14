//! The track row used by every list: playlists, albums, search, queue.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{Placeholder, cover, explicit_badge, icons};
use crate::app;
use crate::menus::{self, TrackMenuContext};
use crate::model::{Track, format_duration};
use crate::nav::Route;
use crate::theme;

pub const ROW_HEIGHT: f32 = 56.0;

#[derive(Clone, Default)]
pub struct RowOptions {
    /// Number shown in the leading column. `None` hides the column.
    pub number: Option<u32>,
    pub show_cover: bool,
    pub show_album: bool,
    pub menu: TrackMenuContext,
}

/// One track row. `on_play` runs on double click or on the leading play button.
pub fn track_row(
    id: impl Into<ElementId>,
    track: &Track,
    options: RowOptions,
    on_play: impl Fn(&mut Window, &mut App) + 'static,
    window: &Window,
    cx: &mut App,
) -> Stateful<Div> {
    let (is_current, is_playing) = {
        let player = app::player(cx);
        let player = player.read(cx);
        let current = player.is_current(&track.id);
        (current, current && player.playing)
    };
    let liked = app::library(cx).read(cx).is_liked(&track.id);
    let playable = track.playable();
    let on_play = Rc::new(on_play);

    let leading = options.number.map(|number| {
        let glyph = if is_playing {
            icons::PAUSE
        } else {
            icons::PLAY
        };
        div()
            .id("lead")
            .relative()
            .flex_none()
            .w(px(28.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(if is_playing {
                svg()
                    .path(icons::AUDIO_LINES)
                    .size(px(16.0))
                    .text_color(theme::accent())
                    .group_hover("row", |s| s.opacity(0.0))
                    .into_any_element()
            } else {
                div()
                    .text_sm()
                    .text_color(if is_current {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    })
                    .group_hover("row", |s| s.opacity(0.0))
                    .child(number.to_string())
                    .into_any_element()
            })
            .child(
                svg()
                    .path(glyph)
                    .absolute()
                    .size(px(14.0))
                    .text_color(theme::text())
                    .opacity(0.0)
                    .group_hover("row", |s| s.opacity(1.0)),
            )
            .on_click({
                let on_play = on_play.clone();
                move |_, window, cx| {
                    cx.stop_propagation();
                    if is_current {
                        app::player(cx).update(cx, |player, cx| player.toggle(cx));
                    } else {
                        on_play(window, cx);
                    }
                }
            })
    });

    let title = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(2.0))
        .child(
            div()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if is_current {
                    theme::accent()
                } else {
                    theme::text()
                })
                .child(track.name.clone()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .when(track.explicit, |d| d.child(explicit_badge()))
                .child(
                    super::artist_line_links(
                        format!("row-{}", track.uri),
                        track,
                        theme::text_muted(),
                        theme::text(),
                    )
                    .text_xs(),
                ),
        );

    // The album column is the first thing to go when the page gets narrow.
    let roomy = crate::views::layout(window, cx).main >= 720.0;
    let album = (options.show_album && roomy).then(|| {
        let album = track.album.clone();
        let label = album.as_ref().map(|a| a.name.clone()).unwrap_or_default();
        div()
            .id("album")
            .flex_1()
            .min_w_0()
            .truncate()
            .text_sm()
            .text_color(theme::text_muted())
            .hover(|s| s.text_color(theme::text()))
            .child(label)
            .on_click(move |_, _, cx| {
                if let Some(album) = album.as_ref().filter(|a| !a.id.is_empty()) {
                    cx.stop_propagation();
                    app::navigate(Route::Album(album.id.clone()), cx);
                }
            })
    });

    let heart = (!track.is_local && !track.is_episode()).then(|| {
        let t = track.clone();
        div()
            .id("like")
            .flex_none()
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .when(!liked, |d| {
                d.opacity(0.0).group_hover("row", |s| s.opacity(1.0))
            })
            .hover(|s| s.bg(theme::hover_fill()))
            .child(
                svg()
                    .path(if liked {
                        icons::HEART_FILL
                    } else {
                        icons::HEART
                    })
                    .size(px(15.0))
                    .text_color(if liked {
                        theme::accent()
                    } else {
                        theme::text_muted()
                    }),
            )
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                app::library(cx).update(cx, |library, cx| library.toggle_like(&t, cx));
            })
    });

    let more = {
        let (t, menu) = (track.clone(), options.menu.clone());
        div()
            .id("more")
            .flex_none()
            .size(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .opacity(0.0)
            .group_hover("row", |s| s.opacity(1.0))
            .hover(|s| s.bg(theme::hover_fill()))
            .child(
                svg()
                    .path(icons::ELLIPSIS)
                    .size(px(16.0))
                    .text_color(theme::text_muted()),
            )
            .on_click(move |event, _, cx| {
                cx.stop_propagation();
                menus::track_menu(&t, menu.clone(), event.position(), cx);
            })
    };

    let (t, menu) = (track.clone(), options.menu.clone());
    div()
        .id(id)
        .group("row")
        .h(px(ROW_HEIGHT))
        .mx_3()
        .px_3()
        .flex()
        .items_center()
        .gap_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .when(!playable, |d| d.opacity(0.4))
        .when(is_current, |d| d.bg(theme::accent_alpha(0.08)))
        .hover(|s| s.bg(theme::hover_fill()))
        .children(leading)
        .when(options.show_cover, |d| {
            d.child(cover(
                track.cover().for_size(40),
                40.0,
                6.0,
                Placeholder::Music,
                window,
                cx,
            ))
        })
        .child(title)
        .children(album)
        .children(heart)
        .child(
            div()
                .flex_none()
                .w(px(44.0))
                .text_xs()
                .text_color(theme::text_muted())
                .flex()
                .justify_end()
                .child(format_duration(track.duration_ms)),
        )
        .child(more)
        .on_click(move |event, window, cx| {
            if playable && event.click_count() >= 2 {
                on_play(window, cx);
            }
        })
        .when(playable && !is_current, |row| {
            // The first click of a double click starts the fetch; the second
            // arrives a few hundred milliseconds later to a track half loaded.
            let uri = track.uri.clone();
            row.on_mouse_down(MouseButton::Left, move |event, _, cx| {
                if event.click_count == 1 {
                    app::player(cx).update(cx, |p, _| p.prefetch_track(&uri));
                }
            })
        })
        .when(playable && !is_current, |row| {
            let uri = track.uri.clone();
            row.on_hover(move |hovered, _, cx| {
                let uri = uri.clone();
                app::player(cx).update(cx, |p, cx| p.hover_track(hovered.then_some(uri), cx));
            })
        })
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            menus::track_menu(&t, menu.clone(), event.position, cx);
        })
}

/// Column header shown above a track list.
pub fn track_header(show_album: bool, window: &Window, cx: &App) -> Div {
    let show_album = show_album && crate::views::layout(window, cx).main >= 720.0;
    div()
        .h(px(36.0))
        .mx_3()
        .px_3()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(theme::glass_border())
        .text_xs()
        .text_color(theme::text_subtle())
        .child(
            div()
                .flex_none()
                .w(px(28.0))
                .flex()
                .justify_center()
                .child("#"),
        )
        .child(div().flex_1().child("Title"))
        .when(show_album, |d| d.child(div().flex_1().child("Album")))
        .child(div().flex_none().w(px(28.0)))
        .child(
            div().flex_none().w(px(44.0)).flex().justify_end().child(
                svg()
                    .path(icons::CLOCK)
                    .size(px(14.0))
                    .text_color(theme::text_subtle()),
            ),
        )
        .child(div().flex_none().w(px(28.0)))
}
