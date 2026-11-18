//! Playlists opened before, kept so opening one again shows it at once.
//!
//! The playlist page shows the cached copy immediately and checks for
//! changes behind it (see `pages::collection`). Copies live in memory and
//! on disk, so they also survive a restart; both are bounded, and a copy
//! older than [`TTL`] is not shown at all.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::model::{Playlist, Track};

/// Older copies are not worth showing, even for a moment.
const TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Playlists kept on disk.
const MAX: usize = 150;
/// Playlists kept in memory; the disk has the rest.
const IN_MEMORY: usize = 16;

pub struct Cached {
    pub playlist: Playlist,
    pub tracks: Vec<Track>,
    /// Unix seconds.
    saved_at: u64,
}

impl Cached {
    fn fresh(&self) -> bool {
        now().saturating_sub(self.saved_at) < TTL.as_secs()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Most recently used first.
type Memory = Mutex<Vec<(String, Arc<Cached>)>>;

fn memory() -> &'static Memory {
    static MEMORY: OnceLock<Memory> = OnceLock::new();
    MEMORY.get_or_init(Default::default)
}

fn remember(id: &str, cached: Arc<Cached>) {
    let mut memory = memory().lock().unwrap();
    memory.retain(|(key, _)| key != id);
    memory.insert(0, (id.to_string(), cached));
    memory.truncate(IN_MEMORY);
}

fn file(id: &str) -> Option<PathBuf> {
    // Playlist ids are base62; anything else is not a file name we write.
    id.chars()
        .all(|c| c.is_ascii_alphanumeric())
        .then(|| crate::paths::playlist_cache_dir().join(format!("{id}.json")))
}

/// The cached copy, if there is a fresh one. Reads the disk when it is not
/// in memory, so call it off the UI thread.
pub fn load(id: &str) -> Option<Arc<Cached>> {
    let hit = memory()
        .lock()
        .unwrap()
        .iter()
        .find(|(key, _)| key == id)
        .map(|(_, cached)| cached.clone());
    if let Some(hit) = hit {
        return hit.fresh().then_some(hit);
    }
    let raw = std::fs::read(file(id)?).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    let cached = Arc::new(Cached {
        playlist: serde_json::from_value(value.get("playlist")?.clone()).ok()?,
        tracks: serde_json::from_value(value.get("tracks")?.clone()).ok()?,
        saved_at: value.get("saved_at")?.as_u64()?,
    });
    if !cached.fresh() {
        return None;
    }
    remember(id, cached.clone());
    Some(cached)
}

/// Keep the playlist as it was just loaded. The disk copy is written on the
/// I/O runtime.
pub fn store(id: &str, playlist: Playlist, tracks: Vec<Track>) {
    let cached = Arc::new(Cached {
        playlist,
        tracks,
        saved_at: now(),
    });
    remember(id, cached.clone());
    let id = id.to_string();
    crate::runtime::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || write(&id, &cached)).await;
    });
}

fn write(id: &str, cached: &Cached) {
    let Some(path) = file(id) else { return };
    let value = json!({
        "saved_at": cached.saved_at,
        "playlist": cached.playlist.to_api_json(),
        "tracks": cached.tracks.iter().map(Track::to_api_json).collect::<Vec<_>>(),
    });
    let Ok(raw) = serde_json::to_vec(&value) else {
        return;
    };
    if crate::paths::write_private(&path, &raw).is_ok() {
        prune();
    }
}

/// Keep the newest [`MAX`] files.
fn prune() {
    let Ok(entries) = std::fs::read_dir(crate::paths::playlist_cache_dir()) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= MAX {
        return;
    }
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in files.into_iter().skip(MAX) {
        let _ = std::fs::remove_file(path);
    }
}

/// Forget every playlist, e.g. on logout.
pub fn clear() {
    memory().lock().unwrap().clear();
    let _ = std::fs::remove_dir_all(crate::paths::playlist_cache_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_expire() {
        let cached = Cached {
            playlist: Playlist::default(),
            tracks: Vec::new(),
            saved_at: now(),
        };
        assert!(cached.fresh());
        let old = Cached {
            saved_at: now() - TTL.as_secs() - 1,
            ..cached
        };
        assert!(!old.fresh());
    }

    #[test]
    fn only_plain_ids_become_file_names() {
        assert!(file("37i9dQZF1DXcBWIGoYBM5M").is_some());
        assert!(file("../settings").is_none());
    }

    #[test]
    fn tracks_survive_the_disk_format() {
        let raw = r#"{"id":"t","uri":"spotify:track:t","name":"Song","type":"track","duration_ms":1000,
            "artists":[{"id":"a","name":"Artist"}],
            "album":{"id":"al","name":"Album","images":[{"url":"big","width":640},{"url":"small","width":64}]}}"#;
        let track: Track = serde_json::from_str(raw).unwrap();
        let back: Track = serde_json::from_value(track.to_api_json()).unwrap();
        assert_eq!(back, track);
        let playlist: Playlist = serde_json::from_str(
            r#"{"id":"p","name":"Mix","snapshot_id":"s1","owner":{"id":"o","display_name":"Owner"},"items":{"total":3}}"#,
        )
        .unwrap();
        let back: Playlist = serde_json::from_value(playlist.to_api_json()).unwrap();
        assert_eq!(back, playlist);
    }
}
