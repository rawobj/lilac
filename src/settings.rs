//! User preferences, persisted as JSON.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Low,
    Normal,
    High,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Low, Quality::Normal, Quality::High];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low · 96 kbps",
            Quality::Normal => "Normal · 160 kbps",
            Quality::High => "Very high · 320 kbps",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub quality: Quality,
    pub normalize: bool,
    pub gapless: bool,
    pub autoplay_on_start: bool,
    pub sidebar_collapsed: bool,
    pub show_now_playing: bool,
    pub compact_library: bool,
    pub reduce_transparency: bool,
    pub media_keys: bool,
    /// Look up lyrics on lrclib.net when Spotify has none. Sends title and artist there.
    pub lrclib_lyrics: bool,
    /// Release date of the newest What's New item the user has seen.
    pub whats_new_seen: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            quality: Quality::High,
            normalize: true,
            gapless: true,
            autoplay_on_start: false,
            sidebar_collapsed: false,
            show_now_playing: true,
            compact_library: false,
            reduce_transparency: false,
            media_keys: true,
            lrclib_lyrics: false,
            whats_new_seen: String::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read(crate::paths::settings_file())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(raw) = serde_json::to_vec_pretty(self) {
            let _ = crate::paths::write_private(&crate::paths::settings_file(), &raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"volume":0.25,"quality":"low","nope":1}"#).unwrap();
        assert_eq!(s.volume, 0.25);
        assert_eq!(s.quality, Quality::Low);
        assert!(s.normalize);
    }
}
