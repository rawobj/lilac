//! Catalog lookups: albums, artists, playlists, shows and search.

use anyhow::Result;

use super::super::enc;
use super::{Api, is_unsupported, status_of};
use serde_json::{Value, json};

use crate::disk_cache;
use crate::model::*;
use crate::spotify::streaming;

fn read_album(value: &Value) -> Option<(Album, Vec<Track>)> {
    // Artist names follow the language they were asked in.
    if value.get("lang")?.as_str()? != streaming::language() {
        return None;
    }
    let album = serde_json::from_value(value.get("album")?.clone()).ok()?;
    let tracks = serde_json::from_value(value.get("tracks")?.clone()).ok()?;
    Some((album, tracks))
}

impl Api {
    pub async fn album(&self, id: &str) -> Result<Album> {
        self.get(&format!("/albums/{id}")).await
    }

    /// All tracks of an album. The album object itself carries the first page.
    pub async fn album_tracks(&self, album: &Album) -> Result<Vec<Track>> {
        let mut tracks = album
            .tracks
            .as_ref()
            .map(|p| p.items.clone())
            .unwrap_or_default();
        let mut more = album.tracks.as_ref().is_none_or(|p| p.next.is_some());
        while more && tracks.len() < 1000 {
            let page: Page<Track> = self
                .get(&format!(
                    "/albums/{}/tracks?limit=50&offset={}",
                    album.id,
                    tracks.len()
                ))
                .await?;
            more = page.next.is_some() && !page.items.is_empty();
            tracks.extend(page.items);
        }
        // Album tracks come without their album; attach it so rows have artwork.
        let album_ref = album.as_ref();
        for track in &mut tracks {
            track.album = Some(album_ref.clone());
        }
        Ok(tracks)
    }

    /// An album and all its tracks: the copy kept on disk when there is
    /// one, else from Spotify. For playing; the album page shows the kept
    /// copy and checks for changes behind it ([`Self::album_kept`],
    /// [`Self::fetch_album`]).
    pub async fn album_with_tracks(&self, id: &str) -> Result<(Album, Vec<Track>)> {
        if let Some((album, _)) = self.album_kept(id).await {
            return Ok(album);
        }
        self.fetch_album(id).await
    }

    /// The album as kept on disk, with its age.
    pub async fn album_kept(&self, id: &str) -> Option<((Album, Vec<Track>), std::time::Duration)> {
        if self.is_demo() {
            return None;
        }
        let key = id.to_string();
        tokio::task::spawn_blocking(move || disk_cache::ALBUMS.get(&key))
            .await
            .ok()
            .flatten()
            .and_then(|hit| Some((read_album(&hit.value)?, hit.age)))
    }

    /// The album from Spotify (the streaming session, the Web API only
    /// when that fails), kept on disk for next time.
    pub async fn fetch_album(&self, id: &str) -> Result<(Album, Vec<Track>)> {
        if self.is_demo() {
            let album = self.album(id).await?;
            let tracks = self.album_tracks(&album).await?;
            return Ok((album, tracks));
        }
        let (album, tracks) = match streaming::album(id).await {
            Ok(found) => found,
            Err(err) => {
                crate::note!("lilac: album from the session failed: {err:#}");
                let album = self.album(id).await?;
                let tracks = self.album_tracks(&album).await?;
                (album, tracks)
            }
        };
        let value = json!({
            "lang": streaming::language(),
            "album": album.to_api_json(),
            "tracks": tracks.iter().map(Track::to_api_json).collect::<Vec<_>>(),
        });
        let key = id.to_string();
        tokio::task::spawn_blocking(move || disk_cache::ALBUMS.put(&key, &value));
        Ok((album, tracks))
    }

    /// From the streaming session; the Web API only when that fails.
    pub async fn playlist(&self, id: &str) -> Result<Playlist> {
        if !self.is_demo() {
            match streaming::playlist(id).await {
                Ok(playlist) => return Ok(playlist),
                Err(err) => crate::note!("lilac: playlist from the session failed: {err:#}"),
            }
        }
        self.get(&format!("/playlists/{id}")).await
    }

    /// From the streaming session; the Web API only when that fails (and
    /// it lists only the user's own playlists for Lilac's app).
    pub async fn playlist_items(&self, id: &str, offset: u32) -> Result<Page<SavedItem>> {
        if !self.is_demo()
            && let Ok(page) = streaming::playlist_items(id, offset).await
        {
            return Ok(page);
        }
        let query = format!("limit=100&offset={offset}&additional_types=track,episode");
        match self.get(&format!("/playlists/{id}/items?{query}")).await {
            Err(err) if status_of(&err) != Some(403) && is_unsupported(&err) => {
                self.get(&format!("/playlists/{id}/tracks?{query}")).await
            }
            other => other,
        }
    }

    pub async fn artist(&self, id: &str) -> Result<Artist> {
        self.get(&format!("/artists/{id}")).await
    }

    pub async fn artist_albums(&self, id: &str) -> Result<Vec<Album>> {
        let mut out: Vec<Album> = Vec::new();
        let mut offset = 0;
        loop {
            let page: Page<Album> = self
                .get(&format!(
                    "/artists/{id}/albums?include_groups=album,single,compilation&limit=50&offset={offset}"
                ))
                .await?;
            let done = page.next.is_none() || page.items.is_empty();
            offset += 50;
            out.extend(page.items);
            if done || offset >= 200 {
                break;
            }
        }
        // The API returns one entry per market variant; keep the first of each.
        let mut seen = std::collections::HashSet::new();
        out.retain(|a| seen.insert((a.name.to_lowercase(), a.album_type.clone())));
        Ok(out)
    }

    /// An artist's newest albums and singles, one page only. Used by the
    /// What's New feed, where the full discography would be wasted requests.
    pub async fn latest_releases(&self, id: &str) -> Result<Vec<Album>> {
        let page: Page<Album> = self
            .get(&format!(
                "/artists/{id}/albums?include_groups=album,single&limit=10&offset=0"
            ))
            .await?;
        Ok(page.items)
    }

    /// An artist's most popular tracks.
    ///
    /// The dedicated endpoint was retired for third-party apps, so when it is
    /// gone approximate it with a search scoped to the artist.
    pub async fn artist_top_tracks(&self, id: &str, name: &str) -> Result<Vec<Track>> {
        #[derive(serde::Deserialize)]
        struct Top {
            tracks: Vec<Track>,
        }
        match self
            .get::<Top>(&format!("/artists/{id}/top-tracks?market=from_token"))
            .await
        {
            Ok(top) => Ok(top.tracks),
            Err(err) if is_unsupported(&err) => {
                let results = self
                    .search(&format!("artist:\"{name}\""), "track", 10)
                    .await?;
                Ok(results
                    .tracks
                    .items
                    .into_iter()
                    .filter(|t| t.artists.iter().any(|a| a.id == id))
                    .collect())
            }
            Err(err) => Err(err),
        }
    }

    /// Artists similar to this one. Empty when the endpoint is unavailable.
    pub async fn related_artists(&self, id: &str) -> Result<Vec<Artist>> {
        #[derive(serde::Deserialize)]
        struct Related {
            artists: Vec<Artist>,
        }
        match self
            .get::<Related>(&format!("/artists/{id}/related-artists"))
            .await
        {
            Ok(related) => Ok(related.artists),
            Err(err) if is_unsupported(&err) => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }

    /// A track or episode by `spotify:` URI.
    pub async fn item(&self, uri: &str) -> Result<Track> {
        let id = uri.rsplit(':').next().unwrap_or_default();
        if uri.starts_with("spotify:episode:") {
            let mut episode: Track = self.get(&format!("/episodes/{id}")).await?;
            episode.kind = ItemKind::Episode;
            Ok(episode)
        } else {
            self.get(&format!("/tracks/{id}")).await
        }
    }

    /// From the streaming session; the Web API only when that fails.
    pub async fn show(&self, id: &str) -> Result<Show> {
        if !self.is_demo()
            && let Ok(show) = streaming::show(id).await
        {
            return Ok(show);
        }
        self.get(&format!("/shows/{id}")).await
    }

    /// From the streaming session; the Web API only when that fails.
    pub async fn show_episodes(&self, show: &Show, offset: u32) -> Result<Page<Track>> {
        if !self.is_demo()
            && let Ok(page) = streaming::show_episodes(show, offset).await
        {
            return Ok(page);
        }
        let mut page: Page<Track> = self
            .get(&format!(
                "/shows/{}/episodes?limit=50&offset={offset}",
                show.id
            ))
            .await?;
        let show_ref = show.as_ref();
        for episode in &mut page.items {
            episode.kind = ItemKind::Episode;
            episode.show = Some(show_ref.clone());
        }
        Ok(page)
    }

    /// Search across `types` (comma separated). `limit` is clamped to what the API allows.
    pub async fn search(&self, query: &str, types: &str, limit: u32) -> Result<SearchResults> {
        let limit = limit.clamp(1, 10);
        let mut results: SearchResults = self
            .get(&format!(
                "/search?q={}&type={types}&limit={limit}",
                enc(query)
            ))
            .await?;
        for episode in &mut results.episodes.items {
            episode.kind = ItemKind::Episode;
        }
        Ok(results)
    }

    /// New album releases, or an empty list where the endpoint is gone.
    pub async fn new_releases(&self) -> Result<Vec<Album>> {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            albums: Page<Album>,
        }
        match self.get::<Wrapper>("/browse/new-releases?limit=20").await {
            Ok(wrapper) => Ok(wrapper.albums.items),
            Err(err) if is_unsupported(&err) => Ok(self
                .search("tag:new", "album", 10)
                .await
                .map(|r| r.albums.items)
                .unwrap_or_default()),
            Err(err) => Err(err),
        }
    }
}
