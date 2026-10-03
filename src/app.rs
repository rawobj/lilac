//! Application-wide state handles.
//!
//! Each concern lives in its own entity so a change only re-renders the views
//! that observe it: a seek tick repaints the player bar, not the library.

use gpui_kit::*;

use crate::nav::{Nav, Route};
use crate::session::Session;
use crate::settings::Settings;
use crate::spotify::api::Api;
use crate::ui_state::UiState;

pub struct AppState {
    pub session: Entity<Session>,
    pub nav: Entity<Nav>,
    pub ui: Entity<UiState>,
    pub settings: Entity<Settings>,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) {
        let settings = cx.new(|_| Settings::load());
        let stored = settings.read(cx).clone();

        let session = cx.new(Session::new);
        let nav = cx.new(|_| Nav::new());
        let ui = cx.new(|_| UiState::new(stored.sidebar_collapsed, stored.show_now_playing));

        cx.set_global(AppState {
            session,
            nav,
            ui,
            settings,
        });
    }
}

pub fn session(cx: &App) -> Entity<Session> {
    cx.global::<AppState>().session.clone()
}

pub fn nav(cx: &App) -> Entity<Nav> {
    cx.global::<AppState>().nav.clone()
}

pub fn ui(cx: &App) -> Entity<UiState> {
    cx.global::<AppState>().ui.clone()
}

pub fn settings(cx: &App) -> Entity<Settings> {
    cx.global::<AppState>().settings.clone()
}

pub fn api(cx: &App) -> Api {
    cx.global::<AppState>().session.read(cx).api.clone()
}

pub fn navigate(route: Route, cx: &mut App) {
    nav(cx).update(cx, |nav, cx| nav.go(route, cx));
}

pub fn toast(text: impl Into<SharedString>, cx: &mut App) {
    let text = text.into();
    ui(cx).update(cx, |ui, cx| ui.toast(text, cx));
}

pub fn toast_error(text: impl Into<SharedString>, cx: &mut App) {
    let text = text.into();
    ui(cx).update(cx, |ui, cx| ui.error(text, cx));
}

/// Change a setting and persist it.
pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    settings(cx).update(cx, |settings, cx| {
        change(settings);
        settings.save();
        cx.notify();
    });
}
