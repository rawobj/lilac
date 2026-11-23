//! Preferences.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::scroll_page;
use crate::app;
use crate::images::ImageCache;
use crate::settings::{LyricsStyle, Settings};
use crate::soloist::{self, Install, Process, build};
use crate::theme;
use crate::ui::{self, Placeholder};
use crate::ui_state::Dialog;
use gpui_kit::component::input::{Input, InputState};

pub struct SettingsPage {
    scroll: ScrollHandle,
    api_key: Entity<InputState>,
}

impl SettingsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&app::settings(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::session(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::soloist(cx), |_, _, cx| cx.notify())
            .detach();
        let stored = app::soloist(cx).read(cx).store.api_key.clone();
        let api_key = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste your Soloist API key");
            if let Some(key) = stored {
                state.set_value(key, window, cx);
            }
            state
        });
        Self {
            scroll: ScrollHandle::new(),
            api_key,
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

fn row(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    control: impl IntoElement,
) -> Div {
    let (title, detail) = (title.into(), detail.into());
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

        let engine = if app::session(cx).read(cx).is_free() {
            section("Playback").child(row(
                "Previews",
                "Spotify Free plays each song's 30-second preview here. Full songs need Spotify Premium, played through Soloist.",
                div(),
            ))
        } else {
            playback_engine(&settings, &self.api_key, cx)
        };

        let mut lyrics_styles = div().flex().gap_2();
        for option in LyricsStyle::ALL {
            lyrics_styles = lyrics_styles.child(
                ui::chip(
                    option.label(),
                    option.label(),
                    settings.lyrics_style == option,
                )
                .on_click(move |_, _, cx| app::update_settings(cx, |s| s.lyrics_style = option)),
            );
        }
        let lyrics_detail = match settings.lyrics_style {
            LyricsStyle::Simple => "A calm tinted page with the lines, nothing moving.",
            LyricsStyle::Vivid => "The album art blurred behind glass, the sung line glowing.",
        };
        let display = section("Display")
            .child(row(
                "Reduce transparency",
                "Use an opaque window background instead of the blurred glass.",
                switch("opaque", settings.reduce_transparency, |s, v| {
                    s.reduce_transparency = v
                }),
            ))
            .when(cfg!(target_os = "linux"), |section| {
                section.child(row(
                    "Hardware acceleration",
                    "Draw the window on the graphics card. Turn it off if Lilac shows a blank or glitching window; drawing on the processor uses more CPU. Takes effect after a restart.",
                    switch("hardware-acceleration", settings.hardware_acceleration, |s, v| {
                        s.hardware_acceleration = v
                    }),
                ))
            })
            .child(row("Lyrics", lyrics_detail, lyrics_styles))
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
                "When Spotify has no lyrics, look them up on LRCLIB and NetEase Cloud Music. Sends the song title and artist to those services.",
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

        let help = section("Help & diagnostics")
            .child(row(
                "Test report",
                "Checks what Lilac needs from Spotify on this computer and account, and saves the results to your Downloads folder to send back. Anonymous: no account name, email or password.",
                ui::pill_button("test-report", "Create", true)
                    .on_click(|_, _, cx| crate::selftest::run(cx)),
            ))
            .child(row(
                "Crash reports",
                "Saved automatically when Lilac stops on an error, with its recent log.",
                ui::pill_button("crash-reports", "Open folder", false).on_click(|_, _, cx| {
                    let dir = crate::diagnostics::crashes_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    if open::that_detached(&dir).is_err() {
                        app::toast_error("Could not open the folder", cx);
                    }
                }),
            ))
            .child(row(
                "Log",
                "What Lilac and its libraries reported recently.",
                ui::pill_button("log-folder", "Open folder", false).on_click(|_, _, cx| {
                    let dir = crate::diagnostics::logs_dir();
                    if open::that_detached(&dir).is_err() {
                        app::toast_error("Could not open the folder", cx);
                    }
                }),
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
                .child(engine)
                .child(display)
                .child(integration)
                .child(storage)
                .child(help)
                .child(about),
        )
    }
}

/// Everything about Soloist, which plays for Lilac.
fn playback_engine(settings: &Settings, api_key: &Entity<InputState>, cx: &App) -> Div {
    let soloist = app::soloist(cx).read(cx);
    let now = soloist::now();

    // What is installed, and what happens next.
    let busy = matches!(
        soloist.install,
        Install::Downloading(..) | Install::Unpacking
    );
    let status: SharedString = match (&soloist.install, soloist.installed()) {
        (Install::Downloading(got, Some(total)), _) if *total > 0 => {
            format!("Downloading from Spotify… {}%", got * 100 / total).into()
        }
        (Install::Downloading(got, _), _) => {
            format!("Downloading from Spotify… {:.1} MB", *got as f64 / 1e6).into()
        }
        (Install::Unpacking, _) => "Installing…".into(),
        (_, Some(b)) if b.expired(now) => format!(
            "{} expired on {}. Download a newer build to keep using it.",
            b.version,
            soloist::date(b.expires_at())
        )
        .into(),
        (_, Some(b)) => format!(
            "{} · built {} · runs until {} ({} days left)",
            b.version,
            soloist::date(b.built_at),
            soloist::date(b.expires_at()),
            b.days_left(now)
        )
        .into(),
        (_, None) => "Not installed. Spotify's own headless player, which plays for Lilac; Lilac downloads it from Spotify's servers (soloist-builds.spotifycdn.com) when you ask, under Spotify's terms.".into(),
    };
    let failed = match &soloist.install {
        Install::Failed(why) => Some(format!("Download failed: {why}")),
        _ => None,
    };
    let install_button = if soloist.installed().is_none() {
        ui::pill_button("soloist-install", "Set up", true)
            .on_click(|_, _, cx| soloist::Soloist::open_setup(cx))
    } else if soloist.expired() || soloist.store.newer.is_some() {
        ui::pill_button("soloist-install", "Update", soloist.expired())
            .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.install(cx)))
    } else {
        ui::pill_button("soloist-install", "Check for updates", false)
            .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.check_for_update(true, cx)))
    };
    let install_controls = div()
        .flex()
        .gap_2()
        .when(!busy, |d| d.child(install_button))
        .when(soloist.installed().is_some() && !busy, |d| {
            d.child(
                ui::pill_button("soloist-remove", "Remove", false)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.remove(cx))),
            )
        });

    let updates: SharedString = match soloist.installed() {
        None => "".into(),
        Some(_) => {
            let next = soloist::date(soloist.next_check().max(now));
            let newer = if soloist.store.newer.is_some() {
                "A newer build is out. "
            } else {
                ""
            };
            if settings.soloist_auto_update {
                format!("{newer}Lilac checks Spotify daily and replaces the build two weeks before it expires. Next check: {next}.").into()
            } else {
                format!("{newer}You will be reminded two weeks and three days before it expires. Next check: {next}.").into()
            }
        }
    };

    let key_state = api_key.clone();
    let key_row = row(
        "API key",
        if soloist.has_key() {
            "Saved. It is yours alone: Spotify asks every Soloist user for their own."
        } else {
            "Your own key from the Spotify for Developers dashboard. The account needs Premium."
        },
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(220.0))
                    .h(px(36.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .bg(theme::glass_fill_strong())
                    .border_1()
                    .border_color(theme::glass_border())
                    .child(Input::new(api_key).appearance(false)),
            )
            .child(
                ui::pill_button("soloist-key-save", "Save", false).on_click(move |_, _, cx| {
                    let key = key_state.read(cx).value().to_string();
                    app::soloist(cx).update(cx, |s, cx| s.set_api_key(key, cx));
                    app::toast("Soloist API key saved", cx);
                }),
            )
            .child(
                ui::pill_button("soloist-key-get", "Get a key", false)
                    .on_click(|_, _, cx| cx.open_url(build::KEY_PAGE)),
            ),
    );

    let pairing: SharedString = match (&soloist.process, soloist.logged_in, soloist.store.paired) {
        (Process::Failed(why), _, _) => format!("Soloist stopped: {why}").into(),
        (_, true, _) => "Paired and running.".into(),
        (_, false, true) => "Paired. Soloist starts when Lilac plays through it.".into(),
        (_, false, false) => {
            "Pick Soloist once in a Spotify app on this network to log it in.".into()
        }
    };
    let can_pair = soloist.installed().is_some() && soloist.has_key() && !soloist.expired();
    let pair_controls = div()
        .flex()
        .gap_2()
        .when(can_pair, |d| {
            d.child(
                ui::pill_button(
                    "soloist-pair",
                    if soloist.store.paired {
                        "Pair again"
                    } else {
                        "Pair"
                    },
                    !soloist.store.paired,
                )
                .on_click(|_, _, cx| {
                    let soloist = app::soloist(cx);
                    if soloist.read(cx).store.paired {
                        soloist.update(cx, |s, cx| s.unpair(cx));
                    }
                    soloist::Soloist::open_setup(cx);
                }),
            )
        })
        .when(soloist.store.paired, |d| {
            d.child(
                ui::pill_button("soloist-unpair", "Forget", false)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.unpair(cx))),
            )
        })
        .when(soloist.process != Process::Stopped, |d| {
            d.child(
                ui::pill_button("soloist-stop", "Stop", false)
                    .on_click(|_, _, cx| app::soloist(cx).update(cx, |s, cx| s.stop(cx))),
            )
        });

    section("Playback")
        .child(row(
            "Soloist",
            failed.map(SharedString::from).unwrap_or(status),
            install_controls,
        ))
        .when(soloist.installed().is_some(), |d| {
            d.child(row(
                "Update automatically",
                updates,
                switch(
                    "soloist-auto-update",
                    settings.soloist_auto_update,
                    |s, v| s.soloist_auto_update = v,
                ),
            ))
        })
        .child(key_row)
        .child(row("Pairing", pairing, pair_controls))
        .child(row(
            "About Soloist",
            "Spotify's documentation, downloads and terms.",
            ui::pill_button("soloist-docs", "Open", false)
                .on_click(|_, _, cx| cx.open_url(build::DOWNLOADS_PAGE)),
        ))
}
