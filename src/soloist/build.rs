//! Facts about a Soloist build: where Spotify publishes it and how long it
//! may run.

use serde::{Deserialize, Serialize};

/// Spotify publishes the latest build of each platform at a fixed address.
const CDN: &str = "https://soloist-builds.spotifycdn.com";

/// Where users are sent to read about Soloist and its terms.
pub const DOWNLOADS_PAGE: &str =
    "https://developer.spotify.com/documentation/soloist/reference/downloads-and-updates";
pub const KEY_PAGE: &str = "https://developer.spotify.com/dashboard/soloist";

/// Builds stop working this long after they were built.
pub const LIFETIME_SECS: u64 = 90 * DAY;
pub const DAY: u64 = 24 * 60 * 60;

/// The archive for this machine, if Spotify builds one for it.
pub fn archive_url() -> Option<String> {
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "soloist_release_x86_64.tar.gz",
        ("linux", "aarch64") => "soloist_release_arm64.tar.gz",
        ("linux", "arm") => "soloist_release_arm32.tar.gz",
        ("macos", "aarch64") => "soloist_release_osx_arm64.tar.gz",
        ("windows", "x86_64") => "soloist_release_windows_x86_64.zip",
        _ => return None,
    };
    Some(format!("{CDN}/{name}"))
}

/// An installed build, as `soloist --version` describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub version: String,
    /// Unix seconds; the build stops working `LIFETIME_SECS` after this.
    pub built_at: u64,
    /// The CDN's validators for the archive it came from, to tell whether
    /// a newer one is out without downloading it.
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub installed_at: u64,
}

impl Build {
    pub fn expires_at(&self) -> u64 {
        self.built_at + LIFETIME_SECS
    }

    /// Whole days left before it expires (0 on its last day and after).
    pub fn days_left(&self, now: u64) -> u64 {
        self.expires_at().saturating_sub(now) / DAY
    }

    pub fn expired(&self, now: u64) -> bool {
        now >= self.expires_at()
    }

    pub fn age_days(&self, now: u64) -> u64 {
        now.saturating_sub(self.built_at) / DAY
    }
}

/// Parse Soloist's version line (`soloist <version> build <unix time> …`)
/// into the version and the build time.
pub fn parse_version(line: &str) -> Option<(String, u64)> {
    let mut words = line.split_whitespace();
    if words.next()? != "soloist" {
        return None;
    }
    let version = words.next()?.to_string();
    if words.next()? != "build" {
        return None;
    }
    let built_at = words.next()?.parse().ok()?;
    Some((version, built_at))
}

/// When to look for a newer build again: daily, and every few hours once
/// the installed one is close to expiring.
pub fn next_check(build: Option<&Build>, last_check: u64, now: u64) -> u64 {
    let interval = match build {
        Some(build) if build.days_left(now) <= 7 => 4 * 60 * 60,
        _ => DAY,
    };
    last_check + interval
}

/// Whether to replace `build` with a newer one that is available. Builds
/// come out often; replacing each one would download 14 MB a day for
/// nothing, so a build is kept until it is two weeks old or two weeks from
/// expiring.
pub fn should_update(build: &Build, now: u64) -> bool {
    build.age_days(now) >= 14 || build.days_left(now) <= 14
}

/// How loudly to remind the user about the build's expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Reminder {
    None,
    /// Two weeks left: a note.
    Soon,
    /// Three days left: a warning and a desktop notification.
    Urgent,
    /// It no longer starts.
    Expired,
}

pub fn reminder(build: &Build, now: u64) -> Reminder {
    if build.expired(now) {
        Reminder::Expired
    } else if build.days_left(now) < 3 {
        Reminder::Urgent
    } else if build.days_left(now) < 14 {
        Reminder::Soon
    } else {
        Reminder::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(built_at: u64) -> Build {
        Build {
            version: "1.2.0.40".into(),
            built_at,
            etag: None,
            last_modified: None,
            installed_at: built_at,
        }
    }

    #[test]
    fn parses_the_version_line() {
        let line = "soloist 1.2.0.40 build 1792800000 (linux/x86_64)\n";
        assert_eq!(
            parse_version(line),
            Some(("1.2.0.40".to_string(), 1792800000))
        );
        assert_eq!(parse_version("soloist 1.2.0"), None);
        assert_eq!(parse_version("something else"), None);
    }

    #[test]
    fn expiry_is_ninety_days_after_the_build() {
        let b = build(1_000_000);
        assert_eq!(b.expires_at(), 1_000_000 + 90 * DAY);
        assert_eq!(b.days_left(1_000_000), 90);
        assert_eq!(b.days_left(b.expires_at() - 1), 0);
        assert!(!b.expired(b.expires_at() - 1));
        assert!(b.expired(b.expires_at()));
    }

    #[test]
    fn reminders_escalate() {
        let b = build(0);
        assert_eq!(reminder(&b, 0), Reminder::None);
        assert_eq!(reminder(&b, 77 * DAY), Reminder::Soon);
        assert_eq!(reminder(&b, 88 * DAY), Reminder::Urgent);
        assert_eq!(reminder(&b, 90 * DAY), Reminder::Expired);
    }

    #[test]
    fn updates_wait_for_an_old_build() {
        let b = build(0);
        assert!(!should_update(&b, 3 * DAY));
        assert!(should_update(&b, 14 * DAY));
        assert!(should_update(&b, 80 * DAY));
    }

    #[test]
    fn checks_more_often_near_expiry() {
        let b = build(0);
        assert_eq!(next_check(Some(&b), 10, 20), 10 + DAY);
        assert_eq!(
            next_check(Some(&b), 85 * DAY, 85 * DAY),
            85 * DAY + 4 * 60 * 60
        );
        assert_eq!(next_check(None, 5, 5), 5 + DAY);
    }
}
