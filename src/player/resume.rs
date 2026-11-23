//! What was playing when Lilac stopped, so the next start can load it again,
//! paused at the same position.
//!
//! Spotify only reports the last session while some device is still active,
//! which is never the case right after Lilac itself was closed.

use serde::{Deserialize, Serialize};

use crate::model::Track;

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
        self.track = (track.uri.as_ref() == self.track_uri).then(|| track.to_api_json());
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

#[cfg(test)]
mod tests {
    use super::*;

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
