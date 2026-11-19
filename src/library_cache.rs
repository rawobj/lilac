//! The library as last loaded, kept on disk so Lilac opens with it at once
//! and a restart asks Spotify only for what changed.
//!
//! One file for the signed-in account, cleared on logout. Everything in it
//! can be fetched again, so a missing or unreadable file only means a full
//! load.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::model::{Album, Artist, Playlist, Show, Track};

/// Bumped when the layout changes; older files are ignored.
const VERSION: u64 = 1;

#[derive(Default)]
pub struct Snapshot {
    /// The account it belongs to (empty when the profile was not known).
    pub user_id: String,
    /// Unix seconds.
    pub saved_at: u64,
    pub playlists: Vec<Playlist>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub shows: Vec<Show>,
    /// Newest first.
    pub liked: Vec<Track>,
    pub liked_total: u32,
    /// Every liked song's id, newest first (details may be missing).
    pub liked_order: Vec<String>,
    /// The collection service's token per list.
    pub sync_tokens: std::collections::HashMap<String, String>,
}

impl Snapshot {
    pub fn age_secs(&self) -> u64 {
        now().saturating_sub(self.saved_at)
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn file() -> std::path::PathBuf {
    crate::paths::cache_dir().join("library.json")
}

/// Reads the disk; call it off the UI thread.
pub fn load() -> Option<Snapshot> {
    let raw = std::fs::read(file()).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    if value.get("version")?.as_u64()? != VERSION {
        return None;
    }
    fn list<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Vec<T> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| serde_json::from_value(item.clone()).ok())
                    .collect()
            })
            .unwrap_or_default()
    }
    Some(Snapshot {
        user_id: value["user_id"].as_str().unwrap_or_default().to_string(),
        saved_at: value["saved_at"].as_u64().unwrap_or(0),
        playlists: list(&value, "playlists"),
        albums: list(&value, "albums"),
        artists: list(&value, "artists"),
        shows: list(&value, "shows"),
        liked: list(&value, "liked"),
        liked_total: value["liked_total"].as_u64().unwrap_or(0) as u32,
        liked_order: serde_json::from_value(value["liked_order"].clone()).unwrap_or_default(),
        sync_tokens: serde_json::from_value(value["sync_tokens"].clone()).unwrap_or_default(),
    })
}

/// Writes the disk; call it off the UI thread.
pub fn save(snapshot: &Snapshot) {
    let value = json!({
        "version": VERSION,
        "user_id": snapshot.user_id,
        "saved_at": snapshot.saved_at,
        "playlists": snapshot.playlists.iter().map(Playlist::to_api_json).collect::<Vec<_>>(),
        "albums": snapshot.albums.iter().map(Album::to_api_json).collect::<Vec<_>>(),
        "artists": snapshot.artists.iter().map(Artist::to_api_json).collect::<Vec<_>>(),
        "shows": snapshot.shows.iter().map(Show::to_api_json).collect::<Vec<_>>(),
        "liked": snapshot.liked.iter().map(Track::to_api_json).collect::<Vec<_>>(),
        "liked_total": snapshot.liked_total,
        "liked_order": snapshot.liked_order,
        "sync_tokens": snapshot.sync_tokens,
    });
    if let Ok(raw) = serde_json::to_vec(&value) {
        let path = file();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = crate::paths::write_private(&path, &raw);
    }
}

/// Forget the library, e.g. on logout.
pub fn clear() {
    let _ = std::fs::remove_file(file());
}

/// Liked songs after a check of the newest page: `fresh` is that page
/// (newest first), `known` what was kept. `None` when the two do not line
/// up and everything must be loaded again (likes removed, or more new ones
/// than one page holds).
pub fn merge_liked(fresh: &[Track], known: &[Track], total: u32) -> Option<Vec<Track>> {
    let overlap = fresh
        .iter()
        .position(|t| known.first().is_some_and(|k| k.id == t.id))?;
    let mut merged: Vec<Track> = fresh[..overlap].to_vec();
    merged.extend(known.iter().cloned());
    // The rest of the page must match what was kept, in order.
    let aligned = fresh[overlap..]
        .iter()
        .zip(&merged[overlap..])
        .all(|(a, b)| a.id == b.id);
    (aligned && merged.len() as u32 == total).then_some(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: id.to_string().into(),
            ..Default::default()
        }
    }

    fn ids(tracks: &[Track]) -> Vec<&str> {
        tracks.iter().map(|t| t.id.as_ref()).collect()
    }

    #[test]
    fn unchanged_likes_need_one_page() {
        let known = vec![track("a"), track("b"), track("c")];
        let fresh = known.clone();
        assert_eq!(
            ids(&merge_liked(&fresh, &known, 3).unwrap()),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn new_likes_go_on_top() {
        let known = vec![track("a"), track("b"), track("c")];
        let fresh = vec![track("n"), track("a"), track("b")];
        assert_eq!(
            ids(&merge_liked(&fresh, &known, 4).unwrap()),
            ["n", "a", "b", "c"]
        );
    }

    #[test]
    fn removed_likes_force_a_full_load() {
        let known = vec![track("a"), track("b"), track("c")];
        // "b" was unliked elsewhere.
        let fresh = vec![track("a"), track("c")];
        assert!(merge_liked(&fresh, &known, 2).is_none());
        // Removed further down than the page reaches: the total gives it away.
        let fresh = vec![track("a")];
        assert!(merge_liked(&fresh, &known, 2).is_none());
    }

    #[test]
    fn no_overlap_forces_a_full_load() {
        let known = vec![track("a")];
        assert!(merge_liked(&[track("x"), track("y")], &known, 3).is_none());
        assert!(merge_liked(&[track("x")], &[], 1).is_none());
    }

    #[test]
    fn library_survives_the_disk_format() {
        let album: Album = serde_json::from_str(
            r#"{"id":"al","uri":"spotify:album:al","name":"A","album_type":"single","release_date":"2024-01-02","total_tracks":3,
                "artists":[{"id":"x","name":"X"}],"images":[{"url":"m","width":300}]}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_value::<Album>(album.to_api_json()).unwrap(),
            album
        );
        let artist: Artist = serde_json::from_str(
            r#"{"id":"x","uri":"spotify:artist:x","name":"X","genres":["pop"],"followers":{"total":5},"images":[{"url":"s","width":64}]}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_value::<Artist>(artist.to_api_json()).unwrap(),
            artist
        );
        let show: Show = serde_json::from_str(
            r#"{"id":"s","uri":"spotify:show:s","name":"S","publisher":"P","total_episodes":9}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_value::<Show>(show.to_api_json()).unwrap(),
            show
        );
    }
}
