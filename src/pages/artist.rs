//! Artist page: popular tracks, discography and related artists.

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

pub struct ArtistPage {
    id: SharedString,
    artist: Option<Artist>,
    top_tracks: Vec<Track>,
    albums: Vec<Album>,
    related: Vec<Artist>,
    show_all_tracks: bool,
    error: Option<SharedString>,
    scroll: ScrollHandle,
}

impl ArtistPage {
    pub fn new(id: SharedString, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&app::player(cx), |_, _, _: &PlayerEvent, cx| cx.notify())
            .detach();
        cx.observe(&app::library(cx), |_, _, cx| cx.notify())
            .detach();

        let api = app::api(cx);
        let fetch_id = id.clone();
        cx.spawn(async move |this, cx| {
            // The header needs the artist first; the rest fills in behind it.
            let (a, i) = (api.clone(), fetch_id.clone());
            let artist = runtime::run(async move { a.artist(&i).await }).await;
            let name = match artist {
                Ok(artist) => {
                    let name = artist.name.clone();
                    this.update(cx, |this, cx| {
                        this.artist = Some(artist);
                        cx.notify();
                    })
                    .ok();
                    name
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(format!("{err}").into());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let (a, i) = (api.clone(), fetch_id.clone());
            let top = runtime::spawn(async move { a.artist_top_tracks(&i, &name).await });
            let (a, i) = (api.clone(), fetch_id.clone());
            let albums = runtime::spawn(async move { a.artist_albums(&i).await });
            let (a, i) = (api.clone(), fetch_id.clone());
            let related = runtime::spawn(async move { a.related_artists(&i).await });

            if let Ok(Ok(tracks)) = top.await {
                this.update(cx, |this, cx| {
                    this.top_tracks = tracks;
                    cx.notify();
                })
                .ok();
            }
            if let Ok(Ok(albums)) = albums.await {
                this.update(cx, |this, cx| {
                    this.albums = albums;
                    cx.notify();
                })
                .ok();
            }
            if let Ok(Ok(related)) = related.await {
                this.update(cx, |this, cx| {
                    this.related = related;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();

        Self {
            id,
            artist: None,
            top_tracks: Vec::new(),
            albums: Vec::new(),
            related: Vec::new(),
            show_all_tracks: false,
            error: None,
            scroll: ScrollHandle::new(),
        }
    }

    fn play(&self, index: usize, cx: &mut App) {
        let name = self
            .artist
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default();
        let (id, tracks) = (self.id.clone(), self.top_tracks.clone());
        app::player(cx).update(cx, |player, cx| {
            player.play_context(Some(ContextUri::Artist(id)), name, tracks, index, cx)
        });
    }
}

impl Render for ArtistPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.error {
            return ui::empty_state(
                icons::CIRCLE_ALERT,
                "Couldn't load this artist",
                error.clone(),
            );
        }
        let Some(artist) = self.artist.clone() else {
            return ui::loading_state();
        };

        let following = app::library(cx).read(cx).is_saved(&artist.id);
        let context = ContextUri::Artist(self.id.clone());
        let (is_context, playing) = {
            let player = app::player(cx);
            let p = player.read(cx);
            (p.is_context(&context), p.playing)
        };
        let followers = artist
            .followers
            .as_ref()
            .map(|f| f.total)
            .filter(|n| *n > 0);

        let hero = div()
            .relative()
            .h(px(300.0))
            .child(
                div()
                    .absolute()
                    .size_full()
                    .bg(theme::hero_gradient(theme::seed_of(&self.id))),
            )
            .child(
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
                                    .text_xs()
                                    .child(
                                        svg()
                                            .path(icons::CIRCLE_CHECK)
                                            .size(px(16.0))
                                            .text_color(theme::accent()),
                                    )
                                    .child("Artist"),
                            )
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_size(px(52.0))
                                    .line_height(px(60.0))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .child(artist.name.clone()),
                            )
                            .children(followers.map(|n| {
                                div()
                                    .text_color(theme::text_muted())
                                    .child(format!("{} followers", format_count(n)))
                            }))
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
        let actions = div()
            .h(px(80.0))
            .px_6()
            .flex()
            .items_center()
            .gap_4()
            .child(
                ui::play_button("play", 52.0, is_context && playing)
                    .when(self.top_tracks.is_empty(), |b| b.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_context {
                            app::player(cx).update(cx, |p, cx| p.toggle(cx));
                        } else {
                            this.play(0, cx);
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

        let mut body = div()
            .pb_8()
            .flex()
            .flex_col()
            .gap_6()
            .child(div().child(hero).child(actions));

        if !self.top_tracks.is_empty() {
            let limit = if self.show_all_tracks { 10 } else { 5 };
            let mut rows = div().flex().flex_col();
            let this = cx.entity().downgrade();
            for (ix, track) in self.top_tracks.iter().take(limit).enumerate() {
                let this = this.clone();
                rows = rows.child(track_row(
                    ("top", ix),
                    track,
                    RowOptions {
                        number: Some(ix as u32 + 1),
                        show_cover: true,
                        show_album: true,
                        ..Default::default()
                    },
                    move |_, cx| {
                        this.update(cx, |this, cx| this.play(ix, cx)).ok();
                    },
                    window,
                    cx,
                ));
            }
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().px_6().child(ui::section_title("Popular")))
                    .child(rows)
                    .when(self.top_tracks.len() > 5, |d| {
                        d.child(
                            div()
                                .id("toggle-tracks")
                                .px_6()
                                .py_2()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text_muted())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()))
                                .child(if self.show_all_tracks {
                                    "Show less"
                                } else {
                                    "See more"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_all_tracks = !this.show_all_tracks;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }

        for (key, title, kinds) in [
            ("albums", "Albums", &["album"][..]),
            ("singles", "Singles and EPs", &["single", "ep"][..]),
            ("compilations", "Compilations", &["compilation"][..]),
        ] {
            let cards: Vec<AnyElement> = self
                .albums
                .iter()
                .filter(|a| kinds.contains(&a.album_type.as_ref()))
                .enumerate()
                .map(|(ix, a)| {
                    let subtitle = format!("{} · {}", a.year(), a.type_label());
                    album_card(ix, a, Some(subtitle.into()), window, cx)
                })
                .collect();
            if !cards.is_empty() {
                body = body.child(shelf(key, title, cards));
            }
        }

        if !self.related.is_empty() {
            let cards = self
                .related
                .iter()
                .take(12)
                .enumerate()
                .map(|(ix, a)| artist_card(ix, a, window, cx))
                .collect();
            body = body.child(shelf("related", "Fans also like", cards));
        }

        scroll_page("artist", &self.scroll, body)
    }
}
