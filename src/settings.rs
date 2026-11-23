//! User preferences, persisted as JSON.

use serde::{Deserialize, Serialize};

/// How the lyrics page looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LyricsStyle {
    /// A tinted page, still lines; light on the GPU.
    Simple,
    /// The artwork blurred and drifting behind glass, a glowing line.
    Vivid,
}

impl LyricsStyle {
    pub const ALL: [LyricsStyle; 2] = [LyricsStyle::Simple, LyricsStyle::Vivid];

    pub fn label(self) -> &'static str {
        match self {
            LyricsStyle::Simple => "Simple",
            LyricsStyle::Vivid => "Vivid",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: f32,
    pub autoplay_on_start: bool,
    pub sidebar_collapsed: bool,
    pub show_now_playing: bool,
    pub compact_library: bool,
    pub reduce_transparency: bool,
    pub lyrics_style: LyricsStyle,
    /// Show lyrics in other scripts in Latin letters, where a reading exists.
    pub lyrics_romanized: bool,
    pub media_keys: bool,
    /// Look up lyrics on LRCLIB and NetEase when Spotify has none. Sends title and artist there.
    pub lrclib_lyrics: bool,
    /// Release date of the newest What's New item the user has seen.
    pub whats_new_seen: String,
    /// Replace the Soloist build before it expires.
    pub soloist_auto_update: bool,
    /// Draw the window on the GPU. Off draws it on the CPU (Linux only).
    pub hardware_acceleration: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 0.7,
            autoplay_on_start: false,
            sidebar_collapsed: false,
            show_now_playing: true,
            compact_library: false,
            reduce_transparency: false,
            lyrics_style: LyricsStyle::Vivid,
            lyrics_romanized: false,
            media_keys: true,
            lrclib_lyrics: false,
            whats_new_seen: String::new(),
            soloist_auto_update: true,
            hardware_acceleration: true,
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
            serde_json::from_str(r#"{"volume":0.25,"lyrics_style":"simple","nope":1}"#).unwrap();
        assert_eq!(s.volume, 0.25);
        assert_eq!(s.lyrics_style, LyricsStyle::Simple);
        assert!(s.media_keys);
    }
}
