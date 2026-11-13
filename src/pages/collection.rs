//! Playlist, album and Liked Songs pages.
//!
//! All three are a hero header followed by a track list, so they share one
//! view. The list is virtualized: a ten-thousand-track playlist costs the same
//! to draw as a ten-track one.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app;
use crate::library::{LibraryEvent, SavedEntity};
use crate::menus::{self, TrackMenuContext};
use crate::model::*;
use crate::nav::Route;
use crate::player::PlayerEvent;
use crate::runtime;
use crate::theme;
use crate::ui::track_row::{RowOptions, track_header, track_row};
use crate::ui::{self, Placeholder, WithTooltip, icons};

#[derive(Clone, PartialEq)]
pub enum Source {
    Playlist(SharedString),
    Album(SharedString),
    Liked,
}

/// Rows above the tracks: hero, action bar, column header.
const HEADER_ROWS: usize = 3;

pub struct CollectionPage {
    source: Source,
    playlist: Option<Playlist>,
    album: Option<Album>,
    tracks: Vec<Track>,
    loading: bool,
    error: Option<SharedString>,
    list: ListState,
    generation: u64,
}

impl CollectionPage {
    pub fn new(source: Source, cx: &mut Context<Self>) -> Self {
        let library = app::library(cx);
        cx.subscribe(&app::player(cx), |_, _, event: &PlayerEvent, cx| {
            // Rows show what plays and whether it is paused, not where in the track.
            if !matches!(event, PlayerEvent::Seeked) {
                cx.notify()
            }
        })
        .detach();
        cx.observe(&library, |this: &mut Self, _, cx| {
            if this.source == Source::Liked {
                this.sync_liked(cx);
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(&library, |this: &mut Self, _, event: &LibraryEvent, cx| {
            let LibraryEvent::PlaylistChanged(id) = event;
            if this.source == Source::Playlist(id.clone()) {
                this.load(cx);
            }
        })
        .detach();

        let mut this = Self {
            source,
            playlist: None,
            album: None,
            tracks: Vec::new(),
            loading: true,
            error: None,
            list: ListState::new(HEADER_ROWS, ListAlignment::Top, px(600.0)),
            generation: 0,
        };
        this.load(cx);
        this
    }

    fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.tracks = tracks;
        self.list.reset(HEADER_ROWS + self.tracks.len());
    }

    fn append_tracks(&mut self, tracks: Vec<Track>) {
        let old = HEADER_ROWS + self.tracks.len();
        self.tracks.extend(tracks);
        self.list
            .splice(old..old, HEADER_ROWS + self.tracks.len() - old);
    }

    fn sync_liked(&mut self, cx: &mut Context<Self>) {
        let library = app::library(cx);
        let library = library.read(cx);
        self.loading = library.liked_loading && library.liked.is_empty();
        if library.liked.len() != self.tracks.len()
            || library.liked.first().map(|t| &t.id) != self.tracks.first().map(|t| &t.id)
        {
            let scroll = self.list.logical_scroll_top();
            self.set_tracks(library.liked.clone());
            self.list.scroll_to(scroll);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.error = None;
        let api = app::api(cx);

        match self.source.clone() {
            Source::Liked => self.sync_liked(cx),
            Source::Album(id) => {
                self.loading = true;
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move {
                        let album = api.album(&id).await?;
                        let tracks = api.album_tracks(&album).await?;
                        Ok((album, tracks))
                    })
                    .await;
                    this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        this.loading = false;
                        match result {
                            Ok((album, tracks)) => {
                                this.album = Some(album);
                                this.set_tracks(tracks);
                            }
                            Err(err) => this.error = Some(format!("{err}").into()),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            Source::Playlist(id) => {
                self.loading = self.tracks.is_empty();
                // Show what the library already knows while the full object loads.
                if self.playlist.is_none() {
                    self.playlist = app::library(cx).read(cx).playlist(&id).cloned();
                }
                cx.spawn(async move |this, cx| {
                    let (meta_api, meta_id) = (api.clone(), id.clone());
                    let meta = runtime::run(async move { meta_api.playlist(&meta_id).await }).await;
                    let alive = this
                        .update(cx, |this, cx| {
                            if this.generation != generation {
                                return false;
                            }
                            match meta {
                                Ok(playlist) => this.playlist = Some(playlist),
                                Err(err) if this.playlist.is_none() => {
                                    this.error = Some(format!("{err}").into());
                                    this.loading = false;
                                }
                                Err(_) => {}
                            }
                            cx.notify();
                            this.error.is_none()
                        })
                        .unwrap_or(false);
                    if !alive {
                        return;
                    }

                    // Page through the items; rows appear as each page lands.
                    let mut offset = 0;
                    loop {
                        let (page_api, page_id) = (api.clone(), id.clone());
                        let page =
                            runtime::run(
                                async move { page_api.playlist_items(&page_id, offset).await },
                            )
                            .await;
                        let more = this
                            .update(cx, |this, cx| {
                                if this.generation != generation {
                                    return false;
                                }
                                this.loading = false;
                                let more = match page {
                                    Ok(page) => {
                                        let more = page.next.is_some() && !page.items.is_empty();
                                        let tracks: Vec<Track> =
                                            page.items.into_iter().filter_map(|i| i.item).collect();
                                        if offset == 0 {
                                            let scroll = this.list.logical_scroll_top();
                                            this.set_tracks(tracks);
                                            this.list.scroll_to(scroll);
                                        } else {
                                            // Keep a queue started from this playlist going past what had loaded.
                                            let context = ContextUri::Playlist(id.clone());
                                            let (start, more) = (this.tracks.len(), tracks.clone());
                                            app::player(cx).update(cx, |player, cx| {
                                                player.extend_context(&context, start, more, cx)
                                            });
                                            this.append_tracks(tracks);
                                        }
                                        more
                                    }
                                    Err(err) => {
                                        if this.tracks.is_empty() {
                                            this.error = Some(format!("{err}").into());
                                        }
                                        false
                                    }
                                };
                                cx.notify();
                                more
                            })
                            .unwrap_or(false);
                        if !more {
                            break;
                        }
                        offset += 100;
                    }
                })
                .detach();
            }
        }
    }

    fn context(&self) -> ContextUri {
        match &self.source {
            Source::Playlist(id) => ContextUri::Playlist(id.clone()),
            Source::Album(id) => ContextUri::Album(id.clone()),
            Source::Liked => ContextUri::Liked,
        }
    }

    fn title(&self) -> SharedString {
        match &self.source {
            Source::Liked => "Liked Songs".into(),
            Source::Playlist(_) => self
                .playlist
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            Source::Album(_) => self
                .album
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_default(),
        }
    }

    fn play(&mut self, index: usize, cx: &mut Context<Self>) {
        let (context, title, tracks) = (self.context(), self.title(), self.tracks.clone());
        app::player(cx).update(cx, |player, cx| {
            player.play_context(Some(context), title, tracks, index, cx)
        });
    }

    fn is_own_playlist(&self, cx: &App) -> bool {
        let user = app::session(cx)
            .read(cx)
            .user
            .as_ref()
            .map(|u| u.id.clone());
        match (&self.playlist, user) {
            (Some(p), Some(user)) => p.owner.id == user || p.collaborative,
            _ => false,
        }
    }

    fn hero(&self, window: &Window, cx: &mut App) -> AnyElement {
        let total_ms: u64 = self.tracks.iter().map(|t| t.duration_ms as u64).sum();
        let count = self.tracks.len();
        let songs = if count == 1 {
            "1 song".to_string()
        } else {
            format!("{} songs", format_count(count as u64))
        };

        let (kind, art, seed, byline): (&str, AnyElement, u64, AnyElement) = match &self.source {
            Source::Liked => {
                let name = app::session(cx)
                    .read(cx)
                    .user
                    .as_ref()
                    .map(|u| u.name())
                    .unwrap_or_default();
                (
                    "Playlist",
                    div()
                        .flex_none()
                        .size(px(208.0))
                        .rounded(px(theme::RADIUS_CARD))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(linear_gradient(
                            135.0,
                            linear_color_stop(theme::accent_strong(), 0.0),
                            linear_color_stop(theme::pink(), 1.0),
                        ))
                        .child(
                            svg()
                                .path(icons::HEART_FILL)
                                .size(px(84.0))
                                .text_color(theme::white(0.95)),
                        )
                        .into_any_element(),
                    420,
                    div().child(format!("{name} · {songs}")).into_any_element(),
                )
            }
            Source::Playlist(id) => {
                let playlist = self.playlist.as_ref();
                let image = playlist.and_then(|p| p.images.for_size(208));
                let owner = playlist.map(|p| p.owner.name()).unwrap_or_default();
                let description = playlist
                    .and_then(|p| p.description.clone())
                    .filter(|d| !d.is_empty());
                (
                    "Playlist",
                    ui::cover(
                        image,
                        208.0,
                        theme::RADIUS_CARD,
                        Placeholder::Playlist,
                        window,
                        cx,
                    )
                    .into_any_element(),
                    theme::seed_of(id),
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(
                            description.map(|d| {
                                div().line_clamp(2).text_color(theme::text_muted()).child(d)
                            }),
                        )
                        .child(
                            div().child(format!("{owner} · {songs}, {}", format_total(total_ms))),
                        )
                        .into_any_element(),
                )
            }
            Source::Album(id) => {
                let album = self.album.as_ref();
                let image = album.and_then(|a| a.images.for_size(208));
                let artist = album.and_then(|a| a.artists.first().cloned());
                let year = album.map(|a| a.year().to_string()).unwrap_or_default();
                (
                    album.map(|a| a.type_label()).unwrap_or("Album"),
                    ui::cover(
                        image,
                        208.0,
                        theme::RADIUS_CARD,
                        Placeholder::Music,
                        window,
                        cx,
                    )
                    .into_any_element(),
                    theme::seed_of(id),
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(artist.map(|artist| {
                            let id = artist.id.clone();
                            div()
                                .id("artist")
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::accent_soft()))
                                .child(artist.name)
                                .on_click(move |_, _, cx| {
                                    app::navigate(Route::Artist(id.clone()), cx)
                                })
                        }))
                        .child(
                            div()
                                .text_color(theme::text_muted())
                                .child(format!("· {year} · {songs}, {}", format_total(total_ms))),
                        )
                        .into_any_element(),
                )
            }
        };

        div()
            .relative()
            .h(px(288.0))
            .child(div().absolute().size_full().bg(theme::hero_gradient(seed)))
            .child(
                div()
                    .relative()
                    .size_full()
                    .px_6()
                    .pb_6()
                    .flex()
                    .items_end()
                    .gap_6()
                    .child(
                        div()
                            .flex_none()
                            .shadow(vec![BoxShadow {
                                color: theme::black(0.45),
                                offset: point(px(0.0), px(16.0)),
                                blur_radius: px(40.0),
                                spread_radius: px(-8.0),
                                inset: false,
                            }])
                            .rounded(px(theme::RADIUS_CARD))
                            .child(art),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(kind))
                            .child(
                                div()
                                    .line_clamp(2)
                                    .text_size(px(44.0))
                                    .line_height(px(52.0))
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .child(self.title()),
                            )
                            .child(byline),
                    ),
            )
            .into_any_element()
    }

    fn action_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let context = self.context();
        let (is_context, playing, shuffle) = {
            let player = app::player(cx);
            let p = player.read(cx);
            (p.is_context(&context), p.playing, p.shuffle())
        };
        let active = is_context && playing;

        let save = match &self.source {
            Source::Album(_) => self.album.clone().map(SavedEntity::Album),
            Source::Playlist(_) if !self.is_own_playlist(cx) => {
                self.playlist.clone().map(SavedEntity::Playlist)
            }
            _ => None,
        };
        let saved = match &self.source {
            Source::Album(id) | Source::Playlist(id) => app::library(cx).read(cx).is_saved(id),
            Source::Liked => true,
        };

        div()
            .h(px(80.0))
            .px_6()
            .flex()
            .items_center()
            .gap_4()
            .child(
                ui::play_button("play", 52.0, active)
                    .when(self.tracks.is_empty(), |b| b.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_context {
                            app::player(cx).update(cx, |p, cx| p.toggle(cx));
                        } else {
                            this.play(0, cx);
                        }
                    })),
            )
            .child(
                ui::toggle_icon_button("shuffle", icons::SHUFFLE, shuffle)
                    .size(px(40.0))
                    .tip("Shuffle")
                    .on_click(|_, _, cx| app::player(cx).update(cx, |p, cx| p.toggle_shuffle(cx))),
            )
            .children(save.map(|item| {
                ui::toggle_icon_button(
                    "save",
                    if saved {
                        icons::CIRCLE_CHECK
                    } else {
                        icons::CIRCLE_PLUS
                    },
                    saved,
                )
                .size(px(40.0))
                .tip(if saved {
                    "Remove from Your Library"
                } else {
                    "Save to Your Library"
                })
                .on_click(move |_, _, cx| {
                    let item = item.clone();
                    app::library(cx).update(cx, |library, cx| library.toggle_saved(item, cx))
                })
            }))
            .child(
                ui::icon_button("more", icons::ELLIPSIS)
                    .size(px(40.0))
                    .on_click(
                        cx.listener(|this, event: &ClickEvent, _, cx| match &this.source {
                            Source::Playlist(_) => {
                                if let Some(playlist) = &this.playlist {
                                    menus::playlist_menu(playlist, event.position(), cx);
                                }
                            }
                            Source::Album(_) => {
                                if let Some(album) = &this.album {
                                    menus::album_menu(album, event.position(), cx);
                                }
                            }
                            Source::Liked => {}
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_album = matches!(self.source, Source::Album(_));
        match ix {
            0 => self.hero(window, cx),
            1 => self.action_bar(cx),
            2 => {
                if let Some(error) = &self.error {
                    ui::empty_state(
                        icons::CIRCLE_ALERT,
                        "Couldn't load this page",
                        error.clone(),
                    )
                    .into_any_element()
                } else if self.loading {
                    ui::loading_state().into_any_element()
                } else if self.tracks.is_empty() {
                    ui::empty_state(
                        icons::MUSIC,
                        "Nothing here yet",
                        "Songs you add will show up here.",
                    )
                    .into_any_element()
                } else {
                    track_header(!is_album, window, cx)
                        .pb_1()
                        .into_any_element()
                }
            }
            _ => {
                let index = ix - HEADER_ROWS;
                let Some(track) = self.tracks.get(index) else {
                    return div().into_any_element();
                };
                let editable = match &self.source {
                    Source::Playlist(id) if self.is_own_playlist(cx) => Some(id.clone()),
                    _ => None,
                };
                let options = RowOptions {
                    number: Some(if is_album && track.track_number > 0 {
                        track.track_number
                    } else {
                        index as u32 + 1
                    }),
                    show_cover: !is_album,
                    show_album: !is_album,
                    menu: TrackMenuContext {
                        editable_playlist: editable,
                        queue_index: None,
                    },
                };
                let this = cx.entity().downgrade();
                track_row(
                    ("track", index),
                    track,
                    options,
                    move |_, cx| {
                        this.update(cx, |this, cx| this.play(index, cx)).ok();
                    },
                    window,
                    cx,
                )
                .into_any_element()
            }
        }
    }
}

impl Render for CollectionPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix: usize, window, cx| {
                        // List items are sized to their content unless told otherwise.
                        div()
                            .w_full()
                            .child(this.render_item(ix, window, cx))
                            .into_any_element()
                    }),
                )
                .size_full(),
            )
            .group("scrollable")
            .child(ui::vertical_scrollbar(&self.list))
    }
}
