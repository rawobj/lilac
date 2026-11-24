//! Artist page, laid out like the official apps: popular tracks, the songs
//! of theirs you liked, the discography by kind, releases they appear on,
//! similar artists and their biography.

use std::collections::HashSet;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{album_card, artist_card, scroll_page};
use crate::app;
use crate::library::SavedEntity;
use crate::menus;
use crate::model::*;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::card::shelf;
use crate::ui::track_row::{RowOptions, track_row};
use crate::ui::{self, Placeholder, icons};
use crate::ui_state::Dialog;

/// Which part of the discography is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shelf {
    Popular,
    Albums,
    Singles,
    Compilations,
}

impl Shelf {
    const ALL: [Shelf; 4] = [
        Shelf::Popular,
        Shelf::Albums,
        Shelf::Singles,
        Shelf::Compilations,
    ];

    fn label(self) -> &'static str {
        match self {
            Shelf::Popular => "Popular releases",
            Shelf::Albums => "Albums",
            Shelf::Singles => "Singles and EPs",
            Shelf::Compilations => "Compilations",
        }
    }
}

pub struct ArtistPage {
    id: SharedString,
    /// From the Web API, used when the streaming session has none.
    web_top_tracks: Vec<Track>,
    web_top_asked: bool,
    /// The Web API was asked for releases and similar artists, which the
    /// streaming session did not give.
    web_lists_asked: bool,
    web_related: Vec<Artist>,
    albums: Vec<Album>,
    show_all_tracks: bool,
    show_all_liked: bool,
    shelf: Shelf,
    scroll: ScrollHandle,
}

impl ArtistPage {
    pub fn new(id: SharedString, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&app::player(cx), |_, _, event: &PlayerEvent, cx| {
            // Rows show what plays and whether it is paused, not where in the track.
            if !matches!(event, PlayerEvent::Seeked) {
                cx.notify()
            }
        })
        .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();
        cx.observe(&app::artists(cx), |_, _, cx| cx.notify())
            .detach();
        app::artists(cx).update(cx, |s, cx| s.ensure(&id, cx));

        Self {
            id,
            web_top_tracks: Vec::new(),
            web_top_asked: false,
            web_lists_asked: false,
            web_related: Vec::new(),
            albums: Vec::new(),
            show_all_tracks: false,
            show_all_liked: false,
            shelf: Shelf::Popular,
            scroll: ScrollHandle::new(),
        }
    }

    /// The Web API's stand-in for top tracks, asked only when the
    /// streaming session had none.
    fn fetch_web_lists(&mut self, albums: bool, related: bool, cx: &mut Context<Self>) {
        let api = app::api(cx);
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            if albums {
                let (a, i) = (api.clone(), id.clone());
                if let Ok(albums) = runtime::run(async move { a.artist_albums(&i).await }).await {
                    this.update(cx, |this, cx| {
                        this.albums = albums;
                        cx.notify();
                    })
                    .ok();
                }
            }
            if related
                && let Ok(related) =
                    runtime::run(async move { api.related_artists(&id).await }).await
            {
                this.update(cx, |this, cx| {
                    this.web_related = related;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn fetch_web_top_tracks(&mut self, name: SharedString, cx: &mut Context<Self>) {
        let api = app::api(cx);
        let id = self.id.clone();
        cx.spawn(async move |this, cx| {
            let tracks = runtime::run(async move { api.artist_top_tracks(&id, &name).await }).await;
            if let Ok(tracks) = tracks {
                this.update(cx, |this, cx| {
                    this.web_top_tracks = tracks;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn top_tracks(&self, cx: &App) -> Vec<Track> {
        let details = app::artists(cx).read(cx).get(&self.id).cloned();
        match details.and_then(|d| d.extras).map(|e| e.top_tracks) {
            Some(tracks) if !tracks.is_empty() => tracks,
            _ => self.web_top_tracks.clone(),
        }
    }

    fn play(
        &self,
        context: Option<ContextUri>,
        name: SharedString,
        tracks: Vec<Track>,
        index: usize,
        cx: &mut App,
    ) {
        app::player(cx).update(cx, |player, cx| {
            player.play_context(context, name, tracks, index, cx)
        });
    }

    fn track_rows(
        &self,
        key: &'static str,
        tracks: &[Track],
        limit: usize,
        context: Option<ContextUri>,
        name: SharedString,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut rows = div().flex().flex_col();
        let all: Vec<Track> = tracks.to_vec();
        for (ix, track) in tracks.iter().take(limit).enumerate() {
            let (all, context, name) = (all.clone(), context.clone(), name.clone());
            let this = cx.entity().downgrade();
            rows = rows.child(track_row(
                (key, ix),
                track,
                RowOptions {
                    number: Some(ix as u32 + 1),
                    show_cover: true,
                    show_album: true,
                    ..Default::default()
                },
                move |_, cx| {
                    let (all, context, name) = (all.clone(), context.clone(), name.clone());
                    this.update(cx, |this, cx| this.play(context, name, all, ix, cx))
                        .ok();
                },
                window,
                cx,
            ));
        }
        rows
    }
}

fn see_more(id: &'static str, open: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_6()
        .py_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_muted())
        .cursor_pointer()
        .hover(|s| s.text_color(theme::text()))
        .child(if open { "Show less" } else { "See more" })
}

fn section(title: impl Into<SharedString>) -> Div {
    div().px_6().child(ui::section_title(title))
}

impl Render for ArtistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let details = app::artists(cx)
            .read(cx)
            .get(&self.id)
            .cloned()
            .unwrap_or_default();
        let Some(artist) = details.profile.clone() else {
            if details.profile_done {
                return ui::empty_state(
                    icons::CIRCLE_ALERT,
                    "Couldn't load this artist",
                    "Spotify didn't answer. Check your connection and try again.",
                );
            }
            return ui::loading_state();
        };
        let extras = details.extras.clone().unwrap_or_default();
        // The streaming session's names are untranslated; this artist's
        // own name comes from the profile ("Yorushika", not "ヨルシカ").
        let top_tracks: Vec<Track> = self
            .top_tracks(cx)
            .into_iter()
            .map(|mut t| {
                for a in &mut t.artists {
                    if a.id == artist.id {
                        a.name = artist.name.clone();
                    }
                }
                t
            })
            .collect();
        // Releases come from the streaming session, refreshed on each visit.
        let discography = &extras.discography;
        if !discography.is_empty()
            && (self.albums.len() != discography.len()
                || self.albums.first().map(|a| &a.id) != discography.first().map(|a| &a.id))
        {
            self.albums = discography.clone();
        }
        // The Web API (rate limited per app) only for what the session
        // could not give.
        if details.extras_done
            && (discography.is_empty() || extras.related.is_empty())
            && !self.web_lists_asked
        {
            self.web_lists_asked = true;
            self.fetch_web_lists(discography.is_empty(), extras.related.is_empty(), cx);
        }
        if details.extras_done && top_tracks.is_empty() && !self.web_top_asked {
            // Asked once: an empty answer stays empty.
            self.web_top_asked = true;
            self.fetch_web_top_tracks(artist.name.clone(), cx);
        }

        let following = app::library(cx).read(cx).is_saved(&artist.id);
        let context = ContextUri::Artist(self.id.clone());
        let (is_context, playing) = {
            let player = app::player(cx);
            let p = player.read(cx);
            (p.is_context(&context), p.playing)
        };
        let overview = details.overview.clone().unwrap_or_default();
        let overview_error = details.overview_error.clone();
        let followers = artist
            .followers
            .as_ref()
            .map(|f| f.total)
            .filter(|n| *n > 0)
            .or(overview.followers);
        let audience = match (overview.monthly_listeners, followers) {
            (Some(n), _) => Some(format!("{} monthly listeners", format_count(n))),
            (None, Some(n)) => Some(format!("{} followers", format_count(n))),
            _ => None,
        };
        let header = overview.header_image.clone().map(SharedString::from);
        let header_image = header.as_ref().and_then(|url| {
            let width = (f32::from(window.viewport_size().width) * window.scale_factor()) as u32;
            crate::images::ImageCache::get(url, width, cx)
        });

        // The header stays put behind the page, which scrolls over it; it
        // drifts up a little and fades as it is covered.
        let hero_height = if header_image.is_some() { 400.0 } else { 300.0 };
        let scrolled = (-f32::from(self.scroll.offset().y)).max(0.0);
        let covered = (scrolled / hero_height).clamp(0.0, 1.0);
        let backdrop = div()
            .absolute()
            .top(px(-scrolled * 0.3))
            .left_0()
            .right_0()
            .h(px(hero_height))
            .overflow_hidden()
            .opacity(1.0 - covered * 0.85)
            .child(div().absolute().size_full().map(|d| {
                // Without a header picture: the main color of their profile
                // picture, as Spotify uses it.
                let color = overview.color.map(|c| Hsla::from(rgb(c))).or_else(|| {
                    artist
                        .images
                        .for_size(160)
                        .and_then(|url| crate::images::ImageCache::backdrop(&url, cx))
                        .map(|b| b.glow)
                });
                match color {
                    Some(color) => d.bg(linear_gradient(
                        180.0,
                        linear_color_stop(color.opacity(0.9), 0.0),
                        linear_color_stop(color.opacity(0.3), 1.0),
                    )),
                    None => d.bg(theme::hero_gradient(theme::seed_of(&self.id))),
                }
            }))
            .children(header_image.map(|image| {
                img(ImageSource::Render(image))
                    .absolute()
                    .size_full()
                    .object_fit(ObjectFit::Cover)
            }))
            // Dark from the bottom up to the middle, so the white name reads
            // on light pictures too.
            .child(div().absolute().size_full().bg(linear_gradient(
                0.0,
                linear_color_stop(theme::black(0.78), 0.0),
                linear_color_stop(theme::black(0.0), 0.6),
            )));

        let hero = div().relative().h(px(hero_height)).child(
            div()
                .relative()
                .size_full()
                .px_6()
                .pb_6()
                .flex()
                .items_end()
                .gap_6()
                .child(ui::cover(
                    artist.images.for_size(200),
                    200.0,
                    100.0,
                    Placeholder::Artist,
                    window,
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                // A blue badge with a white tick, as Spotify
                                // marks its verified artists.
                                .child(
                                    div()
                                        .size(px(20.0))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(rgb(0x4cb3ff))
                                        .child(
                                            svg()
                                                .path(icons::CHECK)
                                                .size(px(12.0))
                                                .text_color(gpui_kit::white()),
                                        ),
                                )
                                .child("Verified Artist"),
                        )
                        .child(
                            div()
                                .line_clamp(2)
                                .text_size(px(52.0))
                                .line_height(px(60.0))
                                .font_weight(FontWeight::EXTRA_BOLD)
                                .child(artist.name.clone()),
                        )
                        .children(
                            audience
                                .clone()
                                .map(|line| div().text_color(theme::white(0.85)).child(line)),
                        )
                        .when(!artist.genres.is_empty(), |d| {
                            let genres: Vec<&str> =
                                artist.genres.iter().take(3).map(|g| g.as_ref()).collect();
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(theme::text_subtle())
                                    .child(genres.join(" · ")),
                            )
                        }),
                ),
        );

        let menu_artist = artist.clone();
        let follow_artist = artist.clone();
        let play_tracks = top_tracks.clone();
        let play_name = artist.name.clone();
        let play_context = context.clone();
        let actions = div()
            .h(px(80.0))
            .px_6()
            .flex()
            .items_center()
            .gap_4()
            .child(
                ui::play_button("play", 52.0, is_context && playing)
                    .when(top_tracks.is_empty(), |b| b.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_context {
                            app::player(cx).update(cx, |p, cx| p.toggle(cx));
                        } else if !play_tracks.is_empty() {
                            this.play(
                                Some(play_context.clone()),
                                play_name.clone(),
                                play_tracks.clone(),
                                0,
                                cx,
                            );
                        }
                    })),
            )
            .child(
                ui::pill_button(
                    "follow",
                    if following { "Following" } else { "Follow" },
                    false,
                )
                .on_click(move |_, _, cx| {
                    let item = SavedEntity::Artist(follow_artist.clone());
                    app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx))
                }),
            )
            .child(
                ui::icon_button("more", icons::ELLIPSIS)
                    .size(px(40.0))
                    .on_click(move |event, _, cx| {
                        menus::artist_menu(&menu_artist, event.position(), cx)
                    }),
            );

        let mut body = div().pb_8().flex().flex_col().gap_8().child(actions);

        // Popular.
        if !top_tracks.is_empty() {
            let limit = if self.show_all_tracks { 10 } else { 5 };
            let rows = self.track_rows(
                "top",
                &top_tracks,
                limit,
                Some(context.clone()),
                artist.name.clone(),
                window,
                cx,
            );
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section("Popular"))
                    .child(rows)
                    .when(top_tracks.len() > 5, |d| {
                        d.child(see_more("toggle-tracks", self.show_all_tracks).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.show_all_tracks = !this.show_all_tracks;
                                cx.notify();
                            }),
                        ))
                    }),
            );
        }

        // The songs of theirs you liked.
        let liked: Vec<Track> = app::library(cx)
            .read(cx)
            .liked
            .iter()
            .filter(|t| t.artists.iter().any(|a| a.id == self.id))
            .cloned()
            .collect();
        if !liked.is_empty() {
            let limit = if self.show_all_liked { liked.len() } else { 5 };
            let title = format!(
                "Liked songs · {} {}",
                liked.len(),
                if liked.len() == 1 { "song" } else { "songs" }
            );
            let rows = self.track_rows(
                "liked",
                &liked,
                limit,
                None,
                format!("Liked songs by {}", artist.name).into(),
                window,
                cx,
            );
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section(title))
                    .child(rows)
                    .when(liked.len() > 5, |d| {
                        d.child(see_more("toggle-liked", self.show_all_liked).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.show_all_liked = !this.show_all_liked;
                                cx.notify();
                            }),
                        ))
                    }),
            );
        }

        // Discography, one kind at a time.
        let popular_releases: Vec<Album> = {
            let mut seen = HashSet::new();
            top_tracks
                .iter()
                .filter_map(|t| t.album.as_ref())
                .filter(|a| !a.id.is_empty() && seen.insert(a.id.clone()))
                .map(|r| {
                    self.albums
                        .iter()
                        .find(|a| a.id == r.id)
                        .cloned()
                        .unwrap_or_else(|| Album {
                            id: r.id.clone(),
                            name: r.name.clone(),
                            images: r.images.clone(),
                            ..Default::default()
                        })
                })
                .collect()
        };
        let pick = |shelf: Shelf| -> Vec<Album> {
            match shelf {
                Shelf::Popular => popular_releases.clone(),
                Shelf::Albums => self
                    .albums
                    .iter()
                    .filter(|a| a.album_type == "album")
                    .cloned()
                    .collect(),
                Shelf::Singles => self
                    .albums
                    .iter()
                    .filter(|a| a.album_type == "single" || a.album_type == "ep")
                    .cloned()
                    .collect(),
                Shelf::Compilations => self
                    .albums
                    .iter()
                    .filter(|a| a.album_type == "compilation")
                    .cloned()
                    .collect(),
            }
        };
        let available: Vec<Shelf> = Shelf::ALL
            .into_iter()
            .filter(|s| !pick(*s).is_empty())
            .collect();
        if !available.is_empty() {
            let shelf = if available.contains(&self.shelf) {
                self.shelf
            } else {
                available[0]
            };
            let mut chips = div().px_6().flex().gap_2();
            for option in &available {
                let option = *option;
                chips = chips.child(
                    ui::chip(option.label(), option.label(), option == shelf).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.shelf = option;
                            cx.notify();
                        }),
                    ),
                );
            }
            let cards: Vec<AnyElement> = pick(shelf)
                .iter()
                .enumerate()
                .map(|(ix, a)| {
                    let subtitle = match (a.year(), a.album_type.is_empty()) {
                        ("", _) | (_, true) => a.type_label().to_string(),
                        (year, false) => format!("{year} · {}", a.type_label()),
                    };
                    album_card(ix, a, Some(subtitle.into()), window, cx)
                })
                .collect();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section("Discography"))
                    .child(chips)
                    .child(shelf_row("discography", cards)),
            );
        }

        if !extras.appears_on.is_empty() {
            let cards = extras
                .appears_on
                .iter()
                .enumerate()
                .map(|(ix, a)| album_card(100 + ix, a, None, window, cx))
                .collect();
            body = body.child(shelf("appears-on", "Appears on", cards));
        }

        let related = if extras.related.is_empty() {
            &self.web_related
        } else {
            &extras.related
        };
        if !related.is_empty() {
            let cards = related
                .iter()
                .take(12)
                .enumerate()
                .map(|(ix, a)| artist_card(ix, a, window, cx))
                .collect();
            body = body.child(shelf("related", "Fans also like", cards));
        }

        // On tour.
        if !overview.concerts.is_empty() {
            body = body.child(
                div()
                    .px_6()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(ui::section_title("On tour"))
                    .child(
                        div().max_w(px(820.0)).flex().flex_col().gap_3().children(
                            overview
                                .concerts
                                .iter()
                                .take(8)
                                .map(crate::panels::concert_row),
                        ),
                    ),
            );
        }

        // Playlists that brought listeners to them.
        if !overview.discovered_on.is_empty() {
            let cards = overview
                .discovered_on
                .iter()
                .enumerate()
                .map(|(ix, (id, name, image))| {
                    let playlist = Playlist {
                        id: id.clone().into(),
                        uri: format!("spotify:playlist:{id}").into(),
                        name: name.clone().into(),
                        images: Images {
                            medium: image.clone().map(Into::into),
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    super::playlist_card(200 + ix, &playlist, window, cx)
                })
                .collect();
            body = body.child(shelf("discovered-on", "Discovered on", cards));
        }

        // About: a card that opens the full story.
        let bio = overview.biography.clone().or(extras.biography.clone());
        if bio.is_some() || audience.is_some() || overview_error.is_some() {
            let picture = overview
                .gallery
                .first()
                .cloned()
                .or(overview.header_image.clone())
                .map(SharedString::from)
                .or_else(|| artist.images.for_size(640));
            let width = 720.0;
            let blurred = picture
                .as_ref()
                .and_then(|url| crate::images::ImageCache::backdrop(url, cx));
            let image = picture.and_then(|url| {
                crate::images::ImageCache::get(&url, (width * window.scale_factor()) as u32, cx)
            });
            let id = self.id.clone();
            body = body.child(
                div()
                    .px_6()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(ui::section_title("About"))
                    .child(
                        div()
                            .id("about-card")
                            .relative()
                            .w_full()
                            .max_w(px(width))
                            .h(px(380.0))
                            .rounded(px(theme::RADIUS_CARD))
                            .overflow_hidden()
                            .cursor_pointer()
                            .bg(theme::glass_fill_strong())
                            // The whole picture, not a crop, on a blurred
                            // copy of itself.
                            .children(blurred.map(|b| {
                                img(ImageSource::Render(b.image))
                                    .absolute()
                                    .size_full()
                                    .object_fit(ObjectFit::Cover)
                            }))
                            .children(image.map(|image| {
                                img(ImageSource::Render(image))
                                    .absolute()
                                    .size_full()
                                    .object_fit(ObjectFit::Contain)
                            }))
                            .child(div().absolute().size_full().bg(linear_gradient(
                                0.0,
                                linear_color_stop(theme::black(0.85), 0.0),
                                linear_color_stop(theme::black(0.0), 0.65),
                            )))
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_0()
                                    .p_6()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(audience.clone().map(|line| {
                                        div().text_lg().font_weight(FontWeight::BOLD).child(line)
                                    }))
                                    .children(bio.map(|bio| {
                                        div()
                                            .text_sm()
                                            .text_color(theme::white(0.85))
                                            .child(crate::panels::excerpt(&bio, 180))
                                    })),
                            )
                            .hover(|s| s.opacity(0.95))
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                app::ui(cx).update(cx, |ui, cx| {
                                    ui.open_dialog(Dialog::ArtistAbout { id }, cx)
                                })
                            }),
                    ),
            );
        }

        // The page's own background, so it covers the header as it rises.
        let content = div().child(hero).child(body.bg(linear_gradient(
            180.0,
            linear_color_stop(theme::backdrop_opaque().opacity(0.82), 0.0),
            linear_color_stop(theme::backdrop_opaque(), 0.12),
        )));
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(|_, _: &ScrollEvent, _, cx| cx.notify()))
            .child(backdrop)
            .child(scroll_page("artist", &self.scroll, content))
    }
}

/// A row of cards without its own title (the discography's title and
/// chips sit above it).
fn shelf_row(key: &'static str, cards: Vec<AnyElement>) -> AnyElement {
    shelf(key, "", cards).into_any_element()
}
