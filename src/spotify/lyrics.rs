//! Lyrics lookup.
//!
//! Spotify's own time-synced lyrics are tried first. When they are missing,
//! and the user enabled community lyrics, LRCLIB (an open lyrics database)
//! is asked, by exact match and then by search, and NetEase Cloud Music
//! after it. Synced lyrics beat plain ones from any source. Results stay in
//! memory for the session and on disk for months.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::api::Auth;
use super::{enc, http};
use crate::model::{LyricLine, Lyrics, Track};

const SPCLIENT: &str = "https://spclient.wg.spotify.com";

/// Lyrics of the tracks seen lately, by track id (misses too, as `None`).
static CACHE: Mutex<Option<(HashMap<String, Option<Lyrics>>, VecDeque<String>)>> = Mutex::new(None);
const CACHE_TRACKS: usize = 64;

fn cached(id: &str) -> Option<Option<Lyrics>> {
    let cache = CACHE.lock().ok()?;
    cache.as_ref()?.0.get(id).cloned()
}

fn remember(id: &str, lyrics: Option<Lyrics>) {
    let Ok(mut cache) = CACHE.lock() else {
        return;
    };
    let (map, order) = cache.get_or_insert_with(Default::default);
    if map.insert(id.to_string(), lyrics).is_none() {
        order.push_back(id.to_string());
        if order.len() > CACHE_TRACKS
            && let Some(old) = order.pop_front()
        {
            map.remove(&old);
        }
    }
}

/// `third_party` allows the community lookups, which send the song's title
/// and artist to lrclib.net and music.163.com. It is opt-in.
pub async fn fetch(auth: &Auth, track: &Track, third_party: bool) -> Result<Lyrics> {
    let key = format!("{}:{third_party}", track.id);
    if let Some(found) = cached(&key) {
        return found.ok_or_else(|| anyhow!("no lyrics"));
    }
    let disk_key = format!(
        "{}-{}",
        track.id,
        if third_party { "all" } else { "spotify" }
    );
    let lookup_key = disk_key.clone();
    if let Ok(Some(kept)) = tokio::task::spawn_blocking(move || from_disk(&lookup_key)).await {
        remember(&key, kept.clone());
        return kept.ok_or_else(|| anyhow!("no lyrics"));
    }
    let found = lookup(auth, track, third_party).await;
    let keep = match &found {
        Ok(lyrics) => Some(Some(lyrics.clone())),
        // Only a clean "nothing there" is kept; a network error is worth
        // another try.
        Err(err) if err.to_string() == "no lyrics" => Some(None),
        Err(_) => None,
    };
    if let Some(keep) = keep {
        tokio::task::spawn_blocking(move || to_disk(&disk_key, keep.as_ref()));
    }
    match &found {
        Ok(lyrics) => remember(&key, Some(lyrics.clone())),
        Err(err) if err.to_string() == "no lyrics" => remember(&key, None),
        Err(_) => {}
    }
    found
}

/// Lyrics rarely change once published; "none found" may, so it is asked
/// again sooner.
const MISS_KEPT: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// `Some(None)`: kept as "no lyrics".
fn from_disk(key: &str) -> Option<Option<Lyrics>> {
    let hit = crate::disk_cache::LYRICS.get(key)?;
    let value = hit.value;
    if value["none"].as_bool() == Some(true) {
        return (hit.age < MISS_KEPT).then_some(None);
    }
    let lines: Vec<LyricLine> = value["lines"]
        .as_array()?
        .iter()
        .filter_map(|line| {
            Some(LyricLine {
                start_ms: line.get(0)?.as_u64()? as u32,
                text: line.get(1)?.as_str()?.to_string().into(),
            })
        })
        .collect();
    Some(Some(Lyrics {
        lines: lines.into(),
        synced: value["synced"].as_bool().unwrap_or(false),
        provider: value["provider"]
            .as_str()
            .unwrap_or_default()
            .to_string()
            .into(),
    }))
}

fn to_disk(key: &str, lyrics: Option<&Lyrics>) {
    let value = match lyrics {
        None => serde_json::json!({ "none": true }),
        Some(lyrics) => serde_json::json!({
            "lines": lyrics.lines.iter().map(|l| serde_json::json!([l.start_ms, l.text.as_ref()])).collect::<Vec<_>>(),
            "synced": lyrics.synced,
            "provider": lyrics.provider.as_ref(),
        }),
    };
    crate::disk_cache::LYRICS.put(key, &value);
}

async fn lookup(auth: &Auth, track: &Track, third_party: bool) -> Result<Lyrics> {
    let spotify = from_spotify(auth, track)
        .await
        .ok()
        .filter(|l| !l.lines.is_empty());
    if spotify.as_ref().is_some_and(|l| l.synced) || !third_party {
        return spotify.ok_or_else(|| anyhow!("no lyrics"));
    }
    let mut plain = spotify;
    for source in [Source::LrclibGet, Source::LrclibSearch, Source::NetEase] {
        let found = match source {
            Source::LrclibGet => from_lrclib(track).await,
            Source::LrclibSearch => from_lrclib_search(track).await,
            Source::NetEase => from_netease(track).await,
        };
        match found {
            Ok(lyrics) if lyrics.synced => return Ok(lyrics),
            Ok(lyrics) if plain.is_none() && !lyrics.lines.is_empty() => plain = Some(lyrics),
            _ => {}
        }
    }
    plain.ok_or_else(|| anyhow!("no lyrics"))
}

#[derive(Clone, Copy)]
enum Source {
    LrclibGet,
    LrclibSearch,
    NetEase,
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
    from_lrclib_record(found)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibHit {
    #[serde(default)]
    duration: f32,
    #[serde(flatten)]
    record: LrcLib,
}

/// LRCLIB's fuzzy search, for when the exact lookup misses (a different
/// album name, "Remastered" in the title): the closest duration wins.
async fn from_lrclib_search(track: &Track) -> Result<Lyrics> {
    let artist = track.artists.first().map(|a| a.name.as_ref()).unwrap_or("");
    let url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        enc(&clean_title(&track.name)),
        enc(artist),
    );
    let hits: Vec<LrcLibHit> = http()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let seconds = track.duration_ms as f32 / 1000.0;
    let best = hits
        .into_iter()
        .filter(|h| (h.duration - seconds).abs() <= 3.0)
        .min_by_key(|h| {
            // Synced first, then the nearest duration.
            let synced = h
                .record
                .synced_lyrics
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty());
            (!synced, ((h.duration - seconds).abs() * 10.0) as u32)
        })
        .ok_or_else(|| anyhow!("no lyrics"))?;
    from_lrclib_record(best.record)
}

fn from_lrclib_record(found: LrcLib) -> Result<Lyrics> {
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
    Ok(plain_lyrics(&plain, "LRCLIB"))
}

fn plain_lyrics(text: &str, provider: &str) -> Lyrics {
    let lines: Vec<LyricLine> = text
        .lines()
        .map(|l| LyricLine {
            start_ms: 0,
            text: l.trim().to_string().into(),
        })
        .collect();
    Lyrics {
        lines: lines.into(),
        synced: false,
        provider: provider.to_string().into(),
    }
}

/// "Song - 2011 Remaster" and "Song (feat. X)" are filed as "Song".
fn clean_title(title: &str) -> String {
    let mut title = title;
    for cut in [" - ", " (feat", " (with", " [feat"] {
        if let Some(ix) = title.find(cut) {
            title = &title[..ix];
        }
    }
    title.trim().to_string()
}

#[derive(Deserialize)]
struct NeSearch {
    #[serde(default)]
    result: Option<NeResult>,
}

#[derive(Deserialize)]
struct NeResult {
    #[serde(default)]
    songs: Vec<NeSong>,
}

#[derive(Deserialize)]
struct NeSong {
    id: u64,
    #[serde(default)]
    duration: u32,
    #[serde(default)]
    artists: Vec<NeArtist>,
}

#[derive(Deserialize)]
struct NeArtist {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct NeLyric {
    #[serde(default)]
    lrc: Option<NeLrc>,
}

#[derive(Deserialize)]
struct NeLrc {
    #[serde(default)]
    lyric: String,
}

/// NetEase Cloud Music, which has synced lyrics for much that LRCLIB lacks
/// (Asian music especially).
async fn from_netease(track: &Track) -> Result<Lyrics> {
    let artist = track
        .artists
        .first()
        .map(|a| a.name.to_string())
        .unwrap_or_default();
    let query = format!("{} {artist}", clean_title(&track.name));
    let url = format!(
        "https://music.163.com/api/search/get?s={}&type=1&limit=10",
        enc(&query)
    );
    let search: NeSearch = http()
        .get(url)
        .header("Referer", "https://music.163.com/")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let songs = search.result.map(|r| r.songs).unwrap_or_default();
    let wanted = artist.to_lowercase();
    let song = songs
        .into_iter()
        .filter(|s| s.duration.abs_diff(track.duration_ms) <= 3000)
        .find(|s| {
            s.artists
                .iter()
                .any(|a| !wanted.is_empty() && a.name.to_lowercase() == wanted)
        })
        .ok_or_else(|| anyhow!("no lyrics"))?;
    let url = format!(
        "https://music.163.com/api/song/lyric?id={}&lv=1&kv=1&tv=-1&rv=-1",
        song.id
    );
    let lyric: NeLyric = http()
        .get(url)
        .header("Referer", "https://music.163.com/")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let text = lyric.lrc.map(|l| l.lyric).unwrap_or_default();
    // NetEase puts credits ("作词 : …") on the first timed lines.
    let lines: Vec<LyricLine> = parse_lrc(&text)
        .into_iter()
        .filter(|l| !(l.text.contains(" : ") || l.text.contains("：")) || l.start_ms > 20_000)
        .collect();
    if lines.iter().filter(|l| !l.text.is_empty()).count() < 2 {
        return Err(anyhow!("no lyrics"));
    }
    Ok(Lyrics {
        lines: lines.into(),
        synced: true,
        provider: "NetEase Cloud Music".into(),
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
    fn titles_lose_versions() {
        assert_eq!(clean_title("Song - 2011 Remaster"), "Song");
        assert_eq!(clean_title("Song (feat. X)"), "Song");
        assert_eq!(clean_title("Plain"), "Plain");
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
