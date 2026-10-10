//! Search results, and the browse grid shown before anything is typed.

use gpui_kit::*;

use super::{album_card, artist_card, playlist_card, scroll_page, show_card};
use crate::actions;
use crate::app;
use crate::model::*;
use crate::nav::Route;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::card::shelf;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Songs,
    Artists,
    Albums,
    Playlists,
    Podcasts,
}

const TABS: [(Tab, &str); 6] = [
    (Tab::All, "All"),
    (Tab::Songs, "Songs"),
    (Tab::Artists, "Artists"),
    (Tab::Albums, "Albums"),
    (Tab::Playlists, "Playlists"),
    (Tab::Podcasts, "Podcasts & Shows"),
];

/// Tiles of the browse grid. Each opens a search scoped to that genre or mood.
const GENRES: &[&str] = &[
    "Pop",
    "Hip-Hop",
    "Rock",
    "Indie",
    "Electronic",
    "R&B",
    "Jazz",
    "Classical",
    "Lo-fi",
    "Metal",
    "Country",
    "Latin",
    "K-Pop",
    "Soul",
    "Folk",
    "Punk",
    "Ambient",
    "House",
    "Techno",
    "Reggae",
    "Blues",
    "Funk",
    "Chill",
    "Workout",
    "Focus",
    "Sleep",
    "Party",
    "Romance",
    "Soundtracks",
    "Podcasts",
];

pub struct SearchPage {
    /// Set for genre pages, which run their own fixed query.
    genre: Option<SharedString>,
    genre_results: Option<SearchResults>,
    tab: Tab,
    scroll: ScrollHandle,
}

impl SearchPage {
    pub fn new(genre: Option<SharedString>, cx: &mut Context<Self>) -> Self {
        cx.observe(&app::search(cx), |_, _, cx| cx.notify())
            .detach();
        cx.subscribe(&app::player(cx), |_, _, _: &PlayerEvent, cx| cx.notify())
            .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();

        if let Some(genre) = genre.clone() {
            let api = app::api(cx);
            cx.spawn(async move |this, cx| {
                let result = runtime::run(async move {
                    api.search(&genre, "playlist,album,artist,track", 10).await
                })
                .await;
                this.update(cx, |this, cx| {
                    this.genre_results = Some(result.unwrap_or_default());
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }

        Self {
            genre,
            genre_results: None,
            tab: Tab::All,
            scroll: ScrollHandle::new(),
        }
    }

    fn browse(&self, window: &Window, cx: &App) -> Div {
        let columns = (crate::views::layout(window, cx).main / 190.0).clamp(2.0, 6.0) as u16;
        let mut grid = div().px_6().grid().grid_cols(columns).gap_3();
        for (ix, genre) in GENRES.iter().enumerate() {
            let seed = theme::seed_of(genre);
            let hue = (seed % 360) as f32 / 360.0;
            grid = grid.child(
                div()
                    .id(("genre", ix))
                    .h(px(96.0))
                    .p_3()
                    .rounded(px(theme::RADIUS_CARD))
                    .overflow_hidden()
                    .cursor_pointer()
                    .bg(linear_gradient(
                        135.0,
                        linear_color_stop(hsla(hue, 0.55, 0.42, 0.9), 0.0),
                        linear_color_stop(hsla((hue + 0.08).fract(), 0.6, 0.26, 0.9), 1.0),
                    ))
                    .hover(|s| s.opacity(0.88))
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child(*genre)
                    .on_click(move |_, _, cx| app::navigate(Route::Genre((*genre).into()), cx)),
            );
        }
        div()
            .py_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().px_6().child(ui::section_title("Browse all")))
            .child(grid)
    }

    fn songs(&self, tracks: &[Track], limit: usize, window: &Window, cx: &mut App) -> Div {
        let mut rows = div().flex().flex_col();
        for (ix, track) in tracks.iter().take(limit).enumerate() {
            let all = tracks.to_vec();
            rows = rows.child(track_row(
                ("song", ix),
                track,
                RowOptions {
                    number: None,
                    show_cover: true,
                    show_album: limit > 4,
                    ..Default::default()
                },
                move |_, cx| {
                    let tracks = all.clone();
                    app::player(cx).update(cx, |player, cx| {
                        player.play_context(
                            Some(ContextUri::Tracks("search".into())),
                            "Search results",
                            tracks,
                            ix,
                            cx,
                        )
                    });
                },
                window,
                cx,
            ));
        }
        rows
    }

    /// The large card for the best match, shown next to the top songs.
    fn top_result(
        &self,
        results: &SearchResults,
        window: &Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let (title, kind, image, round, placeholder, route, play): (
            SharedString,
            &str,
            Option<SharedString>,
            bool,
            Placeholder,
            Route,
            Box<dyn Fn(&mut App)>,
        ) = if let Some(artist) = results.artists.items.first() {
            let (id, name) = (artist.id.clone(), artist.name.clone());
            (
                artist.name.clone(),
                "Artist",
                artist.images.for_size(112),
                true,
                Placeholder::Artist,
                Route::Artist(artist.id.clone()),
                Box::new(move |cx| actions::play_artist(id.clone(), name.clone(), cx)),
            )
        } else if let Some(album) = results.albums.items.first() {
            let id = album.id.clone();
            (
                album.name.clone(),
                "Album",
                album.images.for_size(112),
                false,
                Placeholder::Music,
                Route::Album(album.id.clone()),
                Box::new(move |cx| actions::play_album(id.clone(), cx)),
            )
        } else {
            return None;
        };

        Some(
            div()
                .id("top-result")
                .group("top")
                .relative()
                .w(px(360.0))
                .h(px(224.0))
                .p_5()
                .flex()
                .flex_col()
                .gap_4()
                .rounded(px(theme::RADIUS_CARD))
                .cursor_pointer()
                .bg(theme::glass_fill_strong())
                .hover(|s| s.bg(theme::active_fill()))
                .child(ui::cover(
                    image,
                    96.0,
                    if round { 48.0 } else { 8.0 },
                    placeholder,
                    window,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .truncate()
                                .text_2xl()
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                        .child(div().text_color(theme::text_muted()).child(kind)),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(20.0))
                        .bottom(px(20.0))
                        .opacity(0.0)
                        .group_hover("top", |s| s.opacity(1.0))
                        .child(
                            ui::play_button("play", 48.0, false).on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                play(cx);
                            }),
                        ),
                )
                .on_click(move |_, _, cx| app::navigate(route.clone(), cx))
                .into_any_element(),
        )
    }

    fn results(&self, results: &SearchResults, window: &Window, cx: &mut App) -> Div {
        let mut body = div().pb_8().flex().flex_col().gap_6();
        let tab = self.tab;
        let want = |t: Tab| tab == Tab::All || tab == t;

        if tab == Tab::All && !results.tracks.items.is_empty() {
            body = body.child(
                div()
                    .px_3()
                    .flex()
                    .gap_3()
                    .children(self.top_result(results, window, cx).map(|top| {
                        div()
                            .px_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(ui::section_title("Top result"))
                            .child(top)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().px_6().child(ui::section_title("Songs")))
                            .child(self.songs(&results.tracks.items, 4, window, cx)),
                    ),
            );
        } else if tab == Tab::Songs {
            body = body.child(self.songs(&results.tracks.items, 50, window, cx));
        }

        if want(Tab::Artists) && !results.artists.items.is_empty() {
            let cards = results
                .artists
                .items
                .iter()
                .enumerate()
                .map(|(ix, a)| artist_card(ix, a, window, cx))
                .collect();
            body = body.child(shelf("artists", "Artists", cards));
        }
        if want(Tab::Albums) && !results.albums.items.is_empty() {
            let cards = results
                .albums
                .items
                .iter()
                .enumerate()
                .map(|(ix, a)| album_card(ix, a, None, window, cx))
                .collect();
            body = body.child(shelf("albums", "Albums", cards));
        }
        if want(Tab::Playlists) && !results.playlists.items.is_empty() {
            let cards = results
                .playlists
                .items
                .iter()
                .enumerate()
                .map(|(ix, p)| playlist_card(ix, p, window, cx))
                .collect();
            body = body.child(shelf("playlists", "Playlists", cards));
        }
        if want(Tab::Podcasts) && !results.shows.items.is_empty() {
            let cards = results
                .shows
                .items
                .iter()
                .enumerate()
                .map(|(ix, s)| show_card(ix, s, window, cx))
                .collect();
            body = body.child(shelf("shows", "Podcasts & Shows", cards));
        }
        if want(Tab::Podcasts) && !results.episodes.items.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().px_6().child(ui::section_title("Episodes")))
                    .child(self.songs(&results.episodes.items, 6, window, cx)),
            );
        }
        body
    }
}

impl Render for SearchPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search = app::search(cx);
        let (query, loading, error, live) = {
            let s = search.read(cx);
            (
                s.query.clone(),
                s.loading,
                s.error.clone(),
                s.results.clone(),
            )
        };

        let (heading, results, loading) = match &self.genre {
            Some(genre) => (
                Some(genre.clone()),
                self.genre_results.clone(),
                self.genre_results.is_none(),
            ),
            None => (None, live, loading),
        };

        let content = if self.genre.is_none() && query.is_empty() {
            self.browse(window, cx).into_any_element()
        } else if let Some(error) = error.filter(|_| self.genre.is_none()) {
            ui::empty_state(icons::CIRCLE_ALERT, "Search failed", error).into_any_element()
        } else {
            match results {
                None if loading => ui::loading_state().into_any_element(),
                None => div().into_any_element(),
                Some(results) if results.is_empty() => ui::empty_state(
                    icons::SEARCH,
                    format!("No results found for “{query}”"),
                    "Check the spelling, or try fewer or different keywords.",
                )
                .into_any_element(),
                Some(results) => {
                    let mut tabs = div().px_6().py_4().flex().gap_2();
                    for (tab, label) in TABS {
                        tabs = tabs.child(ui::chip(label, label, self.tab == tab).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.tab = tab;
                                cx.notify();
                            }),
                        ));
                    }
                    div()
                        .flex()
                        .flex_col()
                        .children(heading.map(|h| {
                            div()
                                .px_6()
                                .pt_6()
                                .text_3xl()
                                .font_weight(FontWeight::BOLD)
                                .child(h)
                        }))
                        .child(tabs)
                        .child(self.results(&results, window, cx))
                        .into_any_element()
                }
            }
        };

        scroll_page("search", &self.scroll, content)
    }
}
