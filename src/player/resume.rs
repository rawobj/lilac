//! What was playing when Lilac stopped, so the next start can load it again,
//! paused at the same position.
//!
//! Spotify only reports the last session while some device is still active,
//! which is never the case right after Lilac itself was closed.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::model::{Images, ItemKind, Track};

/// Explicit track lists are kept for contexts without a URI (e.g. a search
/// result); bounded so the file stays small.
const MAX_TRACKS: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LastSession {
    pub track_uri: String,
    #[serde(default)]
    pub context_uri: Option<String>,
    /// The tracks played from when there was no context URI.
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub position_ms: u32,
    /// Unix seconds; compared with Spotify's recently played list to tell
    /// whether something was played elsewhere since.
    #[serde(default)]
    pub saved_at: u64,
    /// What the player bar shows for the track, in the Web API's shape, so
    /// the next start can show it before anything is fetched.
    #[serde(default)]
    pub track: Option<serde_json::Value>,
}

impl LastSession {
    pub fn new(
        track_uri: &str,
        context_uri: Option<&str>,
        tracks: &[String],
        position_ms: u32,
    ) -> Self {
        let tracks = if context_uri.is_some() {
            Vec::new()
        } else {
            // Keep the window around the current track.
            let at = tracks.iter().position(|t| t == track_uri).unwrap_or(0);
            let from = at.saturating_sub(MAX_TRACKS / 4);
            tracks.iter().skip(from).take(MAX_TRACKS).cloned().collect()
        };
        Self {
            track_uri: track_uri.to_string(),
            context_uri: context_uri.map(str::to_string),
            tracks,
            position_ms,
            saved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            track: None,
        }
    }

    pub fn with_track(mut self, track: &Track) -> Self {
        self.track = (track.uri.as_ref() == self.track_uri).then(|| track_json(track));
        self
    }

    /// The saved track's details, if they belong to the saved track.
    pub fn track(&self) -> Option<Track> {
        let track: Track = serde_json::from_value(self.track.clone()?).ok()?;
        (track.uri.as_ref() == self.track_uri).then_some(track)
    }

    pub fn load() -> Option<Self> {
        let raw = std::fs::read(crate::paths::last_session_file()).ok()?;
        serde_json::from_slice(&raw)
            .ok()
            .filter(|s: &Self| !s.track_uri.is_empty())
    }

    pub fn save(&self) {
        if let Ok(raw) = serde_json::to_vec(self) {
            let _ = crate::paths::write_private(&crate::paths::last_session_file(), &raw);
        }
    }

    pub fn forget() {
        let _ = std::fs::remove_file(crate::paths::last_session_file());
    }
}

fn images_json(images: &Images) -> serde_json::Value {
    let sized = [
        (&images.small, 64),
        (&images.medium, 300),
        (&images.large, 640),
    ];
    sized
        .into_iter()
        .filter_map(|(url, width)| Some(json!({ "url": url.as_ref()?.as_ref(), "width": width })))
        .collect()
}

/// Enough of a track for the player bar, readable by `Track`'s deserializer.
fn track_json(track: &Track) -> serde_json::Value {
    let artists: Vec<_> = track
        .artists
        .iter()
        .map(|a| json!({ "id": a.id.as_ref(), "name": a.name.as_ref() }))
        .collect();
    json!({
        "id": track.id.as_ref(),
        "uri": track.uri.as_ref(),
        "name": track.name.as_ref(),
        "type": if track.kind == ItemKind::Episode { "episode" } else { "track" },
        "duration_ms": track.duration_ms,
        "explicit": track.explicit,
        "artists": artists,
        "album": track.album.as_ref().map(|a| json!({
            "id": a.id.as_ref(),
            "name": a.name.as_ref(),
            "images": images_json(&a.images),
        })),
        "show": track.show.as_ref().map(|s| json!({
            "id": s.id.as_ref(),
            "name": s.name.as_ref(),
            "images": images_json(&s.images),
        })),
        "images": images_json(&track.images),
    })
}

/// Unix seconds of an RFC 3339 UTC timestamp as Spotify writes them
/// (`2026-11-14T12:34:56.789Z`).
pub fn parse_utc(stamp: &str) -> Option<u64> {
    let (date, time) = stamp.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '.']).next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>());
    let (h, min, sec) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    // Days since the epoch for a proleptic Gregorian date (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + sec).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spotify_timestamps() {
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2026-11-14T12:34:56.789Z"), Some(1_794_659_696));
        assert_eq!(parse_utc("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_utc("nonsense"), None);
    }

    #[test]
    fn saved_track_details_read_back() {
        let raw = r#"{"id":"t","uri":"spotify:track:t","name":"Song","type":"track","duration_ms":1000,
            "artists":[{"id":"a","name":"Artist"}],
            "album":{"id":"al","name":"Album","images":[{"url":"big","width":640},{"url":"small","width":64}]}}"#;
        let track: Track = serde_json::from_str(raw).unwrap();
        let s = LastSession::new("spotify:track:t", None, &[], 0).with_track(&track);
        let back: LastSession = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back.track(), Some(track.clone()));
        // Details of another track are not kept.
        let s = LastSession::new("spotify:track:other", None, &[], 0).with_track(&track);
        assert_eq!(s.track(), None);
    }

    #[test]
    fn context_sessions_do_not_store_track_lists() {
        let tracks: Vec<String> = (0..10).map(|i| format!("spotify:track:{i}")).collect();
        let s = LastSession::new("spotify:track:3", Some("spotify:album:x"), &tracks, 5);
        assert!(s.tracks.is_empty());
        let s = LastSession::new("spotify:track:3", None, &tracks, 5);
        assert_eq!(s.tracks, tracks);
    }

    #[test]
    fn long_lists_keep_a_window_around_the_current_track() {
        let tracks: Vec<String> = (0..1000).map(|i| format!("spotify:track:{i}")).collect();
        let s = LastSession::new("spotify:track:700", None, &tracks, 0);
        assert_eq!(s.tracks.len(), MAX_TRACKS);
        assert!(s.tracks.contains(&"spotify:track:700".to_string()));
    }
}
