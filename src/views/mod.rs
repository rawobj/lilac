//! The window's root view: lays out the panels and hosts overlays.

mod login;
mod overlays;
mod player_bar;
mod sidebar;
mod soloist_setup;
mod splash;
mod top_bar;

use std::collections::HashMap;

use gpui_kit::component::input::InputState;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::keys::{Dismiss, FocusSearch};
use crate::nav::Route;
use crate::panels::RightPanelView;
use crate::session::Status;
use crate::theme;
use crate::ui_state::Dialog;
use crate::ui_state::RightPanel;

use login::LoginView;
use player_bar::PlayerBar;
use sidebar::Sidebar;
use splash::{Splash, SplashEvent};
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
    player_bar: Entity<PlayerBar>,
    right_panel: Entity<RightPanelView>,
    /// Pages for every route still in the history, so going back is instant.
    pages: HashMap<Route, AnyView>,
    dialog_name: Entity<InputState>,
    dialog_description: Entity<InputState>,
    soloist_key: Entity<InputState>,
    menu_scroll: ScrollHandle,
    dialog_scroll: ScrollHandle,
    /// Playback is locked until Soloist is set up (see `Player::locked`).
    locked: bool,
    shown_dialog: Option<Dialog>,
    /// The loading screen, from start (or login) until the player is warm.
    splash: Option<Entity<Splash>>,
    splash_done: bool,
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

        cx.observe(&session, |this: &mut Self, session, cx| {
            match session.read(cx).status {
                Status::LoggedOut => {
                    this.pages.clear();
                    this.splash = None;
                    this.splash_done = false;
                }
                // A fresh login warms up like a start with stored credentials.
                Status::LoggedIn if this.splash.is_none() && !this.splash_done => {
                    this.show_splash(cx);
                }
                _ => {}
            }
            cx.notify();
        })
        .detach();
        cx.observe(&nav, |this: &mut Self, nav, cx| {
            let nav = nav.read(cx);
            this.pages.retain(|route, _| nav.retained().contains(route));
            cx.notify();
        })
        .detach();
        cx.observe_in(&ui, window, |this: &mut Self, _, window, cx| {
            this.sync_dialog(window, cx);
            cx.notify();
        })
        .detach();
        cx.observe(&settings, |_, _, cx| cx.notify()).detach();
        // Only the Soloist dialogs show its state; spare the window the
        // download progress otherwise.
        cx.observe(&app::soloist(cx), |this: &mut Self, _, cx| {
            if matches!(
                app::ui(cx).read(cx).dialog,
                Some(Dialog::SoloistSetup | Dialog::SoloistExpired)
            ) {
                cx.notify();
            }
            this.sync_locked(cx);
        })
        .detach();
        cx.subscribe(
            &app::player(cx),
            |this: &mut Self, _, _: &crate::player::PlayerEvent, cx| this.sync_locked(cx),
        )
        .detach();

        let focus = cx.focus_handle();
        focus.focus(window, cx);

        let mut this = Self {
            focus,
            login: cx.new(|_| LoginView),
            top_bar: cx.new(|cx| TopBar::new(window, cx)),
            sidebar: cx.new(|cx| Sidebar::new(window, cx)),
            player_bar: cx.new(PlayerBar::new),
            right_panel: cx.new(RightPanelView::new),
            pages: HashMap::new(),
            dialog_name: cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name")),
            dialog_description: cx
                .new(|cx| InputState::new(window, cx).placeholder("Add an optional description")),
            soloist_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("spak_…")
            }),
            locked: false,
            menu_scroll: ScrollHandle::new(),
            dialog_scroll: ScrollHandle::new(),
            shown_dialog: None,
            splash: None,
            splash_done: false,
        };
        if session.read(cx).status != Status::LoggedOut {
            this.show_splash(cx);
        }
        this
    }

    fn show_splash(&mut self, cx: &mut Context<Self>) {
        let splash = cx.new(Splash::new);
        cx.subscribe(&splash, |this, _, event: &SplashEvent, cx| match event {
            SplashEvent::Done => {
                this.splash = None;
                this.splash_done = true;
                cx.notify();
            }
        })
        .detach();
        self.splash = Some(splash);
    }

    /// Prefill the dialog fields when a dialog opens.
    fn sync_locked(&mut self, cx: &mut Context<Self>) {
        let locked = app::player(cx).read(cx).locked(cx);
        if locked != self.locked {
            self.locked = locked;
            cx.notify();
        }
    }

    fn sync_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dialog = app::ui(cx).read(cx).dialog.clone();
        if dialog == self.shown_dialog {
            return;
        }
        if self.shown_dialog == Some(Dialog::SoloistSetup) {
            app::soloist(cx).update(cx, |s, cx| s.setup_closed(cx));
        }
        self.shown_dialog = dialog.clone();
        // A new dialog starts at its top.
        self.dialog_scroll.set_offset(point(px(0.0), px(0.0)));
        let (name, description) = match &dialog {
            Some(Dialog::CreatePlaylist { .. }) => {
                let count = app::library(cx).read(cx).playlists.len() + 1;
                (format!("My Playlist #{count}"), String::new())
            }
            Some(Dialog::EditPlaylist { id }) => match app::library(cx).read(cx).playlist(id) {
                Some(p) => (
                    p.name.to_string(),
                    p.description
                        .as_ref()
                        .map(|d| d.to_string())
                        .unwrap_or_default(),
                ),
                None => return,
            },
            _ => return,
        };
        self.dialog_name.update(cx, |input, cx| {
            input.set_value(name, window, cx);
            input.focus(window, cx);
        });
        self.dialog_description
            .update(cx, |input, cx| input.set_value(description, window, cx));
    }

    fn page(&mut self, route: &Route, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        if let Some(page) = self.pages.get(route) {
            return page.clone();
        }
        let page = crate::pages::build(route, window, cx);
        self.pages.insert(route.clone(), page.clone());
        page
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
}

impl Render for Lilac {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::model::set_display_scale(window.scale_factor());
        let status = app::session(cx).read(cx).status.clone();
        let route = app::nav(cx).read(cx).current().clone();
        let (immersive, sidebar_collapsed, panel) = {
            let ui = app::ui(cx);
            let ui = ui.read(cx);
            (ui.immersive, ui.sidebar_collapsed, ui.right_panel)
        };

        let content = match status {
            // Covered by the loading screen.
            Status::Starting => div().into_any_element(),
            Status::LoggedOut | Status::WaitingForBrowser => self.login.clone().into_any_element(),
            Status::LoggedIn if immersive => {
                crate::pages::immersive::render(window, cx).into_any_element()
            }
            Status::LoggedIn => {
                let page = self.page(&route, window, cx);
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
                                    .child(page.cached(StyleRefinement::default().size_full())),
                            )
                            .when(layout.panel, |row| {
                                row.child(
                                    theme::glass()
                                        .flex_none()
                                        .w(px(theme::RIGHT_PANEL_WIDTH))
                                        .h_full()
                                        .overflow_hidden()
                                        .child(cached(
                                            &self.right_panel,
                                            StyleRefinement::default().size_full(),
                                        )),
                                )
                            }),
                    )
                    .when(self.locked, |column| {
                        column.child(soloist_setup::locked_banner())
                    })
                    .child(cached(
                        &self.player_bar,
                        StyleRefinement::default()
                            .w_full()
                            .h(px(theme::PLAYER_BAR_HEIGHT))
                            .flex_none(),
                    ))
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
            .children(overlays::menu(&self.menu_scroll, cx))
            .children(overlays::dialog(
                &self.dialog_name,
                &self.dialog_description,
                &self.soloist_key,
                &self.dialog_scroll,
                cx,
            ))
            .children(overlays::toasts(cx))
            .children(self.splash.clone())
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
