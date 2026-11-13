//! Locations of on-disk state.

use std::path::PathBuf;

const APP_DIR: &str = "lilac";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                home().join("Library/Application Support")
            } else {
                home().join(".config")
            }
        });
    base.join(APP_DIR)
}

/// Data that can be fetched again: `$XDG_CACHE_HOME/lilac` or the platform's
/// equivalent.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                home().join("Library/Caches")
            } else {
                home().join(".cache")
            }
        });
    base.join(APP_DIR)
}

/// Data Lilac keeps but did not get from the user: `$XDG_DATA_HOME/lilac`
/// or the platform's equivalent.
#[cfg(windows)]
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                home().join("Library/Application Support")
            } else {
                home().join(".local/share")
            }
        });
    base.join(APP_DIR)
}

pub fn credentials_file() -> PathBuf {
    config_dir().join("credentials.json")
}

/// Tokens of the desktop client, which the streaming session needs.
pub fn streaming_credentials_file() -> PathBuf {
    config_dir().join("streaming-credentials.json")
}

pub fn settings_file() -> PathBuf {
    config_dir().join("settings.json")
}

/// A random id for this installation, kept across runs so Spotify sees one
/// device rather than a new one on every start.
pub fn device_id() -> String {
    let path = config_dir().join("device_id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return id.to_string();
        }
    }
    let mut bytes = [0u8; 20];
    let _ = getrandom::fill(&mut bytes);
    let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let _ = write_private(&path, id.as_bytes());
    id
}

/// Write `contents` atomically, readable by the current user only.
pub fn write_private(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}
