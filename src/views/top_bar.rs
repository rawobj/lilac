//! Top bar: history buttons, home, the search field and the account menu.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::nav::Route;
use crate::theme;
use crate::ui::{self, Placeholder, WithTooltip, icons};
use crate::ui_state::{Dialog, MenuItem, RightPanel};

pub struct TopBar {
    search: Entity<InputState>,
}

impl TopBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("What do you want to play?"));

        cx.subscribe(&search, |_, input, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let query = input.read(cx).value();
                if !query.is_empty() {
                    app::navigate(Route::Search, cx);
                }
                app::search(cx).update(cx, |search, cx| search.set_query(query, cx));
            }
            InputEvent::Focus => app::navigate(Route::Search, cx),
            _ => {}
        })
        .detach();

        cx.observe(&app::nav(cx), |_, _, cx| cx.notify()).detach();
        cx.observe(&app::session(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::feed(cx), |_, _, cx| cx.notify()).detach();
        cx.observe(&app::ui(cx), |_, _, cx| cx.notify()).detach();
        cx.observe(&app::settings(cx), |_, _, cx| cx.notify())
            .detach();

        Self { search }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    fn account_menu(position: Point<Pixels>, cx: &mut App) {
        let items = vec![
            MenuItem::new(icons::USER, "Profile", |_, cx| {
                app::navigate(Route::Profile, cx)
            }),
            MenuItem::new(icons::SETTINGS, "Settings", |_, cx| {
                app::navigate(Route::Settings, cx)
            }),
            MenuItem::new(icons::KEYBOARD, "Keyboard shortcuts", |_, cx| {
                app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::Shortcuts, cx))
            })
            .separated(),
            MenuItem::new(icons::INFO, "About Lilac", |_, cx| {
                app::ui(cx).update(cx, |ui, cx| ui.open_dialog(Dialog::About, cx))
            }),
            MenuItem::new(icons::LOG_OUT, "Log out", |_, cx| {
                app::session(cx).update(cx, |session, cx| session.logout(cx))
            })
            .separated(),
        ];
        app::ui(cx).update(cx, |ui, cx| ui.open_menu(position, items, cx));
    }
}

/// Keep a press on a control from also starting a window drag.
fn no_drag(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

fn nav_button(id: &'static str, icon: &'static str, enabled: bool) -> Stateful<Div> {
    ui::icon_button(id, icon)
        .on_mouse_down(MouseButton::Left, no_drag)
        .bg(theme::black(0.25))
        .when(!enabled, |b| b.opacity(0.35).cursor_default())
}

fn window_button(id: &'static str, icon: &'static str, danger: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .hover(move |s| {
            s.bg(if danger {
                theme::danger().opacity(0.8)
            } else {
                theme::hover_fill()
            })
        })
        .child(
            svg()
                .path(icon)
                .size(px(14.0))
                .text_color(theme::text_muted()),
        )
}

impl Render for TopBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (can_back, can_forward, at_home, at_search) = {
            let nav = app::nav(cx);
            let nav = nav.read(cx);
            (
                nav.can_go_back(),
                nav.can_go_forward(),
                *nav.current() == Route::Home,
                *nav.current() == Route::Search,
            )
        };
        let user = app::session(cx).read(cx).user.clone();
        let avatar = user.as_ref().and_then(|u| u.images.for_size(32));
        let client_decorated = matches!(window.window_decorations(), Decorations::Client { .. });
        let at_whats_new = *app::nav(cx).read(cx).current() == Route::WhatsNew;
        let friends_open = app::ui(cx).read(cx).right_panel == RightPanel::Friends;
        let has_new = {
            let seen = app::settings(cx).read(cx).whats_new_seen.clone();
            app::feed(cx).read(cx).unseen(&seen) > 0
        };

        div()
            .id("top-bar")
            .size_full()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        nav_button("back", icons::CHEVRON_LEFT, can_back)
                            .tip("Go back")
                            .on_click(|_, _, cx| app::nav(cx).update(cx, |nav, cx| nav.back(cx))),
                    )
                    .child(
                        nav_button("forward", icons::CHEVRON_RIGHT, can_forward)
                            .tip("Go forward")
                            .on_click(|_, _, cx| {
                                app::nav(cx).update(cx, |nav, cx| nav.forward(cx))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("home")
                            .size(px(40.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(theme::glass_fill_strong())
                            .border_1()
                            .border_color(theme::glass_border())
                            .hover(|s| s.bg(theme::active_fill()))
                            .child(
                                svg()
                                    .path(icons::HOME)
                                    .size(px(20.0))
                                    .text_color(if at_home {
                                        theme::text()
                                    } else {
                                        theme::text_muted()
                                    }),
                            )
                            .tip("Home")
                            .on_mouse_down(MouseButton::Left, no_drag)
                            .on_click(|_, _, cx| app::navigate(Route::Home, cx)),
                    )
                    .child(
                        div()
                            .id("search-field")
                            .w(px(420.0))
                            .h(px(40.0))
                            .pl_3()
                            .pr_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded_full()
                            .bg(theme::glass_fill_strong())
                            .border_1()
                            .border_color(if at_search {
                                theme::accent_alpha(0.6)
                            } else {
                                theme::glass_border()
                            })
                            .hover(|s| s.bg(theme::active_fill()))
                            // Clicks belong to the field, not to the window drag region.
                            .on_mouse_down(MouseButton::Left, no_drag)
                            .child(
                                svg()
                                    .path(icons::SEARCH)
                                    .size(px(18.0))
                                    .text_color(theme::text_muted()),
                            )
                            .child(Input::new(&self.search).appearance(false).cleanable(true)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .relative()
                            .child(
                                ui::toggle_icon_button("whats-new", icons::BELL, at_whats_new)
                                    .tip("What's New")
                                    .on_mouse_down(MouseButton::Left, no_drag)
                                    .on_click(|_, _, cx| {
                                        app::nav(cx)
                                            .update(cx, |nav, cx| nav.toggle(Route::WhatsNew, cx))
                                    }),
                            )
                            .when(has_new && !at_whats_new, |bell| {
                                bell.child(
                                    div()
                                        .absolute()
                                        .top(px(6.0))
                                        .right(px(6.0))
                                        .size(px(8.0))
                                        .rounded_full()
                                        .bg(theme::pink())
                                        .border_1()
                                        .border_color(theme::backdrop_opaque()),
                                )
                            }),
                    )
                    .child(
                        ui::toggle_icon_button("friends", icons::USERS, friends_open)
                            .tip("Friend Activity")
                            .on_mouse_down(MouseButton::Left, no_drag)
                            .on_click(|_, _, cx| {
                                app::ui(cx)
                                    .update(cx, |ui, cx| ui.toggle_panel(RightPanel::Friends, cx))
                            }),
                    )
                    .child(
                        div()
                            .id("account")
                            .p(px(3.0))
                            .rounded_full()
                            .cursor_pointer()
                            .bg(theme::black(0.25))
                            .hover(|s| s.bg(theme::active_fill()))
                            .child(ui::cover(avatar, 30.0, 15.0, Placeholder::User, window, cx))
                            .tip("Account")
                            .on_mouse_down(MouseButton::Left, no_drag)
                            .on_click(|event, _, cx| Self::account_menu(event.position(), cx)),
                    )
                    .when(client_decorated, |row| {
                        row.child(
                            div()
                                .ml_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .on_mouse_down(MouseButton::Left, no_drag)
                                .child(
                                    window_button("minimize", icons::MINUS, false)
                                        .on_click(|_, window, _| window.minimize_window()),
                                )
                                .child(
                                    window_button("maximize", icons::SQUARE, false)
                                        .on_click(|_, window, _| window.zoom_window()),
                                )
                                .child(
                                    window_button("close", icons::X, true)
                                        .on_click(|_, window, _| window.remove_window()),
                                ),
                        )
                    }),
            )
    }
}
