//! What Lilac played, newest first, kept on disk: Home's "Recently played"
//! without asking Spotify, and a stand-in for the listening stats Spotify's
//! Web API gives when it cannot be asked. Only plays on this computer are
//! seen; Spotify's own history covers other devices.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::SharedString;
use serde_json::{Value, json};

use crate::model::{ArtistRef, Track};

/// Plays kept.
const KEPT: usize = 100;

struct Play {
    track: Track,
    /// Unix seconds.
    at: u64,
}

static PLAYS: Mutex<Option<VecDeque<Play>>> = Mutex::new(None);

fn file() -> std::path::PathBuf {
    crate::paths::cache_dir().join("history.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn with<R>(f: impl FnOnce(&mut VecDeque<Play>) -> R) -> R {
    let mut plays = PLAYS.lock().unwrap_or_else(|e| e.into_inner());
    let plays = plays.get_or_insert_with(load);
    f(plays)
}

fn load() -> VecDeque<Play> {
    let Ok(raw) = std::fs::read(file()) else {
        return VecDeque::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_slice::<Value>(&raw) else {
        return VecDeque::new();
    };
    items
        .into_iter()
        .filter_map(|item| {
            Some(Play {
                track: serde_json::from_value(item.get("track")?.clone()).ok()?,
                at: item.get("at")?.as_u64()?,
            })
        })
        .collect()
}

/// A track started playing here. Writes the disk on the I/O runtime.
pub fn record(track: &Track) {
    if track.id.is_empty() {
        return;
    }
    let snapshot = with(|plays| {
        // The same track again right away (a seek, a resume, its details
        // in the user's language arriving) is one play, with the newer
        // details.
        match plays.front_mut() {
            Some(front) if front.track.id == track.id => front.track = track.clone(),
            _ => plays.push_front(Play {
                track: track.clone(),
                at: now(),
            }),
        }
        plays.truncate(KEPT);
        Some(
            plays
                .iter()
                .map(|p| json!({ "track": p.track.to_api_json(), "at": p.at }))
                .collect::<Vec<_>>(),
        )
    });
    if let Some(snapshot) = snapshot {
        crate::runtime::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(raw) = serde_json::to_vec(&snapshot) {
                    let _ = crate::paths::write_private(&file(), &raw);
                }
            })
            .await;
        });
    }
}

/// Recently played tracks, newest first, each once.
pub fn recent(limit: usize) -> Vec<Track> {
    with(|plays| {
        let mut seen = std::collections::HashSet::new();
        plays
            .iter()
            .filter(|p| seen.insert(p.track.id.clone()))
            .map(|p| p.track.clone())
            .take(limit)
            .collect()
    })
}

/// The most played tracks of the last `days`, most played first.
pub fn top_tracks(days: u64, limit: usize) -> Vec<Track> {
    let since = now().saturating_sub(days * 24 * 3600);
    with(|plays| {
        let mut counts: HashMap<SharedString, (usize, &Track)> = HashMap::new();
        for play in plays.iter().filter(|p| p.at >= since) {
            counts
                .entry(play.track.id.clone())
                .or_insert((0, &play.track))
                .0 += 1;
        }
        let mut ranked: Vec<(usize, &Track)> = counts.into_values().collect();
        ranked.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        ranked
            .into_iter()
            .take(limit)
            .map(|(_, t)| t.clone())
            .collect()
    })
}

/// The most played artists of the last `days`, most played first.
pub fn top_artists(days: u64, limit: usize) -> Vec<ArtistRef> {
    let since = now().saturating_sub(days * 24 * 3600);
    with(|plays| {
        let mut counts: HashMap<SharedString, (usize, ArtistRef)> = HashMap::new();
        for play in plays.iter().filter(|p| p.at >= since) {
            for artist in &play.track.artists {
                if artist.id.is_empty() {
                    continue;
                }
                counts
                    .entry(artist.id.clone())
                    .or_insert((0, artist.clone()))
                    .0 += 1;
            }
        }
        let mut ranked: Vec<(usize, ArtistRef)> = counts.into_values().collect();
        ranked.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        ranked.into_iter().take(limit).map(|(_, a)| a).collect()
    })
}

/// Forget everything, e.g. on logout.
pub fn clear() {
    with(|plays| plays.clear());
    let _ = std::fs::remove_file(file());
}
