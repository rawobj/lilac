//! Preferences.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::scroll_page;
use crate::app;
use crate::images::ImageCache;
use crate::settings::{Quality, Settings};
use crate::theme;
use crate::ui::{self, Placeholder};
use crate::ui_state::Dialog;

pub struct SettingsPage {
    scroll: ScrollHandle,
}

impl SettingsPage {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&app::settings(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::session(cx), |_, _, cx| cx.notify())
            .detach();
        Self {
            scroll: ScrollHandle::new(),
        }
    }
}

fn section(title: &'static str) -> Div {
    div().flex().flex_col().gap_1().child(
        div()
            .pb_1()
            .text_lg()
            .font_weight(FontWeight::BOLD)
            .child(title),
    )
}

fn row(title: &'static str, detail: &'static str, control: impl IntoElement) -> Div {
    div()
        .min_h(px(56.0))
        .py_2()
        .flex()
        .items_center()
        .gap_6()
        .border_b_1()
        .border_color(theme::glass_border())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(div().child(title))
                .when(!detail.is_empty(), |d| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme::text_muted())
                            .child(detail),
                    )
                }),
        )
        .child(control)
}

/// An on/off switch bound to one boolean setting.
fn switch(id: &'static str, on: bool, set: fn(&mut Settings, bool)) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(42.0))
        .h(px(24.0))
        .p(px(3.0))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if on {
            theme::accent()
        } else {
            theme::white(0.18)
        })
        .when(on, |d| d.justify_end())
        .child(div().size(px(18.0)).rounded_full().bg(theme::white(0.97)))
        .on_click(move |_, _, cx| app::update_settings(cx, |s| set(s, !on)))
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = app::settings(cx).read(cx).clone();
        let user = app::session(cx).read(cx).user.clone();
        let cache_mb = ImageCache::used_bytes(cx) as f32 / (1024.0 * 1024.0);

        let account = section("Account").children(user.map(|user| {
            let plan = match user.product.as_deref() {
                Some("premium") => "Spotify Premium",
                Some("free") | Some("open") => "Spotify Free",
                _ => "Spotify",
            };
            div()
                .py_2()
                .flex()
                .items_center()
                .gap_4()
                .child(ui::cover(
                    user.images.for_size(56),
                    56.0,
                    28.0,
                    Placeholder::User,
                    window,
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(user.name()))
                        .child(div().text_xs().text_color(theme::text_muted()).child(plan)),
                )
                .child(
                    ui::pill_button("logout", "Log out", false).on_click(|_, _, cx| {
                        app::session(cx).update(cx, |session, cx| session.logout(cx))
                    }),
                )
        }));

        let mut quality = div().flex().gap_2();
        for option in Quality::ALL {
            quality = quality.child(
                ui::chip(option.label(), option.label(), settings.quality == option)
                    .on_click(move |_, _, cx| app::update_settings(cx, |s| s.quality = option)),
            );
        }

        let audio = section("Audio")
            .child(row(
                "Streaming quality",
                "Applies from the next track.",
                quality,
            ))
            .child(row(
                "Normalize volume",
                "Play every track at a similar loudness.",
                switch("normalize", settings.normalize, |s, v| s.normalize = v),
            ))
            .child(row(
                "Gapless playback",
                "Load the next track early so albums play without pauses.",
                switch("gapless", settings.gapless, |s, v| s.gapless = v),
            ));

        let display = section("Display")
            .child(row(
                "Reduce transparency",
                "Use an opaque window background instead of the blurred glass.",
                switch("opaque", settings.reduce_transparency, |s, v| {
                    s.reduce_transparency = v
                }),
            ))
            .child(row(
                "Show Now Playing panel on start",
                "",
                switch("now-playing", settings.show_now_playing, |s, v| {
                    s.show_now_playing = v
                }),
            ));

        let integration = section("Integration")
            .child(row(
                "Media keys and desktop controls",
                "Expose playback to the desktop via MPRIS. Takes effect after a restart.",
                switch("media-keys", settings.media_keys, |s, v| s.media_keys = v),
            ))
            .child(row(
                "Community lyrics",
                "When Spotify has no lyrics, look them up on lrclib.net. Sends the song title and artist to that service.",
                switch("lrclib", settings.lrclib_lyrics, |s, v| s.lrclib_lyrics = v),
            ));

        let storage = section("Storage").child(row(
            "Artwork cache",
            "Decoded cover art held in memory. It is trimmed automatically.",
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_color(theme::text_muted())
                        .child(format!("{cache_mb:.1} MB")),
                )
                .child(
                    ui::pill_button("clear-cache", "Clear", false).on_click(|_, _, cx| {
                        ImageCache::clear(cx);
                        cx.refresh_windows();
                        app::toast("Artwork cache cleared", cx);
                    }),
                ),
        ));

        let about = section("About")
            .child(row(
                "Keyboard shortcuts",
                "",
                ui::pill_button("shortcuts", "View", false).on_click(|_, _, cx| {
                    app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::Shortcuts, cx))
                }),
            ))
            .child(row(
                "Lilac",
                concat!("Version ", env!("CARGO_PKG_VERSION")),
                ui::pill_button("about", "About", false).on_click(|_, _, cx| {
                    app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::About, cx))
                }),
            ));

        scroll_page(
            "settings",
            &self.scroll,
            div()
                .max_w(px(820.0))
                .mx_auto()
                .p_8()
                .flex()
                .flex_col()
                .gap_8()
                .child(
                    div()
                        .text_3xl()
                        .font_weight(FontWeight::BOLD)
                        .child("Settings"),
                )
                .child(account)
                .child(audio)
                .child(display)
                .child(integration)
                .child(storage)
                .child(about),
        )
    }
}
