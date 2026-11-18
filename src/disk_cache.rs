//! Small JSON records kept on disk for things that rarely change: song and
//! album details, lyrics. A record older than its store's limit is not
//! returned; callers that want to show an old copy while they check for a
//! new one look at `Hit::age`.
//!
//! Each record is one file under `~/.cache/lilac/<store>/`, named by a
//! base62 id. Stores are bounded by file count; the oldest files go first.
//! Everything here can be fetched again, so read and write errors only mean
//! a fetch. Never used for audio.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;

pub struct Store {
    dir: &'static str,
    /// Records older than this are treated as missing.
    pub max_age: Duration,
    max_files: usize,
    writes: AtomicU32,
}

const DAY: u64 = 24 * 3600;

/// Song details (names, artists, album, artwork URLs).
pub static TRACKS: Store = Store::new("tracks", 60, 40_000);
/// Albums with their track lists.
pub static ALBUMS: Store = Store::new("albums", 60, 2_000);
/// Lyrics; also "none found", which callers expire sooner.
pub static LYRICS: Store = Store::new("lyrics", 180, 10_000);

/// Prune every this many writes, not on each one.
const PRUNE_EVERY: u32 = 200;

pub struct Hit {
    pub value: Value,
    pub age: Duration,
}

impl Store {
    const fn new(dir: &'static str, days: u64, max_files: usize) -> Store {
        Store {
            dir,
            max_age: Duration::from_secs(days * DAY),
            max_files,
            writes: AtomicU32::new(0),
        }
    }

    fn dir(&self) -> PathBuf {
        crate::paths::cache_dir().join(self.dir)
    }

    fn file(&self, key: &str) -> Option<PathBuf> {
        // Keys are Spotify ids (optionally with a suffix like "-ja");
        // anything else is not a file name we write.
        (!key.is_empty()
            && key.len() <= 64
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .then(|| self.dir().join(format!("{key}.json")))
    }

    /// Blocking file read; call off the UI thread.
    pub fn get(&self, key: &str) -> Option<Hit> {
        let path = self.file(key)?;
        let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if age > self.max_age {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let raw = std::fs::read(&path).ok()?;
        let value = serde_json::from_slice(&raw).ok()?;
        Some(Hit { value, age })
    }

    /// Blocking file write; call off the UI thread.
    pub fn put(&self, key: &str, value: &Value) {
        let Some(path) = self.file(key) else { return };
        let Ok(raw) = serde_json::to_vec(value) else {
            return;
        };
        let _ = std::fs::create_dir_all(self.dir());
        if std::fs::write(&path, raw).is_ok()
            && self
                .writes
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(PRUNE_EVERY)
        {
            self.prune();
        }
    }

    /// Keep the newest `max_files` records.
    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(self.dir()) else {
            return;
        };
        let mut files: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
            .collect();
        if files.len() <= self.max_files {
            return;
        }
        files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        for (_, path) in files.into_iter().skip(self.max_files) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Forget everything, e.g. on logout.
pub fn clear() {
    for store in [&TRACKS, &ALBUMS, &LYRICS] {
        let _ = std::fs::remove_dir_all(store.dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_keys_become_file_names() {
        assert!(TRACKS.file("4MEr1Y8pLCtZ4gW5MriwB9").is_some());
        assert!(TRACKS.file("4MEr1Y8pLCtZ4gW5MriwB9-en").is_some());
        assert!(TRACKS.file("../settings").is_none());
        assert!(TRACKS.file("").is_none());
    }
}
