//! The window's root view: lays out the panels and hosts overlays.

mod login;
mod sidebar;
mod top_bar;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::keys::{Dismiss, FocusSearch};
use crate::session::Status;
use crate::theme;
use crate::ui::{self, icons};
use crate::ui_state::RightPanel;

use login::LoginView;
use sidebar::Sidebar;
use top_bar::TopBar;

/// Wrap a child view so it is only re-rendered when it changes itself.
///
/// Without this, any repaint (a hover highlight, the seek bar ticking) would
/// rebuild every panel in the window.
fn cached<V: Render>(view: &Entity<V>, style: StyleRefinement) -> impl IntoElement + use<V> {
    AnyView::from(view.clone()).cached(style)
}

/// Panel sizes for the current window width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub sidebar: f32,
    /// Whether the right-hand panel fits.
    pub panel: bool,
    /// Width left for the main page.
    pub main: f32,
}

impl Layout {
    /// The main page never gets narrower than this; side panels give way first.
    const MIN_MAIN: f32 = 520.0;

    pub fn compute(window_width: Pixels, sidebar_collapsed: bool, panel: RightPanel) -> Self {
        let width = f32::from(window_width);
        let gaps = theme::GAP * 2.0;
        let mut sidebar = if sidebar_collapsed {
            theme::SIDEBAR_COLLAPSED
        } else {
            theme::SIDEBAR_WIDTH
        };
        let mut show_panel = panel != RightPanel::Hidden;
        let panel_cost = theme::RIGHT_PANEL_WIDTH + theme::GAP;

        let main = |sidebar: f32, show_panel: bool| {
            width - gaps - sidebar - theme::GAP - if show_panel { panel_cost } else { 0.0 }
        };
        // Shed the sidebar's labels first, then the right panel.
        if main(sidebar, show_panel) < Self::MIN_MAIN {
            sidebar = theme::SIDEBAR_COLLAPSED;
        }
        if main(sidebar, show_panel) < Self::MIN_MAIN {
            show_panel = false;
        }
        Self {
            sidebar,
            panel: show_panel,
            main: main(sidebar, show_panel),
        }
    }
}

/// Layout for the window as it is right now.
pub fn layout(window: &Window, cx: &App) -> Layout {
    let ui = app::ui(cx);
    let ui = ui.read(cx);
    Layout::compute(
        window.viewport_size().width,
        ui.sidebar_collapsed,
        ui.right_panel,
    )
}

pub struct Lilac {
    focus: FocusHandle,
    login: Entity<LoginView>,
    top_bar: Entity<TopBar>,
    sidebar: Entity<Sidebar>,
}

impl Lilac {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.global::<app::AppState>();
        let (session, nav, ui, settings) = (
            state.session.clone(),
            state.nav.clone(),
            state.ui.clone(),
            state.settings.clone(),
        );

        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.observe(&nav, |_, _, cx| cx.notify()).detach();
        cx.observe(&ui, |_, _, cx| cx.notify()).detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);

        Self {
            focus,
            login: cx.new(|_| LoginView),
            top_bar: cx.new(|cx| TopBar::new(window, cx)),
            sidebar: cx.new(|cx| Sidebar::new(window, cx)),
        }
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        let ui = app::ui(cx);
        let (has_menu, has_dialog, immersive) = {
            let ui = ui.read(cx);
            (ui.menu.is_some(), ui.dialog.is_some(), ui.immersive)
        };
        if has_menu {
            ui.update(cx, |ui, cx| ui.close_menu(cx));
        } else if has_dialog {
            ui.update(cx, |ui, cx| ui.close_dialog(cx));
        } else if immersive {
            ui.update(cx, |ui, cx| ui.toggle_immersive(cx));
        }
        self.focus.focus(window, cx);
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.top_bar
            .update(cx, |bar, cx| bar.focus_search(window, cx));
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
            .child(
                theme::glow(theme::rgba8(0x4f46e5, 0.14).into(), 200.0)
                    .top(px(160.0))
                    .right(px(-80.0)),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = app::session(cx).read(cx).status.clone();
        let (immersive, sidebar_collapsed, panel) = {
            let ui = app::ui(cx);
            let ui = ui.read(cx);
            (ui.immersive, ui.sidebar_collapsed, ui.right_panel)
        };

        let content = match status {
            Status::Starting => self.splash().into_any_element(),
            Status::LoggedOut | Status::WaitingForBrowser => self.login.clone().into_any_element(),
            Status::LoggedIn => {
                let layout =
                    Layout::compute(window.viewport_size().width, sidebar_collapsed, panel);
                let sidebar_width = layout.sidebar;
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(cached(
                        &self.top_bar,
                        StyleRefinement::default()
                            .w_full()
                            .h(px(theme::TOP_BAR_HEIGHT))
                            .flex_none(),
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px(px(theme::GAP))
                            .flex()
                            .gap(px(theme::GAP))
                            .child(
                                theme::glass()
                                    .flex_none()
                                    .w(px(sidebar_width))
                                    .h_full()
                                    .overflow_hidden()
                                    .child(cached(
                                        &self.sidebar,
                                        StyleRefinement::default().size_full(),
                                    )),
                            )
                            .child(
                                theme::glass()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .overflow_hidden()
                                    .child(ui::empty_state(
                                        icons::MUSIC,
                                        "Nothing here yet",
                                        "Pages are on their way.",
                                    )),
                            ),
                    )
                    .into_any_element()
            }
        };

        div()
            .id("lilac")
            .key_context("Lilac")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::focus_search))
            .relative()
            .size_full()
            .font_family(theme::FONT_UI)
            .text_color(theme::text())
            .text_sm()
            .child(self.backdrop(cx))
            .child(div().relative().size_full().child(content))
    }
}

#[cfg(test)]
mod tests {
    use super::Layout;
    use crate::theme;
    use crate::ui_state::RightPanel;
    use gpui_kit::px;

    #[test]
    fn panels_give_way_as_the_window_narrows() {
        let wide = Layout::compute(px(1600.0), false, RightPanel::Queue);
        assert_eq!(wide.sidebar, theme::SIDEBAR_WIDTH);
        assert!(wide.panel);

        let medium = Layout::compute(px(1150.0), false, RightPanel::Queue);
        assert_eq!(medium.sidebar, theme::SIDEBAR_COLLAPSED);
        assert!(medium.panel);

        let narrow = Layout::compute(px(900.0), false, RightPanel::Queue);
        assert_eq!(narrow.sidebar, theme::SIDEBAR_COLLAPSED);
        assert!(!narrow.panel);
        assert!(narrow.main >= Layout::MIN_MAIN);

        let hidden = Layout::compute(px(1600.0), true, RightPanel::Hidden);
        assert!(!hidden.panel);
        assert_eq!(hidden.sidebar, theme::SIDEBAR_COLLAPSED);
    }
}
