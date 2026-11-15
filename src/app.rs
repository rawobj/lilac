//! Application-wide state handles.
//!
//! Each concern lives in its own entity so a change only re-renders the views
//! that observe it: a seek tick repaints the player bar, not the library.

use gpui_kit::*;

use crate::feed::Feed;
use crate::friends::Friends;
use crate::library::Library;
use crate::nav::{Nav, Route};
use crate::player::Player;
use crate::search::Search;
use crate::session::Session;
use crate::settings::Settings;
use crate::spotify::api::Api;
use crate::ui_state::UiState;

pub struct AppState {
    pub session: Entity<Session>,
    pub library: Entity<Library>,
    pub player: Entity<Player>,
    pub nav: Entity<Nav>,
    pub ui: Entity<UiState>,
    pub search: Entity<Search>,
    pub settings: Entity<Settings>,
    pub feed: Entity<Feed>,
    pub friends: Entity<Friends>,
}

impl Global for AppState {}

impl AppState {
    pub fn init(cx: &mut App) {
        let settings = cx.new(|_| Settings::load());
        let stored = settings.read(cx).clone();

        let session = cx.new(Session::new);
        let nav = cx.new(|_| Nav::new());
        let ui = cx.new(|_| UiState::new(stored.sidebar_collapsed, stored.show_now_playing));
        let library = cx.new(|cx| Library::new(&session, cx));
        let player = cx.new(|cx| Player::new(&session, &stored, cx));

        let search = cx.new(|_| Search::default());
        let feed = cx.new(|cx| Feed::new(&session, cx));
        let friends = cx.new(|cx| Friends::new(&ui, cx));

        // `lilac spotify:album:…` or an open.spotify.com link opens that page directly.
        if let Some(route) = std::env::args()
            .skip(1)
            .find_map(|arg| Route::from_link(&arg))
        {
            nav.update(cx, |nav, cx| nav.go(route, cx));
        }

        cx.set_global(AppState {
            session,
            library,
            player: player.clone(),
            nav,
            ui,
            search,
            settings,
            feed,
            friends,
        });
        // Connect to Spotify while the stored login is still being checked.
        player.update(cx, |player, cx| player.start_with_stored_login(cx));
    }
}

pub fn session(cx: &App) -> Entity<Session> {
    cx.global::<AppState>().session.clone()
}

pub fn library(cx: &App) -> Entity<Library> {
    cx.global::<AppState>().library.clone()
}

pub fn player(cx: &App) -> Entity<Player> {
    cx.global::<AppState>().player.clone()
}

pub fn nav(cx: &App) -> Entity<Nav> {
    cx.global::<AppState>().nav.clone()
}

pub fn ui(cx: &App) -> Entity<UiState> {
    cx.global::<AppState>().ui.clone()
}

pub fn search(cx: &App) -> Entity<Search> {
    cx.global::<AppState>().search.clone()
}

pub fn settings(cx: &App) -> Entity<Settings> {
    cx.global::<AppState>().settings.clone()
}

pub fn feed(cx: &App) -> Entity<Feed> {
    cx.global::<AppState>().feed.clone()
}

pub fn friends(cx: &App) -> Entity<Friends> {
    cx.global::<AppState>().friends.clone()
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
