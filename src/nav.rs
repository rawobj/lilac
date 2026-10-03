//! Navigation history, browser style.

use gpui_kit::{Context, SharedString};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Route {
    Home,
    Search,
    Liked,
    Playlist(SharedString),
    Album(SharedString),
    Artist(SharedString),
    Show(SharedString),
    /// Search results for a browse tile, e.g. a genre.
    Genre(SharedString),
    Profile,
    Lyrics,
    Settings,
}

const MAX_HISTORY: usize = 64;

pub struct Nav {
    stack: Vec<Route>,
    index: usize,
}

impl Nav {
    pub fn new() -> Self {
        Self {
            stack: vec![Route::Home],
            index: 0,
        }
    }

    pub fn current(&self) -> &Route {
        &self.stack[self.index]
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.stack.len()
    }

    /// Routes still reachable through back/forward. Pages for anything else can be dropped.
    pub fn retained(&self) -> &[Route] {
        &self.stack
    }

    pub fn go(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.push(route) {
            cx.notify();
        }
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.can_go_back() {
            self.index -= 1;
            cx.notify();
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if self.can_go_forward() {
            self.index += 1;
            cx.notify();
        }
    }

    /// Open or close a route that behaves like a toggle (lyrics, settings).
    pub fn toggle(&mut self, route: Route, cx: &mut Context<Self>) {
        if *self.current() == route {
            self.back(cx);
        } else {
            self.go(route, cx);
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.stack = vec![Route::Home];
        self.index = 0;
        cx.notify();
    }

    fn push(&mut self, route: Route) -> bool {
        if *self.current() == route {
            return false;
        }
        self.stack.truncate(self.index + 1);
        self.stack.push(route);
        if self.stack.len() > MAX_HISTORY {
            self.stack.remove(0);
        }
        self.index = self.stack.len() - 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_behaves_like_a_browser() {
        let mut nav = Nav::new();
        assert!(nav.push(Route::Search));
        assert!(nav.push(Route::Liked));
        assert!(!nav.push(Route::Liked), "same route is not pushed twice");
        assert!(nav.can_go_back() && !nav.can_go_forward());

        nav.index -= 1;
        assert_eq!(*nav.current(), Route::Search);
        assert!(nav.can_go_forward());

        // Navigating from the middle drops the forward entries.
        assert!(nav.push(Route::Settings));
        assert!(!nav.can_go_forward());
        assert_eq!(
            nav.retained(),
            &[Route::Home, Route::Search, Route::Settings]
        );
    }

    #[test]
    fn history_is_bounded() {
        let mut nav = Nav::new();
        for i in 0..(MAX_HISTORY * 2) {
            nav.push(Route::Album(i.to_string().into()));
        }
        assert_eq!(nav.retained().len(), MAX_HISTORY);
        assert_eq!(nav.index, MAX_HISTORY - 1);
    }
}
