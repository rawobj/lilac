//! Embedded SVG icons.
//!
//! Icons are compiled into the binary, so there is no filesystem access at
//! runtime. GPUI rasterizes each one once and caches the result, which keeps a
//! linear scan over this small table off the hot path.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((
                concat!("icons/", $name, ".svg"),
                include_bytes!(concat!("../assets/icons/", $name, ".svg")),
            ),)*
        ];
    };
}

icons![
    "arrow-down-up",
    "audio-lines",
    "bell",
    "check",
    "chevron-left",
    "chevron-right",
    "circle-alert",
    "circle-check-fill",
    "circle-plus",
    "clock",
    "disc-3",
    "ellipsis",
    "heart",
    "heart-fill",
    "house",
    "info",
    "keyboard",
    "laptop",
    "library",
    "lilac",
    "link",
    "list-end",
    "list-music",
    "list-plus",
    "loader-circle",
    "log-out",
    "maximize-2",
    "mic-vocal",
    "minimize-2",
    "minus",
    "monitor-speaker",
    "music",
    "panel-right",
    "pause-fill",
    "pencil",
    "play-fill",
    "plus",
    "podcast",
    "refresh-cw",
    "repeat",
    "repeat-1",
    "search",
    "settings",
    "shuffle",
    "skip-back-fill",
    "skip-forward-fill",
    "square",
    "trash",
    "user",
    "user-plus",
    "users",
    "volume",
    "volume-1",
    "volume-2",
    "volume-x",
    "x",
];

/// Asset source for the application. Falls back to the component library's
/// bundled icons so its built-in widgets keep working.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, data)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(data)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect();
        out.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_no_duplicates() {
        let mut names: Vec<_> = ICONS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ICONS.len());
    }

    #[test]
    fn lookup_hits_embedded_icon() {
        let data = Assets.load("icons/play-fill.svg").unwrap().unwrap();
        assert!(data.starts_with(b"<svg"));
    }
}
