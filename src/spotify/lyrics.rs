//! Lyrics lookup.
//!
//! Spotify's own time-synced lyrics are tried first. When they are missing,
//! LRCLIB, a community database, can fill in if the user enabled it.

use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::api::Auth;
use super::{enc, http};
use crate::model::{LyricLine, Lyrics, Track};

const SPCLIENT: &str = "https://spclient.wg.spotify.com";

/// `third_party` allows the LRCLIB lookup, which sends the song's title and
/// artist to lrclib.net. It is opt-in.
pub async fn fetch(auth: &Auth, track: &Track, third_party: bool) -> Result<Lyrics> {
    match from_spotify(auth, track).await {
        Ok(lyrics) if !lyrics.lines.is_empty() => Ok(lyrics),
        Ok(_) | Err(_) if third_party => from_lrclib(track).await,
        Ok(lyrics) => Ok(lyrics),
        Err(err) => Err(err),
    }
}

#[derive(Deserialize)]
struct ColorLyrics {
    lyrics: SpotifyLyrics,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyLyrics {
    #[serde(default)]
    sync_type: String,
    #[serde(default)]
    lines: Vec<SpotifyLine>,
    #[serde(default)]
    provider_display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyLine {
    #[serde(default)]
    start_time_ms: String,
    #[serde(default)]
    words: String,
}

async fn from_spotify(auth: &Auth, track: &Track) -> Result<Lyrics> {
    let token = auth.token().await?;
    let url = format!(
        "{SPCLIENT}/color-lyrics/v2/track/{}?format=json&vocalRemoval=false",
        track.id
    );
    let response = http()
        .get(url)
        .bearer_auth(token)
        .header("app-platform", "WebPlayer")
        .header("Accept", "application/json")
        .send()
        .await?
        .error_for_status()?;
    let parsed: ColorLyrics = response.json().await?;
    let synced = parsed.lyrics.sync_type == "LINE_SYNCED";
    let lines: Vec<LyricLine> = parsed
        .lyrics
        .lines
        .into_iter()
        .map(|l| LyricLine {
            start_ms: l.start_time_ms.parse().unwrap_or(0),
            text: l.words.into(),
        })
        .collect();
    let provider = if parsed.lyrics.provider_display_name.is_empty() {
        "Spotify".to_string()
    } else {
        parsed.lyrics.provider_display_name
    };
    Ok(Lyrics {
        lines: lines.into(),
        synced,
        provider: provider.into(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLib {
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    instrumental: bool,
}

async fn from_lrclib(track: &Track) -> Result<Lyrics> {
    let artist = track.artists.first().map(|a| a.name.as_ref()).unwrap_or("");
    let album = track.album.as_ref().map(|a| a.name.as_ref()).unwrap_or("");
    let url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
        enc(&track.name),
        enc(artist),
        enc(album),
        track.duration_ms / 1000
    );
    let found: LrcLib = http()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if found.instrumental {
        return Ok(Lyrics {
            lines: vec![LyricLine {
                start_ms: 0,
                text: "♪ Instrumental ♪".into(),
            }]
            .into(),
            synced: false,
            provider: "LRCLIB".into(),
        });
    }
    if let Some(synced) = found.synced_lyrics.filter(|s| !s.trim().is_empty()) {
        let lines = parse_lrc(&synced);
        if !lines.is_empty() {
            return Ok(Lyrics {
                lines: lines.into(),
                synced: true,
                provider: "LRCLIB".into(),
            });
        }
    }
    let plain = found
        .plain_lyrics
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("no lyrics"))?;
    let lines: Vec<LyricLine> = plain
        .lines()
        .map(|l| LyricLine {
            start_ms: 0,
            text: l.trim().to_string().into(),
        })
        .collect();
    Ok(Lyrics {
        lines: lines.into(),
        synced: false,
        provider: "LRCLIB".into(),
    })
}

/// Parse LRC text: one `[mm:ss.xx] words` entry per line.
pub fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let Some(rest) = raw.trim().strip_prefix('[') else {
            continue;
        };
        let Some((stamp, words)) = rest.split_once(']') else {
            continue;
        };
        let Some((minutes, seconds)) = stamp.split_once(':') else {
            continue;
        };
        let (Ok(minutes), Ok(seconds)) = (minutes.parse::<u32>(), seconds.parse::<f32>()) else {
            // Metadata tags such as [ar:Artist].
            continue;
        };
        let start_ms = minutes * 60_000 + (seconds * 1000.0).round() as u32;
        lines.push(LyricLine {
            start_ms,
            text: words.trim().to_string().into(),
        });
    }
    lines.sort_by_key(|l| l.start_ms);
    lines
}

/// Index of the line being sung at `position_ms`.
pub fn active_line(lines: &[LyricLine], position_ms: u32) -> Option<usize> {
    match lines.partition_point(|l| l.start_ms <= position_ms) {
        0 => None,
        n => Some(n - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parsing() {
        let lines =
            parse_lrc("[ar:Someone]\n[00:12.50] Hello\n[01:02.03]World\n\nnot a line\n[00:01.00]");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].start_ms, 1000);
        assert_eq!(
            lines[1],
            LyricLine {
                start_ms: 12_500,
                text: "Hello".into()
            }
        );
        assert_eq!(lines[2].start_ms, 62_030);
    }

    #[test]
    fn active_line_lookup() {
        let lines = parse_lrc("[00:01.00]a\n[00:05.00]b\n[00:09.00]c");
        assert_eq!(active_line(&lines, 0), None);
        assert_eq!(active_line(&lines, 1000), Some(0));
        assert_eq!(active_line(&lines, 8999), Some(1));
        assert_eq!(active_line(&lines, 60_000), Some(2));
    }
}
