//! Everything that talks to Spotify.

pub mod api;
pub mod buddylist;
pub mod demo;
pub mod lyrics;
pub mod oauth;

use std::sync::OnceLock;
use std::time::Duration;

/// Shared HTTP client. One connection pool for the whole process.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&accept_language()) {
            headers.insert(reqwest::header::ACCEPT_LANGUAGE, value);
        }
        reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(concat!("lilac/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .build()
            .expect("failed to build HTTP client")
    })
}

/// Percent-encode a query component.
pub fn enc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(
                    char::from_digit((b >> 4) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
                out.push(
                    char::from_digit((b & 0xf) as u32, 16)
                        .unwrap()
                        .to_ascii_uppercase(),
                );
            }
        }
    }
    out
}

/// Decode a percent-encoded query component. Invalid escapes pass through untouched.
pub fn dec(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = "artist:\"Sigur Rós\" & more/1+1";
        assert_eq!(dec(&enc(s)), s);
        assert_eq!(enc("a b"), "a%20b");
        assert_eq!(dec("100%"), "100%");
        assert_eq!(dec("%zz"), "%zz");
        assert_eq!(dec("a%2"), "a%2");
    }
}

/// The user's language for Spotify, which names artists and releases in
/// it where it has a translation ("Yorushika" rather than "ヨルシカ" for
/// English), as the official apps get. From the POSIX locale; English
/// otherwise.
pub fn accept_language() -> String {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .unwrap_or_default();
    language_tag(&locale)
}

fn language_tag(locale: &str) -> String {
    // "en_US.UTF-8@euro" → "en-US".
    let base = locale.split(['.', '@']).next().unwrap_or_default();
    let mut parts = base.split(['_', '-']);
    let language = parts.next().unwrap_or_default().to_ascii_lowercase();
    if language.len() < 2 || !language.chars().all(|c| c.is_ascii_alphabetic()) {
        return "en".to_string();
    }
    match parts
        .next()
        .filter(|r| r.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        Some(region) if !region.is_empty() => {
            format!(
                "{language}-{}, {language};q=0.9",
                region.to_ascii_uppercase()
            )
        }
        _ => language,
    }
}

#[cfg(test)]
mod language_tests {
    use super::language_tag;

    #[test]
    fn locales_become_language_tags() {
        assert_eq!(language_tag("en_US.UTF-8"), "en-US, en;q=0.9");
        assert_eq!(language_tag("ja_JP"), "ja-JP, ja;q=0.9");
        assert_eq!(language_tag("de"), "de");
        assert_eq!(language_tag(""), "en");
        assert_eq!(language_tag("C.UTF-8"), "en");
    }
}
