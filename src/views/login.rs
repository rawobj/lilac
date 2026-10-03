//! The signed-out screen.

use gpui_kit::*;

use crate::app;
use crate::session::Status;
use crate::theme;
use crate::ui::{self, icons};

pub struct LoginView;

impl Render for LoginView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = app::session(cx);
        let (status, error) = {
            let session = session.read(cx);
            (session.status.clone(), session.error.clone())
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
                                .text_color(theme::text_muted())
                                .child("Waiting for you to finish in the browser…"),
                        ),
                )
                .child(ui::pill_button("cancel", "Cancel", false).on_click({
                    let session = session.clone();
                    move |_, _, cx| session.update(cx, |s, cx| s.cancel_login(cx))
                }))
        } else {
            div().flex().flex_col().items_center().gap_3().child(
                ui::pill_button("login", "Log in with Spotify", true)
                    .h(px(44.0))
                    .px_8()
                    .text_base()
                    .on_click({
                        let session = session.clone();
                        move |_, _, cx| session.update(cx, |s, cx| s.login(cx))
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
                    .child(
                        div()
                            .size(px(72.0))
                            .rounded(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(linear_gradient(
                                150.0,
                                linear_color_stop(theme::accent_soft(), 0.0),
                                linear_color_stop(theme::accent_strong(), 1.0),
                            ))
                            .child(
                                svg()
                                    .path(icons::LILAC)
                                    .size(px(36.0))
                                    .text_color(theme::backdrop_opaque()),
                            ),
                    )
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
                                    .child("lilac"),
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
