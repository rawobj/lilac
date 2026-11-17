//! The signed-out screen.

use gpui_kit::*;

use crate::app;
use crate::session::Status;
use crate::theme;
use crate::ui;

pub struct LoginView;

impl Render for LoginView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = app::session(cx);
        let (status, error, step) = {
            let session = session.read(cx);
            (
                session.status.clone(),
                session.error.clone(),
                session.login_step.clone(),
            )
        };
        let waiting = status == Status::WaitingForBrowser;

        let action = if waiting {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(ui::spinner(18.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_color(theme::text_muted())
                                        .child("Waiting for you to finish in the browser…"),
                                )
                                .children(step.map(|step| {
                                    div().text_xs().text_color(theme::text_subtle()).child(step)
                                })),
                        ),
                )
                .child(ui::pill_button("cancel", "Cancel", false).on_click({
                    let session = session.clone();
                    move |_, _, cx| session.update(cx, |s, cx| s.cancel_login(cx))
                }))
        } else {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    ui::pill_button("login", "Log in with Spotify", true)
                        .h(px(44.0))
                        .px_8()
                        .text_base()
                        .on_click({
                            let session = session.clone();
                            move |_, _, cx| session.update(cx, |s, cx| s.login(cx))
                        }),
                )
                .child(
                    div()
                        .id("demo")
                        .text_xs()
                        .text_color(theme::text_muted())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::accent_soft()))
                        .child("or explore the demo library")
                        .on_click({
                            let session = session.clone();
                            move |_, _, cx| session.update(cx, |s, cx| s.enter_demo(cx))
                        }),
                )
        };

        div()
            .id("login")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            // The whole screen moves the window; there is no title bar here.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(
                theme::glass()
                    .w(px(440.0))
                    .p_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_6()
                    .occlude()
                    .child(img(crate::assets::APP_ICON).size(px(88.0)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_3xl()
                                    .font_weight(FontWeight::BOLD)
                                    .child("Lilac"),
                            )
                            .child(
                                div()
                                    .text_color(theme::text_muted())
                                    .child("A fast, native client for your Spotify library."),
                            ),
                    )
                    .child(action)
                    .children(error.map(|error| {
                        div()
                            .w_full()
                            .p_3()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::danger().opacity(0.12))
                            .text_xs()
                            .text_color(theme::danger())
                            .child(error)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::text_subtle())
                            .child("Playback requires Spotify Premium."),
                    ),
            )
    }
}
