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
    WhatsNew,
    Lyrics,
    Settings,
}

impl Route {
    /// Parse a `spotify:` URI or an `open.spotify.com` link.
    pub fn from_link(link: &str) -> Option<Route> {
        let link = link.trim();
        let (kind, id) = if let Some(rest) = link.strip_prefix("spotify:") {
            let mut parts = rest.split(':');
            (parts.next()?, parts.next().unwrap_or(""))
        } else {
            let rest = link
                .strip_prefix("https://open.spotify.com/")
                .or_else(|| link.strip_prefix("http://open.spotify.com/"))?;
            let path = rest.split(['?', '#']).next()?;
            // Localized links look like /intl-de/album/<id>.
            let mut parts = path
                .split('/')
                .filter(|p| !p.is_empty() && !p.starts_with("intl-"));
            (parts.next()?, parts.next().unwrap_or(""))
        };
        let id = || (!id.is_empty()).then(|| SharedString::from(id.to_string()));
        Some(match kind {
            "album" => Route::Album(id()?),
            "playlist" => Route::Playlist(id()?),
            "artist" => Route::Artist(id()?),
            "show" => Route::Show(id()?),
            "collection" => Route::Liked,
            "search" => Route::Search,
            _ => return None,
        })
    }
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
    fn links_parse_into_routes() {
        assert_eq!(
            Route::from_link("spotify:album:abc"),
            Some(Route::Album("abc".into()))
        );
        assert_eq!(
            Route::from_link("https://open.spotify.com/playlist/37i9?si=xyz"),
            Some(Route::Playlist("37i9".into()))
        );
        assert_eq!(
            Route::from_link("https://open.spotify.com/intl-de/artist/4Z8W"),
            Some(Route::Artist("4Z8W".into()))
        );
        assert_eq!(Route::from_link("spotify:collection"), Some(Route::Liked));
        assert_eq!(Route::from_link("spotify:album:"), None);
        assert_eq!(Route::from_link("https://example.com/album/abc"), None);
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
