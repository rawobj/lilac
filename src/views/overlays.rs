//! Layers drawn above the panels: context menu, dialogs and toasts.

use gpui_kit::component::input::{Input, InputState};
use gpui_kit::*;

use crate::app;
use crate::keys::SHORTCUTS;
use crate::library::SavedEntity;
use crate::theme;
use crate::ui::{self, icons};
use crate::ui_state::{Dialog, ToastKind};

pub fn menu(scroll: &ScrollHandle, cx: &mut App) -> Option<impl IntoElement + use<>> {
    let ui = app::ui(cx);
    let menu = ui.read(cx).menu.as_ref()?;
    let position = menu.position;
    let generation = menu.generation;

    let mut list = theme::popup()
        .id(("menu", generation))
        .min_w(px(220.0))
        .max_w(px(320.0))
        .max_h(px(420.0))
        .overflow_y_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(scroll)
        .p_1()
        .pr(px(12.0))
        .flex()
        .flex_col();

    for (ix, item) in menu.items.iter().enumerate() {
        if item.separator {
            list = list.child(div().my_1().mx_2().h(px(1.0)).bg(theme::glass_border()));
        }
        let color = if item.danger {
            theme::danger()
        } else {
            theme::text()
        };
        let action = item.action.clone();
        list = list.child(
            div()
                .id(("item", ix))
                .h(px(34.0))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(6.0))
                .cursor_pointer()
                .text_sm()
                .text_color(color)
                .hover(|s| s.bg(theme::hover_fill()))
                .child(
                    svg()
                        .path(item.icon)
                        .size(px(16.0))
                        .text_color(if item.danger {
                            theme::danger()
                        } else {
                            theme::text_muted()
                        }),
                )
                .child(div().flex_1().truncate().child(item.label.clone()))
                .on_click(move |_, window, cx| {
                    // Close first: the action may open a follow-up menu.
                    app::ui(cx).update(cx, |ui, cx| ui.close_menu(cx));
                    action(window, cx);
                }),
        );
    }

    let list =
        list.on_mouse_down_out(|_, _, cx| app::ui(cx).update(cx, |ui, cx| ui.close_menu(cx)));

    Some(
        deferred(
            anchored()
                .position(position)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    // Blocks the pointer for what is behind, here rather
                    // than on the list, so hovering it shows the scrollbar.
                    div()
                        .id("menu-frame")
                        .occlude()
                        .relative()
                        .child(list)
                        .group("scrollable")
                        .child(ui::vertical_scrollbar(scroll)),
                ),
        )
        .with_priority(2),
    )
}

pub fn toasts(cx: &mut App) -> Option<impl IntoElement + use<>> {
    let ui = app::ui(cx);
    let ui = ui.read(cx);
    if ui.toasts.is_empty() {
        return None;
    }
    let mut column = div()
        .absolute()
        .bottom(px(theme::PLAYER_BAR_HEIGHT + 16.0))
        .left_0()
        .right_0()
        .flex()
        .flex_col()
        .items_center()
        .gap_2();
    for toast in &ui.toasts {
        let error = toast.kind == ToastKind::Error;
        let id = toast.id;
        let actions = toast
            .actions
            .iter()
            .enumerate()
            .map(|(ix, (label, action))| {
                let action = action.clone();
                div()
                    .id(("toast-action", id as usize * 4 + ix))
                    .flex_none()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::accent())
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::white(0.08)))
                    .child(label.clone())
                    .on_click(move |_, window, cx| {
                        app::ui(cx).update(cx, |ui, cx| ui.dismiss_toast(id, cx));
                        action(window, cx);
                    })
            });
        column = column.child(
            theme::popup()
                .rounded(px(19.0))
                .px_4()
                .py_2()
                .min_h(px(38.0))
                .max_w(px(560.0))
                .flex()
                .items_center()
                .gap_2()
                .child(
                    svg()
                        .path(if error {
                            icons::CIRCLE_ALERT
                        } else {
                            icons::CIRCLE_CHECK
                        })
                        .size(px(16.0))
                        .text_color(if error {
                            theme::danger()
                        } else {
                            theme::accent()
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .line_clamp(2)
                        .text_sm()
                        .child(toast.text.clone()),
                )
                .children(actions),
        );
    }
    Some(deferred(column).with_priority(3))
}

fn field(label: &'static str, state: &Entity<InputState>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(
            div()
                .h(px(40.0))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(theme::RADIUS_CONTROL))
                .bg(theme::glass_fill_strong())
                .border_1()
                .border_color(theme::glass_border())
                .child(Input::new(state).appearance(false)),
        )
}

pub fn dialog(
    name: &Entity<InputState>,
    description: &Entity<InputState>,
    soloist_key: &Entity<InputState>,
    scroll: &ScrollHandle,
    cx: &mut App,
) -> Option<impl IntoElement + use<>> {
    let dialog = app::ui(cx).read(cx).dialog.clone()?;
    let close = |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        app::ui(cx).update(cx, |ui, cx| ui.close_dialog(cx));
    };

    let (title, body, confirm): (&str, AnyElement, Option<AnyElement>) = match dialog {
        Dialog::CreatePlaylist { seed } => {
            let (name_state, description_state) = (name.clone(), description.clone());
            (
                "Create playlist",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(field("Name", name))
                    .child(field("Description", description))
                    .into_any_element(),
                Some(
                    ui::pill_button("confirm", "Create", true)
                        .on_click(move |_, _, cx| {
                            let name = name_state.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let description = description_state.read(cx).value().trim().to_string();
                            let seed = seed.clone().map(|seed| *seed);
                            app::library(cx).update(cx, |library, cx| {
                                library.create_playlist(name, description, seed, cx)
                            });
                            app::ui(cx).update(cx, |ui, cx| ui.close_dialog(cx));
                        })
                        .into_any_element(),
                ),
            )
        }
        Dialog::EditPlaylist { id } => {
            let (name_state, description_state) = (name.clone(), description.clone());
            (
                "Edit details",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(field("Name", name))
                    .child(field("Description", description))
                    .into_any_element(),
                Some(
                    ui::pill_button("confirm", "Save", true)
                        .on_click(move |_, _, cx| {
                            let name = name_state.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                return;
                            }
                            let description = description_state.read(cx).value().trim().to_string();
                            let id = id.clone();
                            app::library(cx).update(cx, |library, cx| {
                                library.rename_playlist(id, name, description, cx)
                            });
                            app::ui(cx).update(cx, |ui, cx| ui.close_dialog(cx));
                        })
                        .into_any_element(),
                ),
            )
        }
        Dialog::DeletePlaylist { id, name } => (
            "Delete from Your Library?",
            div()
                .text_color(theme::text_muted())
                .child(format!("This will remove “{name}” from Your Library."))
                .into_any_element(),
            Some(
                ui::pill_button("confirm", "Delete", true)
                    .on_click(move |_, _, cx| {
                        // Spotify has no hard delete; removing a playlist is unfollowing it.
                        if let Some(playlist) = app::library(cx).read(cx).playlist(&id).cloned() {
                            app::library(cx).update(cx, |library, cx| {
                                library.toggle_saved(SavedEntity::Playlist(playlist), cx)
                            });
                        }
                        let route = crate::nav::Route::Playlist(id.clone());
                        if *app::nav(cx).read(cx).current() == route {
                            app::nav(cx).update(cx, |nav, cx| nav.back(cx));
                        }
                        app::ui(cx).update(cx, |ui, cx| ui.close_dialog(cx));
                    })
                    .into_any_element(),
            ),
        ),
        Dialog::Shortcuts => {
            let mut grid = div().flex().flex_col().gap(px(6.0));
            for (keys, what) in SHORTCUTS {
                grid = grid.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(div().text_color(theme::text_muted()).child(*what))
                        .child(
                            div()
                                .px_2()
                                .py(px(2.0))
                                .rounded(px(6.0))
                                .bg(theme::glass_fill_strong())
                                .border_1()
                                .border_color(theme::glass_border())
                                .text_xs()
                                .child(*keys),
                        ),
                );
            }
            ("Keyboard shortcuts", grid.into_any_element(), None)
        }
        Dialog::ArtistAbout { id } => (
            "About the artist",
            super::artist_about::render(&id, cx),
            None,
        ),
        Dialog::SoloistSetup => super::soloist_setup::render(soloist_key, cx),
        Dialog::SoloistExpired => {
            let soloist = app::soloist(cx).read(cx);
            let (version, expired_on) = soloist
                .installed()
                .map(|b| (b.version.clone(), crate::soloist::date(b.expires_at())))
                .unwrap_or_default();
            let install = soloist.install.clone();
            let busy = matches!(
                install,
                crate::soloist::Install::Downloading(..) | crate::soloist::Install::Unpacking
            );
            let progress = match install {
                crate::soloist::Install::Downloading(got, total) => Some(match total {
                    Some(total) if total > 0 => {
                        format!("Downloading… {}%", got * 100 / total)
                    }
                    _ => format!("Downloading… {:.1} MB", got as f64 / 1e6),
                }),
                crate::soloist::Install::Unpacking => Some("Installing…".into()),
                crate::soloist::Install::Failed(why) => Some(format!("Download failed: {why}")),
                crate::soloist::Install::Idle => None,
            };
            (
                "Soloist has expired",
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .text_color(theme::text_muted())
                    .child(format!(
                        "Soloist {version} stopped working on {expired_on}. Spotify's Soloist builds run for 90 days; a newer build keeps Lilac playing."
                    ))
                    .children(progress.map(|p| div().text_color(theme::text()).child(p)))
                    .into_any_element(),
                (!busy).then(|| {
                    ui::pill_button("confirm", "Download from Spotify", true)
                        .on_click(|_, _, cx| {
                            app::soloist(cx).update(cx, |s, cx| s.install(cx));
                        })
                        .into_any_element()
                }),
            )
        }
        Dialog::About => (
            "About Lilac",
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(img(crate::assets::APP_ICON).flex_none().size(px(72.0)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_color(theme::text_muted())
                        .child(concat!("Version ", env!("CARGO_PKG_VERSION")))
                        .child("A native Spotify client written in Rust with GPUI.")
                        .child("Not affiliated with or endorsed by Spotify AB."),
                )
                .into_any_element(),
            None,
        ),
    };

    let has_confirm = confirm.is_some();
    let shown = app::ui(cx).read(cx).dialog.clone();
    let wide = matches!(
        shown,
        Some(Dialog::SoloistSetup | Dialog::ArtistAbout { .. })
    );
    let roomy = matches!(shown, Some(Dialog::ArtistAbout { .. }));
    let panel = theme::popup()
        .id("dialog")
        .w(px(if roomy {
            640.0
        } else if wide {
            500.0
        } else {
            440.0
        }))
        .max_h(px(if wide { 760.0 } else { 560.0 }))
        .overflow_y_scroll()
        .restrict_scroll_to_axis()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_xl().font_weight(FontWeight::BOLD).child(title))
                .child(ui::icon_button("close", icons::X).on_click(close)),
        )
        .child(body)
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    ui::pill_button(
                        "cancel",
                        if has_confirm { "Cancel" } else { "Close" },
                        false,
                    )
                    .on_click(close),
                )
                .children(confirm),
        );

    Some(
        deferred(
            div()
                .id("scrim")
                .absolute()
                .size_full()
                .top_0()
                .left_0()
                .occlude()
                .bg(theme::black(0.55))
                .flex()
                .items_center()
                .justify_center()
                .on_click(close)
                .child(
                    div()
                        .id("dialog-frame")
                        .occlude()
                        .relative()
                        .child(panel.track_scroll(scroll))
                        .group("scrollable")
                        .child(ui::vertical_scrollbar(scroll))
                        .on_click(|_, _, cx| cx.stop_propagation()),
                ),
        )
        .with_priority(1),
    )
}
