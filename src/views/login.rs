//! The signed-out screen.

use gpui_kit::*;

use crate::app;
use crate::session::Status;
use crate::theme;
use crate::ui;
use crate::ui::icons;
use crate::ui::window_frame::DragsWindow as _;

pub struct LoginView;

impl Render for LoginView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = app::session(cx);
        let (status, error, step, url) = {
            let session = session.read(cx);
            (
                session.status.clone(),
                session.error.clone(),
                session.login_step.clone(),
                session.login_url.clone(),
            )
        };
        let waiting = status == Status::WaitingForBrowser;

        let action = if waiting {
            let current = step.unwrap_or(0);
            div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Two approvals in your browser"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_center()
                                .text_color(theme::text_muted())
                                .child(
                                    "Open each in the browser where you are logged in to \
                                     Spotify, or copy the link into it.",
                                ),
                        ),
                )
                .children(STEPS.iter().enumerate().map(|(index, (title, what))| {
                    let state = match index.cmp(&current) {
                        std::cmp::Ordering::Less => StepState::Done,
                        std::cmp::Ordering::Equal => StepState::Current,
                        std::cmp::Ordering::Greater => StepState::Next,
                    };
                    let url = (state == StepState::Current).then(|| url.clone()).flatten();
                    step_row(index, title, what, state, url)
                }))
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
            .drags_window()
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
                            .text_center()
                            .text_color(theme::text_subtle())
                            .child(
                                "Spotify Premium is needed for playback. \
                                 Spotify Free plays 30-second previews.",
                            ),
                    ),
            )
    }
}

/// The two approvals of a sign-in: the desktop client the player streams
/// through, then Lilac's own client for the library.
const STEPS: [(&str, &str); 2] = [
    ("Allow playback", "Lets Lilac's player sign in to your account."),
    ("Allow access to your library", "Playlists, saved songs, albums and artists."),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepState {
    Done,
    Current,
    Next,
}

fn step_row(
    index: usize,
    title: &'static str,
    what: &'static str,
    state: StepState,
    url: Option<SharedString>,
) -> Div {
    let badge = div()
        .size(px(26.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .text_sm()
        .font_weight(FontWeight::BOLD);
    let badge = match state {
        StepState::Done => badge.bg(theme::accent()).child(
            svg()
                .path(icons::CHECK)
                .size(px(14.0))
                .text_color(gpui_kit::white()),
        ),
        StepState::Current => badge
            .bg(theme::accent())
            .text_color(gpui_kit::white())
            .child((index + 1).to_string()),
        StepState::Next => badge
            .border_1()
            .border_color(theme::glass_border())
            .text_color(theme::text_subtle())
            .child((index + 1).to_string()),
    };
    let status = match state {
        StepState::Done => div()
            .text_xs()
            .text_color(theme::accent_soft())
            .child("Approved"),
        StepState::Current => div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(theme::text_muted())
            .child(ui::spinner(12.0))
            .child("Waiting for your approval…"),
        StepState::Next => div()
            .text_xs()
            .text_color(theme::text_subtle())
            .child("Next"),
    };

    let row = div()
        .w_full()
        .p_3()
        .flex()
        .gap_3()
        .rounded(px(theme::RADIUS_CARD))
        .border_1()
        .child(badge)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if state == StepState::Next {
                            theme::text_subtle()
                        } else {
                            theme::text()
                        })
                        .child(title),
                )
                .child(div().text_xs().text_color(theme::text_subtle()).child(what))
                .child(status)
                .children(url.map(|url| {
                    div()
                        .pt_2()
                        .flex()
                        .gap_2()
                        .child(ui::pill_button("open", "Open in browser", true).on_click({
                            let url = url.clone();
                            move |_, _, cx| {
                                if let Err(err) = open::that_detached(url.as_ref()) {
                                    log::warn!("could not open the browser: {err}");
                                    app::toast("Could not open the browser; copy the link", cx);
                                }
                            }
                        }))
                        .child(ui::pill_button("copy", "Copy link", false).on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
                                app::toast("Link copied", cx);
                            },
                        ))
                })),
        );
    match state {
        StepState::Current => row
            .bg(theme::accent().opacity(0.14))
            .border_color(theme::accent().opacity(0.55)),
        StepState::Done => row
            .bg(theme::glass_fill())
            .border_color(theme::glass_border()),
        StepState::Next => row.border_color(theme::glass_border()).opacity(0.6),
    }
}
