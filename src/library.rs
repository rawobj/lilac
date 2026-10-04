//! The user's collection, kept in memory and updated optimistically.

use std::collections::HashSet;

use gpui_kit::*;

use crate::app;
use crate::model::*;
use crate::runtime;
use crate::session::{Session, SessionEvent};

#[derive(Default)]
pub struct Library {
    pub playlists: Vec<Playlist>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub shows: Vec<Show>,
    /// Liked songs, newest first. Filled page by page in the background.
    pub liked: Vec<Track>,
    pub liked_total: u32,
    pub liked_loading: bool,
    liked_ids: HashSet<SharedString>,
    saved_ids: HashSet<SharedString>,
    pub loaded: bool,
    generation: u64,
}

impl Library {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.subscribe(session, |this, _, event, cx| match event {
            SessionEvent::Ready => this.reload(cx),
            SessionEvent::LoggedOut => {
                let generation = this.generation + 1;
                *this = Library {
                    generation,
                    ..Default::default()
                };
                cx.notify();
            }
        })
        .detach();
        Self::default()
    }

    pub fn is_liked(&self, track_id: &str) -> bool {
        self.liked_ids.contains(track_id)
    }

    /// Whether an album, artist, show or playlist is in the library.
    pub fn is_saved(&self, id: &str) -> bool {
        self.saved_ids.contains(id)
    }

    pub fn playlist(&self, id: &str) -> Option<&Playlist> {
        self.playlists.iter().find(|p| p.id.as_ref() == id)
    }

    /// Playlists the user can add tracks to.
    pub fn editable_playlists(&self, user_id: &str) -> impl Iterator<Item = &Playlist> {
        self.playlists
            .iter()
            .filter(move |p| p.owner.id.as_ref() == user_id || p.collaborative)
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let api = app::api(cx);

        macro_rules! load {
            ($call:ident, $apply:expr) => {{
                let api = api.clone();
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move { api.$call().await }).await;
                    this.update(cx, |this, cx| {
                        if this.generation != generation {
                            return;
                        }
                        match result {
                            Ok(items) => {
                                #[allow(clippy::redundant_closure_call)]
                                ($apply)(this, items);
                                this.loaded = true;
                                cx.notify();
                            }
                            Err(err) => {
                                app::toast_error(format!("Could not load library: {err}"), cx)
                            }
                        }
                    })
                    .ok();
                })
                .detach();
            }};
        }

        load!(my_playlists, |this: &mut Library, items: Vec<Playlist>| {
            this.saved_ids.extend(items.iter().map(|p| p.id.clone()));
            this.playlists = items;
        });
        load!(saved_albums, |this: &mut Library, items: Vec<Album>| {
            this.saved_ids.extend(items.iter().map(|a| a.id.clone()));
            this.albums = items;
        });
        load!(
            followed_artists,
            |this: &mut Library, items: Vec<Artist>| {
                this.saved_ids.extend(items.iter().map(|a| a.id.clone()));
                this.artists = items;
            }
        );
        load!(saved_shows, |this: &mut Library, items: Vec<Show>| {
            this.saved_ids.extend(items.iter().map(|s| s.id.clone()));
            this.shows = items;
        });

        self.load_liked(generation, cx);
    }

    /// Page through the liked songs. The first page lands quickly; the rest trickle in.
    fn load_liked(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.liked_loading = true;
        self.liked.clear();
        self.liked_ids.clear();
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let mut offset = 0;
            loop {
                let api = api.clone();
                let page = runtime::run(async move { api.saved_tracks(offset).await }).await;
                let more = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        let Ok(page) = page else {
                            this.liked_loading = false;
                            cx.notify();
                            return false;
                        };
                        this.liked_total = page.total;
                        let more = page.next.is_some() && !page.items.is_empty();
                        for track in page.items.into_iter().filter_map(|i| i.item) {
                            if this.liked_ids.insert(track.id.clone()) {
                                this.liked.push(track);
                            }
                        }
                        this.liked_loading = more;
                        cx.notify();
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
                offset += 50;
            }
        })
        .detach();
    }

    pub fn toggle_like(&mut self, track: &Track, cx: &mut Context<Self>) {
        if track.id.is_empty() {
            return;
        }
        let liked = !self.is_liked(&track.id);
        self.apply_like(track, liked);
        cx.notify();

        let api = app::api(cx);
        let uri = track.uri.to_string();
        let track = track.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.set_saved(&[uri], liked).await }).await;
            cx.update(|cx| match result {
                Ok(()) => app::toast(
                    if liked {
                        "Added to Liked Songs"
                    } else {
                        "Removed from Liked Songs"
                    },
                    cx,
                ),
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.apply_like(&track, !liked);
                        cx.notify();
                    })
                    .ok();
                    app::toast_error(format!("Could not update Liked Songs: {err}"), cx);
                }
            });
        })
        .detach();
    }

    fn apply_like(&mut self, track: &Track, liked: bool) {
        if liked {
            if self.liked_ids.insert(track.id.clone()) {
                self.liked.insert(0, track.clone());
                self.liked_total += 1;
            }
        } else if self.liked_ids.remove(&track.id) {
            self.liked.retain(|t| t.id != track.id);
            self.liked_total = self.liked_total.saturating_sub(1);
        }
    }

    /// Save or remove an album, artist, show or playlist.
    pub fn toggle_saved(&mut self, item: SavedEntity, cx: &mut Context<Self>) {
        let id = item.id().clone();
        let saved = !self.is_saved(&id);
        let label = item.label();
        self.apply_saved(&item, saved);
        cx.notify();

        let api = app::api(cx);
        let uri = item.uri();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.set_saved(&[uri], saved).await }).await;
            cx.update(|cx| match result {
                Ok(()) => app::toast(
                    if saved {
                        format!("Added {label} to Your Library")
                    } else {
                        format!("Removed {label} from Your Library")
                    },
                    cx,
                ),
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.apply_saved(&item, !saved);
                        cx.notify();
                    })
                    .ok();
                    app::toast_error(format!("Could not update Your Library: {err}"), cx);
                }
            });
        })
        .detach();
    }

    fn apply_saved(&mut self, item: &SavedEntity, saved: bool) {
        let id = item.id().clone();
        if saved {
            if !self.saved_ids.insert(id) {
                return;
            }
            match item {
                SavedEntity::Album(a) => self.albums.insert(0, a.clone()),
                SavedEntity::Artist(a) => self.artists.insert(0, a.clone()),
                SavedEntity::Show(s) => self.shows.insert(0, s.clone()),
                SavedEntity::Playlist(p) => self.playlists.insert(0, p.clone()),
            }
        } else {
            self.saved_ids.remove(&id);
            match item {
                SavedEntity::Album(_) => self.albums.retain(|a| a.id != id),
                SavedEntity::Artist(_) => self.artists.retain(|a| a.id != id),
                SavedEntity::Show(_) => self.shows.retain(|s| s.id != id),
                SavedEntity::Playlist(_) => self.playlists.retain(|p| p.id != id),
            }
        }
    }

    /// Create a playlist, optionally seeded with a track, and open it.
    pub fn create_playlist(
        &mut self,
        name: String,
        description: String,
        seed: Option<Track>,
        cx: &mut Context<Self>,
    ) {
        let api = app::api(cx);
        let user_id = app::session(cx)
            .read(cx)
            .user
            .as_ref()
            .map(|u| u.id.to_string())
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move {
                let playlist = api.create_playlist(&user_id, &name, &description).await?;
                if let Some(track) = seed {
                    api.add_to_playlist(&playlist.id, &[track.uri.to_string()])
                        .await?;
                }
                Ok(playlist)
            })
            .await;
            cx.update(|cx| match result {
                Ok(playlist) => {
                    let id = playlist.id.clone();
                    this.update(cx, |this, cx| {
                        this.saved_ids.insert(playlist.id.clone());
                        this.playlists.insert(0, playlist);
                        cx.notify();
                    })
                    .ok();
                    app::toast("Playlist created", cx);
                    app::navigate(crate::nav::Route::Playlist(id), cx);
                }
                Err(err) => app::toast_error(format!("Could not create playlist: {err}"), cx),
            });
        })
        .detach();
    }

    pub fn rename_playlist(
        &mut self,
        id: SharedString,
        name: String,
        description: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
            playlist.name = name.clone().into();
            if !description.is_empty() {
                playlist.description = Some(description.clone().into());
            }
            cx.notify();
        }
        let api = app::api(cx);
        cx.spawn(async move |_, cx| {
            let result =
                runtime::run(async move { api.update_playlist(&id, &name, &description).await })
                    .await;
            cx.update(|cx| match result {
                Ok(()) => app::toast("Playlist details saved", cx),
                Err(err) => app::toast_error(format!("Could not save playlist: {err}"), cx),
            });
        })
        .detach();
    }

    pub fn add_to_playlist(
        &mut self,
        playlist_id: SharedString,
        tracks: Vec<Track>,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .playlist(&playlist_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let count = tracks.len() as u32;
        let uris: Vec<String> = tracks.iter().map(|t| t.uri.to_string()).collect();
        let api = app::api(cx);
        let id = playlist_id.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { api.add_to_playlist(&id, &uris).await }).await;
            cx.update(|cx| match result {
                Ok(()) => {
                    this.update(cx, |this, cx| {
                        if let Some(p) = this.playlists.iter_mut().find(|p| p.id == playlist_id) {
                            let total = p.total() + count;
                            p.items = Some(PageRef { total });
                        }
                        cx.emit(LibraryEvent::PlaylistChanged(playlist_id.clone()));
                        cx.notify();
                    })
                    .ok();
                    app::toast(format!("Added to {name}"), cx);
                }
                Err(err) => app::toast_error(format!("Could not add to playlist: {err}"), cx),
            });
        })
        .detach();
    }

    pub fn remove_from_playlist(
        &mut self,
        playlist_id: SharedString,
        track: Track,
        cx: &mut Context<Self>,
    ) {
        let api = app::api(cx);
        let id = playlist_id.clone();
        let uri = track.uri.to_string();
        cx.spawn(async move |this, cx| {
            let result =
                runtime::run(async move { api.remove_from_playlist(&id, &[uri]).await }).await;
            cx.update(|cx| match result {
                Ok(()) => {
                    this.update(cx, |this, cx| {
                        if let Some(p) = this.playlists.iter_mut().find(|p| p.id == playlist_id) {
                            let total = p.total().saturating_sub(1);
                            p.items = Some(PageRef { total });
                        }
                        cx.emit(LibraryEvent::PlaylistChanged(playlist_id.clone()));
                        cx.notify();
                    })
                    .ok();
                    app::toast("Removed from playlist", cx);
                }
                Err(err) => app::toast_error(format!("Could not remove track: {err}"), cx),
            });
        })
        .detach();
    }
}

pub enum LibraryEvent {
    /// The contents of a playlist changed and open pages should refetch it.
    PlaylistChanged(SharedString),
}

impl EventEmitter<LibraryEvent> for Library {}

/// Something that can be saved to (or removed from) the library.
#[derive(Clone)]
pub enum SavedEntity {
    Album(Album),
    Artist(Artist),
    Show(Show),
    Playlist(Playlist),
}

impl SavedEntity {
    fn id(&self) -> &SharedString {
        match self {
            SavedEntity::Album(a) => &a.id,
            SavedEntity::Artist(a) => &a.id,
            SavedEntity::Show(s) => &s.id,
            SavedEntity::Playlist(p) => &p.id,
        }
    }

    fn uri(&self) -> String {
        let (kind, id) = match self {
            SavedEntity::Album(a) => ("album", &a.id),
            SavedEntity::Artist(a) => ("artist", &a.id),
            SavedEntity::Show(s) => ("show", &s.id),
            SavedEntity::Playlist(p) => ("playlist", &p.id),
        };
        format!("spotify:{kind}:{id}")
    }

    fn label(&self) -> &'static str {
        match self {
            SavedEntity::Album(_) => "album",
            SavedEntity::Artist(_) => "artist",
            SavedEntity::Show(_) => "podcast",
            SavedEntity::Playlist(_) => "playlist",
        }
    }
}
