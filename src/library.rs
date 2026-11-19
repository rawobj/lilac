//! The user's collection, kept in memory and updated optimistically.

use std::collections::HashSet;

use gpui_kit::*;

use crate::app;
use crate::library_cache;
use crate::model::*;
use crate::runtime;
use crate::session::{Session, SessionEvent};
use crate::spotify::streaming;

#[derive(Default)]
pub struct Library {
    pub playlists: Vec<Playlist>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub shows: Vec<Show>,
    /// Liked songs, newest first. Filled page by page in the background.
    pub liked: Vec<Track>,
    /// Every liked song, newest first, by id: known from the library list
    /// alone. `liked` holds the ones whose details are loaded, which happens
    /// when Liked Songs is opened ([`Library::ensure_liked_details`]).
    liked_order: Vec<SharedString>,
    /// Liked songs whose details were asked for.
    liked_asked: HashSet<SharedString>,
    liked_details_loading: bool,
    pub liked_total: u32,
    pub liked_loading: bool,
    liked_ids: HashSet<SharedString>,
    saved_ids: HashSet<SharedString>,
    pub loaded: bool,
    generation: u64,
    /// What is shown came from disk and may be behind Spotify.
    from_cache: bool,
    told_stale: bool,
    persist_pending: bool,
    liked_checked: Option<std::time::Instant>,
    /// The collection service's token per list, for asking what changed.
    sync_tokens: std::collections::HashMap<String, String>,
}

const LIKED_RECHECK: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Liked songs whose details are loaded at a time.
const LIKED_BATCH: usize = 50;

/// A restart within this keeps the kept albums, artists and podcasts (each
/// asked of the rate-limited Web API; saving one in Lilac updates them at once).
const LISTS_FRESH_SECS: u64 = 60 * 60;
const PERSIST_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

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
        if app::session(cx).read(cx).is_demo() {
            self.refresh(generation, None, cx);
            return;
        }
        let user_id = self.user_id(cx);
        cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { library_cache::load() })
                .await
                .filter(|s| s.user_id.is_empty() || user_id.is_empty() || s.user_id == user_id);
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let age = snapshot.as_ref().map(|s| s.age_secs());
                if let Some(snapshot) = snapshot {
                    this.apply_snapshot(snapshot);
                    cx.notify();
                }
                this.refresh(generation, age, cx);
            })
            .ok();
        })
        .detach();
    }

    fn user_id(&self, cx: &App) -> String {
        app::session(cx)
            .read(cx)
            .user
            .as_ref()
            .map(|u| u.id.to_string())
            .unwrap_or_default()
    }

    fn apply_snapshot(&mut self, snapshot: library_cache::Snapshot) {
        self.saved_ids
            .extend(snapshot.playlists.iter().map(|p| p.id.clone()));
        self.saved_ids
            .extend(snapshot.albums.iter().map(|a| a.id.clone()));
        self.saved_ids
            .extend(snapshot.artists.iter().map(|a| a.id.clone()));
        self.saved_ids
            .extend(snapshot.shows.iter().map(|s| s.id.clone()));
        self.liked_ids = snapshot.liked.iter().map(|t| t.id.clone()).collect();
        self.playlists = snapshot.playlists;
        self.albums = snapshot.albums;
        self.artists = snapshot.artists;
        self.shows = snapshot.shows;
        self.liked_order = if snapshot.liked_order.is_empty() {
            snapshot.liked.iter().map(|t| t.id.clone()).collect()
        } else {
            snapshot
                .liked_order
                .into_iter()
                .map(SharedString::from)
                .collect()
        };
        self.liked_ids.extend(self.liked_order.iter().cloned());
        self.liked = snapshot.liked;
        self.liked_total = snapshot.liked_total.max(self.liked_order.len() as u32);
        self.sync_tokens = snapshot.sync_tokens;
        self.from_cache = true;
        self.loaded = true;
    }

    /// Ask Spotify for what may have changed since `cached_age` seconds ago
    /// (`None`: nothing was kept, load everything).
    fn refresh(&mut self, generation: u64, cached_age: Option<u64>, cx: &mut Context<Self>) {
        let api = app::api(cx);
        let demo = app::session(cx).read(cx).is_demo();
        // Saved albums, artists and podcasts change rarely and cost Web API
        // requests; a restart soon after the last load keeps them.
        let recent = cached_age.is_some_and(|age| age < LISTS_FRESH_SECS);

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
                                this.persist(cx);
                                cx.notify();
                            }
                            Err(err) => this.load_failed(err, cx),
                        }
                    })
                    .ok();
                })
                .detach();
            }};
        }

        if demo {
            load!(my_playlists, |this: &mut Library, items: Vec<Playlist>| {
                this.set_playlists(items, None)
            });
        } else {
            self.load_playlists(generation, cx);
            // Liked songs and saved albums, followed artists, saved
            // podcasts: Spotify's collection service, as its own clients
            // sync them (only what changed, once a list is known). The Web
            // API only for a list the service cannot give.
            for set in [Set::Collection, Set::Artists, Set::Shows] {
                self.sync_set(set, generation, cx);
            }
            return;
        }
        if !recent {
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
        }

        if self.liked.is_empty() {
            self.load_liked(generation, cx);
        } else {
            self.check_liked(generation, cx);
        }
    }

    /// Bring one library list up to date through the collection service:
    /// the changes since the kept token when there is one, else the whole
    /// list. Falls back to the Web API for that list.
    fn sync_set(&mut self, set: Set, generation: u64, cx: &mut Context<Self>) {
        let token = self
            .sync_tokens
            .get(set.name())
            .filter(|_| self.has(set))
            .cloned();
        if set == Set::Collection && self.liked_order.is_empty() {
            self.liked_loading = true;
            cx.notify();
        }
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { Ok(fetch_set(set, token).await) }).await;
            let result = result.and_then(|r| r);
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                if set == Set::Collection {
                    this.liked_loading = false;
                }
                match result {
                    Ok(update) => {
                        this.apply_set(set, update, cx);
                        this.loaded = true;
                        this.persist(cx);
                        cx.notify();
                    }
                    Err(err) => {
                        eprintln!(
                            "lilac: {} from the collection service failed: {err:#}",
                            set.name()
                        );
                        this.web_fallback(set, generation, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn has(&self, set: Set) -> bool {
        match set {
            Set::Collection => !self.liked_order.is_empty() || !self.albums.is_empty(),
            Set::Artists => !self.artists.is_empty(),
            Set::Shows => !self.shows.is_empty(),
        }
    }

    fn apply_set(&mut self, set: Set, update: SetUpdate, cx: &mut Context<Self>) {
        let SetUpdate {
            whole,
            removed,
            track_ids,
            albums,
            artists,
            shows,
            sync_token,
        } = update;
        self.sync_tokens.insert(set.name().to_string(), sync_token);
        // Newer entries go on top; one already listed moves there.
        fn merge<T: Clone>(
            list: &mut Vec<T>,
            whole: bool,
            fresh: Vec<T>,
            removed: &HashSet<SharedString>,
            id: impl Fn(&T) -> &SharedString,
        ) {
            if whole {
                *list = fresh;
                return;
            }
            let fresh_ids: HashSet<SharedString> = fresh.iter().map(|t| id(t).clone()).collect();
            list.retain(|t| !removed.contains(id(t)) && !fresh_ids.contains(id(t)));
            let mut merged = fresh;
            merged.append(list);
            *list = merged;
        }
        for gone in &removed {
            self.saved_ids.remove(gone);
            self.liked_ids.remove(gone);
        }
        match set {
            Set::Collection => {
                merge(&mut self.liked_order, whole, track_ids, &removed, |id| id);
                merge(&mut self.albums, whole, albums, &removed, |a| &a.id);
                self.rebuild_liked(Vec::new());
                // The first batch at once; the rest as Liked Songs scrolls.
                if self.liked.len() < LIKED_BATCH {
                    self.load_more_liked(cx);
                }
                self.saved_ids
                    .extend(self.albums.iter().map(|a| a.id.clone()));
            }
            Set::Artists => {
                merge(&mut self.artists, whole, artists, &removed, |a| &a.id);
                self.saved_ids
                    .extend(self.artists.iter().map(|a| a.id.clone()));
            }
            Set::Shows => {
                merge(&mut self.shows, whole, shows, &removed, |s| &s.id);
                self.saved_ids
                    .extend(self.shows.iter().map(|s| s.id.clone()));
            }
        }
    }

    fn web_fallback(&mut self, set: Set, generation: u64, cx: &mut Context<Self>) {
        let api = app::api(cx);
        match set {
            Set::Collection => {
                if self.liked.is_empty() {
                    self.load_liked(generation, cx);
                } else {
                    self.check_liked(generation, cx);
                }
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move { api.saved_albums().await }).await;
                    this.update(cx, |this, cx| match result {
                        Ok(items) if this.generation == generation => {
                            this.saved_ids.extend(items.iter().map(|a| a.id.clone()));
                            this.albums = items;
                            this.persist(cx);
                            cx.notify();
                        }
                        Ok(_) => {}
                        Err(err) => this.load_failed(err, cx),
                    })
                    .ok();
                })
                .detach();
            }
            Set::Artists => {
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move { api.followed_artists().await }).await;
                    this.update(cx, |this, cx| match result {
                        Ok(items) if this.generation == generation => {
                            this.saved_ids.extend(items.iter().map(|a| a.id.clone()));
                            this.artists = items;
                            this.persist(cx);
                            cx.notify();
                        }
                        Ok(_) => {}
                        Err(err) => this.load_failed(err, cx),
                    })
                    .ok();
                })
                .detach();
            }
            Set::Shows => {
                cx.spawn(async move |this, cx| {
                    let result = runtime::run(async move { api.saved_shows().await }).await;
                    this.update(cx, |this, cx| match result {
                        Ok(items) if this.generation == generation => {
                            this.saved_ids.extend(items.iter().map(|s| s.id.clone()));
                            this.shows = items;
                            this.persist(cx);
                            cx.notify();
                        }
                        Ok(_) => {}
                        Err(err) => this.load_failed(err, cx),
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn load_failed(&mut self, err: anyhow::Error, cx: &mut Context<Self>) {
        // Whatever was kept stays up; say so once rather than per list.
        if self.from_cache {
            if !self.told_stale {
                self.told_stale = true;
                app::toast(format!("Showing your library as last loaded: {err}"), cx);
            }
        } else {
            app::toast_error(format!("Could not load library: {err}"), cx);
        }
    }

    /// The playlist list from the streaming session (librespot's rootlist),
    /// which the Web API's rate limit does not touch; the Web API only when
    /// that fails.
    fn load_playlists(&mut self, generation: u64, cx: &mut Context<Self>) {
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move {
                match streaming::rootlist().await {
                    Ok(list) => Ok(list),
                    Err(err) => {
                        eprintln!("lilac: playlists from the session failed: {err:#}");
                        api.my_playlists().await
                    }
                }
            })
            .await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(items) => {
                        let me = app::session(cx).read(cx).user.clone();
                        this.set_playlists(items, me.as_ref());
                        this.loaded = true;
                        this.persist(cx);
                        cx.notify();
                        if this.playlists.iter().any(lacks_details) {
                            this.fill_playlist_details(generation, cx);
                        }
                    }
                    Err(err) => this.load_failed(err, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// The rootlist names owners only by user name and has no cover for
    /// playlists without an uploaded picture. Both come from the streaming
    /// session (names from profiles, covers tiled from the playlist's first
    /// albums) and are kept, so this runs again only for playlists new to
    /// the library.
    fn fill_playlist_details(&mut self, generation: u64, cx: &mut Context<Self>) {
        let mut owners: Vec<String> = self
            .playlists
            .iter()
            .filter(|p| p.owner.display_name.is_none() && !p.owner.id.is_empty())
            .map(|p| p.owner.id.to_string())
            .collect();
        owners.sort();
        owners.dedup();
        if !owners.is_empty() {
            cx.spawn(async move |this, cx| {
                let Ok(names) = runtime::run(streaming::user_names(owners)).await else {
                    return;
                };
                this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    for playlist in &mut this.playlists {
                        if playlist.owner.display_name.is_some() {
                            continue;
                        }
                        if let Some((_, name)) =
                            names.iter().find(|(user, _)| *user == playlist.owner.id)
                        {
                            playlist.owner.display_name = Some(name.clone().into());
                        }
                    }
                    this.persist(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }

        let coverless: Vec<String> = self
            .playlists
            .iter()
            .filter(|p| p.images.is_empty() && p.total() > 0)
            .map(|p| p.id.to_string())
            .collect();
        if coverless.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(covers) = runtime::run(streaming::playlist_covers(coverless)).await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                for (id, images) in covers {
                    if let Some(playlist) = this.playlists.iter_mut().find(|p| p.id.as_ref() == id)
                        && playlist.images.is_empty()
                    {
                        playlist.images = images;
                    }
                }
                this.persist(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Take a fresh playlist list, keeping what it lacks (pictures, owner
    /// names, versions) from the copy already shown.
    fn set_playlists(&mut self, mut items: Vec<Playlist>, me: Option<&User>) {
        for playlist in &mut items {
            if let Some(me) = me
                && playlist.owner.id == me.id
                && playlist.owner.display_name.is_none()
            {
                playlist.owner.display_name = me.display_name.clone();
            }
            let Some(known) = self.playlists.iter().find(|p| p.id == playlist.id) else {
                continue;
            };
            if playlist.images.is_empty() {
                playlist.images = known.images.clone();
            }
            if playlist.owner.display_name.is_none() && known.owner.id == playlist.owner.id {
                playlist.owner.display_name = known.owner.display_name.clone();
            }
            if playlist.snapshot_id.is_none() {
                playlist.snapshot_id = known.snapshot_id.clone();
            }
            if playlist.public.is_none() {
                playlist.public = known.public;
            }
        }
        self.saved_ids.extend(items.iter().map(|p| p.id.clone()));
        self.playlists = items;
    }

    /// Write the library to disk shortly after it settles; a burst of
    /// updates at startup becomes one write.
    fn persist(&mut self, cx: &mut Context<Self>) {
        if self.persist_pending || app::session(cx).read(cx).is_demo() {
            return;
        }
        self.persist_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PERSIST_DELAY).await;
            let snapshot = this
                .update(cx, |this, cx| {
                    this.persist_pending = false;
                    // A partial liked list would be read back as complete.
                    if this.liked_loading {
                        return None;
                    }
                    Some(library_cache::Snapshot {
                        user_id: this.user_id(cx),
                        saved_at: library_cache::now(),
                        playlists: this.playlists.clone(),
                        albums: this.albums.clone(),
                        artists: this.artists.clone(),
                        shows: this.shows.clone(),
                        liked: this.liked.clone(),
                        liked_total: this.liked_total,
                        liked_order: this.liked_order.iter().map(|id| id.to_string()).collect(),
                        sync_tokens: this.sync_tokens.clone(),
                    })
                })
                .ok()
                .flatten();
            if let Some(snapshot) = snapshot {
                cx.background_executor()
                    .spawn(async move { library_cache::save(&snapshot) })
                    .await;
            }
        })
        .detach();
    }

    /// `liked` again from `liked_order`, with the details known (and
    /// `fetched`); songs without details yet are left out until they come.
    fn rebuild_liked(&mut self, fetched: Vec<Track>) {
        let mut known: std::collections::HashMap<SharedString, Track> = self
            .liked
            .drain(..)
            .chain(fetched)
            .map(|t| (t.id.clone(), t))
            .collect();
        self.liked = self
            .liked_order
            .iter()
            .filter_map(|id| known.remove(id))
            .collect();
        self.liked_ids = self.liked_order.iter().cloned().collect();
        self.liked_total = self.liked_order.len() as u32;
    }

    /// Details for the next liked songs not shown yet: the first batch as
    /// soon as the list is known, the rest as Liked Songs is scrolled
    /// towards their end ([`Library::liked_has_more`]). From disk where kept,
    /// else one batched lookup through the streaming session.
    pub fn load_more_liked(&mut self, cx: &mut Context<Self>) {
        if self.liked_details_loading || app::session(cx).read(cx).is_demo() {
            return;
        }
        let have: HashSet<&SharedString> = self.liked.iter().map(|t| &t.id).collect();
        let next: Vec<SharedString> = self
            .liked_order
            .iter()
            .filter(|id| !have.contains(id) && !self.liked_asked.contains(*id))
            .take(LIKED_BATCH)
            .cloned()
            .collect();
        if next.is_empty() {
            return;
        }
        // Asked once: a song Spotify gives no details for is not asked again.
        self.liked_asked.extend(next.iter().cloned());
        let uris: Vec<String> = next
            .iter()
            .map(|id| format!("spotify:track:{id}"))
            .collect();
        self.liked_details_loading = true;
        if self.liked.is_empty() {
            self.liked_loading = true;
        }
        cx.notify();
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let result = runtime::run(async move { streaming::tracks_by_uri(&uris).await }).await;
            this.update(cx, |this, cx| {
                this.liked_details_loading = false;
                this.liked_loading = false;
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(tracks) => {
                        this.rebuild_liked(tracks);
                        this.persist(cx);
                    }
                    Err(err) => {
                        for id in &next {
                            this.liked_asked.remove(id);
                        }
                        this.load_failed(err, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Liked songs remain whose details have not been loaded.
    pub fn liked_has_more(&self) -> bool {
        self.liked.len() < self.liked_order.len()
            && self
                .liked_order
                .iter()
                .any(|id| !self.liked_ids_loaded(id) && !self.liked_asked.contains(id))
    }

    fn liked_ids_loaded(&self, id: &SharedString) -> bool {
        self.liked.iter().any(|t| &t.id == id)
    }

    /// The liked songs to play, in order: placeholders (enough to play)
    /// for any whose details have not been loaded.
    pub fn liked_for_play(&self) -> Vec<Track> {
        let known: std::collections::HashMap<&SharedString, &Track> =
            self.liked.iter().map(|t| (&t.id, t)).collect();
        self.liked_order
            .iter()
            .map(|id| match known.get(id) {
                Some(track) => (*track).clone(),
                None => Track {
                    id: id.clone(),
                    uri: format!("spotify:track:{id}").into(),
                    ..Default::default()
                },
            })
            .collect()
    }

    /// Opening Liked Songs asks whether anything changed, unless that was
    /// asked lately.
    pub fn revisit_liked(&mut self, cx: &mut Context<Self>) {
        let recent = self
            .liked_checked
            .is_some_and(|at| at.elapsed() < LIKED_RECHECK);
        if recent || self.liked_loading || self.liked_order.is_empty() {
            return;
        }
        self.liked_checked = Some(std::time::Instant::now());
        if app::session(cx).read(cx).is_demo() {
            self.check_liked(self.generation, cx);
        } else {
            self.sync_set(Set::Collection, self.generation, cx);
        }
    }

    /// One request for the newest liked songs; everything again only when
    /// they do not line up with what was kept.
    fn check_liked(&mut self, generation: u64, cx: &mut Context<Self>) {
        self.liked_checked = Some(std::time::Instant::now());
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let page = runtime::run(async move { api.saved_tracks(0).await }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let page = match page {
                    Ok(page) => page,
                    Err(err) => return this.load_failed(err, cx),
                };
                let fresh: Vec<Track> = page.items.into_iter().filter_map(|i| i.item).collect();
                match library_cache::merge_liked(&fresh, &this.liked, page.total) {
                    Some(merged) => {
                        if merged.len() != this.liked.len() {
                            this.liked_ids = merged.iter().map(|t| t.id.clone()).collect();
                            this.liked_order = merged.iter().map(|t| t.id.clone()).collect();
                            this.liked = merged;
                            this.liked_total = page.total;
                            this.persist(cx);
                            cx.notify();
                        }
                    }
                    None => this.load_liked(generation, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Page through the liked songs. The first page lands quickly; the rest
    /// trickle in. With a kept copy on screen, the new list replaces it
    /// only once complete.
    fn load_liked(&mut self, generation: u64, cx: &mut Context<Self>) {
        let replace = !self.liked.is_empty();
        self.liked_loading = true;
        if !replace {
            self.liked.clear();
            self.liked_order.clear();
            self.liked_ids.clear();
        }
        let api = app::api(cx);
        cx.spawn(async move |this, cx| {
            let mut offset = 0;
            let mut pending: Vec<Track> = Vec::new();
            loop {
                let api = api.clone();
                let page = runtime::run(async move { api.saved_tracks(offset).await }).await;
                let more = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        let page = match page {
                            Ok(page) => page,
                            Err(err) => {
                                this.liked_loading = false;
                                if replace {
                                    this.load_failed(err, cx);
                                }
                                cx.notify();
                                return false;
                            }
                        };
                        let more = page.next.is_some() && !page.items.is_empty();
                        let tracks = page.items.into_iter().filter_map(|i| i.item);
                        if replace {
                            pending.extend(tracks);
                            if !more {
                                let mut seen = HashSet::new();
                                pending.retain(|t| seen.insert(t.id.clone()));
                                this.liked_ids = seen;
                                this.liked = std::mem::take(&mut pending);
                                this.liked_order =
                                    this.liked.iter().map(|t| t.id.clone()).collect();
                                this.liked_total = page.total;
                            }
                        } else {
                            this.liked_total = page.total;
                            for track in tracks {
                                if this.liked_ids.insert(track.id.clone()) {
                                    this.liked_order.push(track.id.clone());
                                    this.liked.push(track);
                                }
                            }
                        }
                        this.liked_loading = more;
                        if !more {
                            this.persist(cx);
                        }
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
        self.persist(cx);
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
                self.liked_order.insert(0, track.id.clone());
                self.liked_total += 1;
            }
        } else if self.liked_ids.remove(&track.id) {
            self.liked.retain(|t| t.id != track.id);
            self.liked_order.retain(|id| *id != track.id);
            self.liked_total = self.liked_total.saturating_sub(1);
        }
    }

    /// Save or remove an album, artist, show or playlist.
    pub fn toggle_saved(&mut self, item: SavedEntity, cx: &mut Context<Self>) {
        let id = item.id().clone();
        let saved = !self.is_saved(&id);
        let label = item.label();
        self.apply_saved(&item, saved);
        self.persist(cx);
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
                        this.persist(cx);
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

/// The library lists the collection service keeps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Set {
    /// Liked songs and saved albums.
    Collection,
    Artists,
    Shows,
}

impl Set {
    fn name(self) -> &'static str {
        match self {
            Set::Collection => "collection",
            Set::Artists => "artist",
            Set::Shows => "show",
        }
    }
}

/// A list from the collection service, with details filled in.
struct SetUpdate {
    /// The whole list (else only what changed).
    whole: bool,
    removed: HashSet<SharedString>,
    /// Liked songs added, by id; their details load when needed.
    track_ids: Vec<SharedString>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    shows: Vec<Show>,
    sync_token: String,
}

/// Ask the collection service for a list (or its changes since `token`)
/// and look up what is new on it, newest first.
async fn fetch_set(set: Set, token: Option<String>) -> anyhow::Result<SetUpdate> {
    let changes = match &token {
        Some(token) => streaming::collection_changes(set.name(), token).await?,
        None => None,
    };
    let (whole, items, sync_token) = match changes {
        Some((mut items, sync_token)) => {
            items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
            (false, items, sync_token)
        }
        None => {
            let (items, sync_token) = streaming::collection(set.name()).await?;
            (true, items, sync_token)
        }
    };
    let id_of =
        |uri: &str| SharedString::from(uri.rsplit(':').next().unwrap_or_default().to_string());
    let removed: HashSet<SharedString> = items
        .iter()
        .filter(|i| i.removed)
        .map(|i| id_of(&i.uri))
        .collect();
    let added: Vec<String> = items
        .into_iter()
        .filter(|i| !i.removed)
        .map(|i| i.uri)
        .collect();
    let of_kind = |kind: &str| -> Vec<String> {
        added
            .iter()
            .filter(|u| u.starts_with(kind))
            .cloned()
            .collect()
    };
    let mut update = SetUpdate {
        whole,
        removed,
        track_ids: Vec::new(),
        albums: Vec::new(),
        artists: Vec::new(),
        shows: Vec::new(),
        sync_token,
    };
    match set {
        Set::Collection => {
            // Liked songs stay ids until their page opens; saved albums are
            // shown in the sidebar, so their details come now (one batch).
            update.track_ids = of_kind("spotify:track:")
                .iter()
                .map(|uri| id_of(uri))
                .collect();
            update.albums = streaming::albums_by_uri(&of_kind("spotify:album:")).await?;
        }
        Set::Artists => {
            update.artists = streaming::artists_by_uri(&of_kind("spotify:artist:")).await?
        }
        Set::Shows => update.shows = streaming::shows_by_uri(&of_kind("spotify:show:")).await?,
    }
    Ok(update)
}

fn lacks_details(playlist: &Playlist) -> bool {
    (playlist.images.is_empty() && playlist.total() > 0) || playlist.owner.display_name.is_none()
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
