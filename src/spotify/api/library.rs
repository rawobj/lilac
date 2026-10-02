//! The user's own collection: playlists, saved items, follows.

use anyhow::Result;
use reqwest::Method;
use serde_json::json;

use super::{Api, is_unsupported};
use crate::model::*;

impl Api {
    /// Every playlist the user owns or follows.
    pub async fn my_playlists(&self) -> Result<Vec<Playlist>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page: Page<Playlist> = self
                .get(&format!("/me/playlists?limit=50&offset={offset}"))
                .await?;
            let done = page.next.is_none() || page.items.is_empty();
            offset += 50;
            out.extend(page.items);
            if done || offset >= 1000 {
                return Ok(out);
            }
        }
    }

    pub async fn saved_tracks(&self, offset: u32) -> Result<Page<SavedItem>> {
        self.get(&format!("/me/tracks?limit=50&offset={offset}"))
            .await
    }

    pub async fn saved_albums(&self) -> Result<Vec<Album>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let page: Page<SavedAlbum> = self
                .get(&format!("/me/albums?limit=50&offset={offset}"))
                .await?;
            let done = page.next.is_none() || page.items.is_empty();
            offset += 50;
            out.extend(page.items.into_iter().map(|s| s.album));
            if done || offset >= 500 {
                return Ok(out);
            }
        }
    }

    pub async fn followed_artists(&self) -> Result<Vec<Artist>> {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            artists: Cursor,
        }
        #[derive(serde::Deserialize)]
        struct Cursor {
            #[serde(default)]
            items: Vec<Artist>,
            #[serde(default)]
            next: Option<String>,
        }
        let mut out = Vec::new();
        let mut url = "/me/following?type=artist&limit=50".to_string();
        for _ in 0..10 {
            let page: Wrapper = self.get(&url).await?;
            out.extend(page.artists.items);
            match page.artists.next {
                Some(next) => url = next,
                None => break,
            }
        }
        Ok(out)
    }

    pub async fn saved_shows(&self) -> Result<Vec<Show>> {
        let page: Page<SavedShow> = self.get("/me/shows?limit=50").await?;
        Ok(page.items.into_iter().map(|s| s.show).collect())
    }

    pub async fn recently_played(&self) -> Result<Vec<Track>> {
        let page: Page<SavedItem> = self.get("/me/player/recently-played?limit=50").await?;
        let mut seen = std::collections::HashSet::new();
        Ok(page
            .items
            .into_iter()
            .filter_map(|i| i.item)
            .filter(|t| seen.insert(t.id.clone()))
            .collect())
    }

    pub async fn top_tracks(&self, range: &str, limit: u32) -> Result<Vec<Track>> {
        let page: Page<Track> = self
            .get(&format!("/me/top/tracks?time_range={range}&limit={limit}"))
            .await?;
        Ok(page.items)
    }

    pub async fn top_artists(&self, range: &str, limit: u32) -> Result<Vec<Artist>> {
        let page: Page<Artist> = self
            .get(&format!("/me/top/artists?time_range={range}&limit={limit}"))
            .await?;
        Ok(page.items)
    }

    // ------------------------------------------------------------ mutations

    /// Save or remove items by URI.
    ///
    /// The unified `/me/library` endpoint replaced the per-type ones in 2026.
    /// Older deployments still answer on the legacy paths, so fall back to
    /// them when the new one is not available.
    pub async fn set_saved(&self, uris: &[String], saved: bool) -> Result<()> {
        if uris.is_empty() {
            return Ok(());
        }
        let method = if saved { Method::PUT } else { Method::DELETE };
        let mut result = Ok(());
        for chunk in uris.chunks(40) {
            let path = format!("/me/library?uris={}", super::super::enc(&chunk.join(",")));
            result = self.call(method.clone(), &path, None).await;
            if result.is_err() {
                break;
            }
        }
        match result {
            Err(err) if is_unsupported(&err) => self.set_saved_legacy(uris, saved).await,
            other => other,
        }
    }

    async fn set_saved_legacy(&self, uris: &[String], saved: bool) -> Result<()> {
        let method = if saved { Method::PUT } else { Method::DELETE };
        for uri in uris {
            let mut parts = uri.splitn(3, ':').skip(1);
            let (Some(kind), Some(id)) = (parts.next(), parts.next()) else {
                continue;
            };
            let path = match kind {
                "track" => format!("/me/tracks?ids={id}"),
                "album" => format!("/me/albums?ids={id}"),
                "episode" => format!("/me/episodes?ids={id}"),
                "show" => format!("/me/shows?ids={id}"),
                "artist" => format!("/me/following?type=artist&ids={id}"),
                "playlist" => format!("/playlists/{id}/followers"),
                _ => continue,
            };
            self.call(method.clone(), &path, None).await?;
        }
        Ok(())
    }

    /// Whether each of `uris` is in the user's library.
    pub async fn contains(&self, uris: &[String]) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(uris.len());
        for chunk in uris.chunks(40) {
            let path = format!(
                "/me/library/contains?uris={}",
                super::super::enc(&chunk.join(","))
            );
            match self.get::<Vec<bool>>(&path).await {
                Ok(flags) => out.extend(flags),
                Err(err) if is_unsupported(&err) => {
                    let ids: Vec<&str> =
                        chunk.iter().filter_map(|u| u.rsplit(':').next()).collect();
                    let flags: Vec<bool> = self
                        .get(&format!("/me/tracks/contains?ids={}", ids.join(",")))
                        .await?;
                    out.extend(flags);
                }
                Err(err) => return Err(err),
            }
        }
        out.resize(uris.len(), false);
        Ok(out)
    }

    pub async fn create_playlist(
        &self,
        user_id: &str,
        name: &str,
        description: &str,
    ) -> Result<Playlist> {
        let body = json!({ "name": name, "description": description, "public": false });
        let text = match self.send(Method::POST, "/me/playlists", Some(&body)).await {
            Err(err) if is_unsupported(&err) => {
                self.send(
                    Method::POST,
                    &format!("/users/{user_id}/playlists"),
                    Some(&body),
                )
                .await?
            }
            other => other?,
        };
        Ok(serde_json::from_str(&text.unwrap_or_default())?)
    }

    pub async fn update_playlist(&self, id: &str, name: &str, description: &str) -> Result<()> {
        let mut body = json!({ "name": name });
        // An empty description is rejected; omit it to leave the old one in place.
        if !description.is_empty() {
            body["description"] = json!(description);
        }
        self.call(Method::PUT, &format!("/playlists/{id}"), Some(body))
            .await
    }

    pub async fn add_to_playlist(&self, id: &str, uris: &[String]) -> Result<()> {
        for chunk in uris.chunks(100) {
            let body = json!({ "uris": chunk });
            match self
                .call(
                    Method::POST,
                    &format!("/playlists/{id}/items"),
                    Some(body.clone()),
                )
                .await
            {
                Err(err) if is_unsupported(&err) => {
                    self.call(Method::POST, &format!("/playlists/{id}/tracks"), Some(body))
                        .await?
                }
                other => other?,
            }
        }
        Ok(())
    }

    pub async fn remove_from_playlist(&self, id: &str, uris: &[String]) -> Result<()> {
        for chunk in uris.chunks(100) {
            let items: Vec<_> = chunk.iter().map(|uri| json!({ "uri": uri })).collect();
            let new = json!({ "items": items });
            match self
                .call(Method::DELETE, &format!("/playlists/{id}/items"), Some(new))
                .await
            {
                Err(err) if is_unsupported(&err) => {
                    let old = json!({ "tracks": items });
                    self.call(
                        Method::DELETE,
                        &format!("/playlists/{id}/tracks"),
                        Some(old),
                    )
                    .await?
                }
                other => other?,
            }
        }
        Ok(())
    }
}
