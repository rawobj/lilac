//! The window's root view.

mod login;

use gpui_kit::*;

use crate::app;
use crate::session::Status;
use crate::theme;
use crate::ui::{self, icons};

use login::LoginView;

pub struct Lilac {
    focus: FocusHandle,
    login: Entity<LoginView>,
}

impl Lilac {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.global::<app::AppState>();
        let (session, settings) = (state.session.clone(), state.settings.clone());
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);

        Self {
            focus,
            login: cx.new(|_| LoginView),
        }
    }

    fn backdrop(&self, cx: &App) -> Div {
        let opaque = app::settings(cx).read(cx).reduce_transparency;
        div()
            .absolute()
            .size_full()
            .overflow_hidden()
            .bg(if opaque {
                theme::backdrop_opaque()
            } else {
                theme::backdrop()
            })
            .child(
                theme::glow(theme::rgba8(0x7c3aed, 0.22).into(), 280.0)
                    .top(px(-120.0))
                    .left(px(-60.0)),
            )
            .child(
                theme::glow(theme::rgba8(0xc026d3, 0.13).into(), 240.0)
                    .bottom(px(-80.0))
                    .right(px(120.0)),
            )
    }

    fn splash(&self) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                svg()
                    .path(icons::LILAC)
                    .size(px(48.0))
                    .text_color(theme::accent()),
            )
            .child(ui::spinner(22.0))
    }
}

impl Render for Lilac {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, user) = {
            let session = app::session(cx);
            let session = session.read(cx);
            (session.status.clone(), session.user.clone())
        };

        let content = match status {
            Status::Starting => self.splash().into_any_element(),
            Status::LoggedOut | Status::WaitingForBrowser => self.login.clone().into_any_element(),
            Status::LoggedIn => ui::empty_state(
                icons::LILAC,
                format!("Hello, {}", user.map(|u| u.name()).unwrap_or_default()),
                "You are signed in.",
            )
            .into_any_element(),
        };

        div()
            .id("lilac")
            .key_context("Lilac")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .font_family(theme::FONT_UI)
            .text_color(theme::text())
            .text_sm()
            .child(self.backdrop(cx))
            .child(div().relative().size_full().child(content))
    }
}
