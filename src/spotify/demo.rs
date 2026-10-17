//! Offline demo backend.
//!
//! Answers the same paths as the Web API with generated JSON, so every view
//! and every parser runs exactly as it does against the real service. Nothing
//! here touches the network and none of the names refer to real artists.

use serde_json::{Value, json};

const ARTISTS: &[(&str, &[&str])] = &[
    ("Violet Hours", &["dream pop", "shoegaze"]),
    ("Marrow & Pine", &["indie folk"]),
    ("Kessler Array", &["ambient techno", "idm"]),
    ("The Lantern Club", &["indie rock"]),
    ("Odalys Reyes", &["neo soul", "r&b"]),
    ("Glass Harbor", &["synthwave"]),
    ("Juniper Static", &["lo-fi", "chillhop"]),
    ("Nightjar Assembly", &["modern jazz"]),
];

const ALBUM_WORDS: &[&str] = &[
    "Afterglow",
    "Paper Moons",
    "Low Tide",
    "Second Sun",
    "Soft Machines",
    "Northern Rooms",
    "Half Light",
    "Wild Lilac",
    "Static Bloom",
    "Slow Orbit",
    "Ultraviolet",
    "Quiet Engines",
    "Amber Signals",
    "Hollow Coast",
    "Night Swim",
    "Velvet Circuit",
    "Open Water",
    "Far Rooms",
    "Tessellate",
    "Small Hours",
    "Kindling",
    "Parallel Lines",
    "Evening Standard",
    "Aurora Lane",
];

const TRACK_WORDS: &[&str] = &[
    "Lavender",
    "Undertow",
    "Glasshouse",
    "Sundial",
    "Paper Planes",
    "Driftwood",
    "Neon Rain",
    "Cathedral",
    "Slow Motion",
    "Honeycomb",
    "Satellite",
    "Kerosene",
    "Blue Hour",
    "Weightless",
    "Telegraph",
    "Marigold",
    "Fever Dream",
    "Silhouette",
    "Overgrown",
    "Polaroid",
    "Echo Park",
    "Moth",
    "Tidal",
    "Halcyon",
    "Ember",
    "Vertigo",
    "Northbound",
    "Daylight Saving",
    "Atlas",
    "Wildfire",
    "Porcelain",
    "Sleepwalker",
];

const PLAYLISTS: &[(&str, &str)] = &[
    (
        "Late Night Coding",
        "Low-key electronics for long sessions.",
    ),
    ("Sunday Slow", "Coffee, rain and nowhere to be."),
    ("Purple Skies", "Dream pop and shimmer."),
    ("Deep Focus", "No words, no distractions."),
    ("Road Trip 2026", "Windows down."),
    ("Jazz After Dark", "Small rooms and brushed drums."),
    ("Run Faster", "170 bpm and up."),
    ("Discoveries", "Things worth a second listen."),
];

const TRACKS_PER_ALBUM: usize = 9;
const ALBUMS_PER_ARTIST: usize = 3;

fn artist_ref(a: usize) -> Value {
    json!({ "id": format!("ar{a}"), "name": ARTISTS[a % ARTISTS.len()].0, "type": "artist" })
}

fn artist(a: usize) -> Value {
    let (name, genres) = ARTISTS[a % ARTISTS.len()];
    json!({
        "id": format!("ar{a}"),
        "uri": format!("spotify:artist:ar{a}"),
        "name": name,
        "genres": genres,
        "images": [],
        "followers": { "total": 48_200 + a * 91_337 },
    })
}

/// Albums are numbered globally: album `n` belongs to artist `n / ALBUMS_PER_ARTIST`.
fn album_count() -> usize {
    ARTISTS.len() * ALBUMS_PER_ARTIST
}

fn album_ref(n: usize) -> Value {
    json!({ "id": format!("al{n}"), "name": ALBUM_WORDS[n % ALBUM_WORDS.len()], "images": [] })
}

fn album(n: usize, with_tracks: bool) -> Value {
    let a = n / ALBUMS_PER_ARTIST;
    let kind = if n % ALBUMS_PER_ARTIST == 2 {
        "single"
    } else {
        "album"
    };
    let total = if kind == "single" {
        3
    } else {
        TRACKS_PER_ALBUM
    };
    let mut value = json!({
        "id": format!("al{n}"),
        "uri": format!("spotify:album:al{n}"),
        "name": ALBUM_WORDS[n % ALBUM_WORDS.len()],
        "album_type": kind,
        "artists": [artist_ref(a)],
        "images": [],
        "release_date": format!("{}-0{}-1{}", 2019 + n % 8, 1 + n % 9, n % 9),
        "total_tracks": total,
    });
    if with_tracks {
        let items: Vec<Value> = (0..total).map(|t| track(n, t)).collect();
        value["tracks"] = json!({ "items": items, "total": total, "next": null });
    }
    value
}

fn track(n: usize, t: usize) -> Value {
    let a = n / ALBUMS_PER_ARTIST;
    let word = TRACK_WORDS[(n * 7 + t * 3) % TRACK_WORDS.len()];
    let mut artists = vec![artist_ref(a)];
    if (n + t) % 7 == 3 {
        artists.push(artist_ref((a + 3) % ARTISTS.len()));
    }
    json!({
        "id": format!("tr{n}x{t}"),
        "uri": format!("spotify:track:tr{n}x{t}"),
        "name": word,
        "type": "track",
        "artists": artists,
        "album": album_ref(n),
        "duration_ms": 152_000 + ((n * 37 + t * 53) % 140) * 1000,
        "explicit": (n + t) % 9 == 0,
        "track_number": t + 1,
        "disc_number": 1,
        "is_playable": true,
    })
}

/// A deterministic pseudo-random pick of tracks across the catalog.
fn mixed_tracks(seed: usize, count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let n = (seed * 5 + i * 7) % album_count();
            let total = if n % ALBUMS_PER_ARTIST == 2 {
                3
            } else {
                TRACKS_PER_ALBUM
            };
            track(n, (seed + i * 2) % total)
        })
        .collect()
}

fn playlist(p: usize) -> Value {
    let (name, description) = PLAYLISTS[p % PLAYLISTS.len()];
    let owned = p % 3 != 2;
    json!({
        "id": format!("pl{p}"),
        "uri": format!("spotify:playlist:pl{p}"),
        "name": name,
        "description": description,
        "images": [],
        "owner": if owned {
            json!({ "id": "demo", "display_name": "Demo Listener" })
        } else {
            json!({ "id": "lilac", "display_name": "lilac" })
        },
        "collaborative": false,
        "public": false,
        "snapshot_id": "demo",
        "items": { "total": playlist_len(p) },
    })
}

fn playlist_len(p: usize) -> usize {
    // One long playlist to show off list virtualization.
    if p == 3 { 2400 } else { 24 + p * 9 }
}

fn show(s: usize) -> Value {
    let names = ["Compile Time", "The Long Way Round", "Signal & Noise"];
    json!({
        "id": format!("sh{s}"),
        "uri": format!("spotify:show:sh{s}"),
        "name": names[s % names.len()],
        "publisher": "lilac demo",
        "description": "A demo podcast. The episodes are placeholders and carry no audio.",
        "images": [],
        "total_episodes": 12,
    })
}

fn episode(s: usize, e: usize) -> Value {
    json!({
        "id": format!("ep{s}x{e}"),
        "uri": format!("spotify:episode:ep{s}x{e}"),
        "name": format!("Episode {} — {}", 12 - e, TRACK_WORDS[(s * 5 + e) % TRACK_WORDS.len()]),
        "type": "episode",
        "description": "Placeholder episode description for the demo catalog.",
        "duration_ms": 1_800_000 + e * 137_000,
        "release_date": format!("2026-0{}-1{}", 9 - e % 9, e % 9),
        "images": [],
        "is_playable": true,
    })
}

fn page(items: Vec<Value>, total: usize, has_next: bool) -> Value {
    json!({ "items": items, "total": total, "next": if has_next { json!("demo") } else { Value::Null } })
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
}

fn query_usize(query: &str, key: &str) -> usize {
    query_param(query, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn index(id: &str, prefix: &str) -> usize {
    id.strip_prefix(prefix)
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Answer a GET. `None` means the demo backend has nothing at this path.
/// Sample friend activity: a few people, some listening right now.
pub fn friends() -> Vec<super::buddylist::Friend> {
    const PEOPLE: &[&str] = &["Maya", "Jonas", "Priya", "Theo", "Lena", "Sam"];
    // Minutes since each friend's last play.
    const AGO: &[u64] = &[0, 2, 14, 47, 180, 1500];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    PEOPLE
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let n = (i * 5 + 1) % album_count();
            let a = n / ALBUMS_PER_ARTIST;
            let t = i % TRACKS_PER_ALBUM;
            let (context, context_uri) = if i % 2 == 0 {
                let p = i % PLAYLISTS.len();
                (
                    PLAYLISTS[p].0.to_string(),
                    format!("spotify:playlist:pl{p}"),
                )
            } else {
                (
                    ALBUM_WORDS[n % ALBUM_WORDS.len()].to_string(),
                    format!("spotify:album:al{n}"),
                )
            };
            super::buddylist::Friend {
                name: name.to_string(),
                user_uri: format!("spotify:user:friend{i}"),
                image: None,
                track: TRACK_WORDS[(n * 7 + t * 3) % TRACK_WORDS.len()].to_string(),
                track_uri: format!("spotify:track:tr{n}x{t}"),
                artist: ARTISTS[a % ARTISTS.len()].0.to_string(),
                artist_uri: format!("spotify:artist:ar{a}"),
                context,
                context_uri,
                timestamp_ms: now.saturating_sub(AGO[i] * 60_000),
            }
        })
        .collect()
}

pub fn get(path: &str) -> Option<Value> {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let offset = query_usize(query, "offset");

    Some(match segments.as_slice() {
        ["me"] => json!({
            "id": "demo",
            "display_name": "Demo Listener",
            "product": "premium",
            "images": [],
        }),
        ["me", "playlists"] => {
            let items = (0..PLAYLISTS.len()).map(playlist).collect();
            page(items, PLAYLISTS.len(), false)
        }
        ["me", "tracks"] => {
            let total = 64;
            let items: Vec<Value> = mixed_tracks(11, total)
                .into_iter()
                .skip(offset)
                .take(50)
                .map(|t| json!({ "added_at": "2026-10-01T10:00:00Z", "track": t }))
                .collect();
            page(items, total, offset + 50 < total)
        }
        ["me", "albums"] => {
            let items = [1, 4, 6, 10, 13, 16, 21]
                .iter()
                .map(|n| json!({ "album": album(*n, false) }))
                .collect();
            page(items, 7, false)
        }
        ["me", "following"] => json!({
            "artists": { "items": [artist(0), artist(2), artist(4), artist(5), artist(7)], "next": null }
        }),
        ["me", "shows"] => page(
            (0..3).map(|s| json!({ "show": show(s) })).collect(),
            3,
            false,
        ),
        ["me", "player", "recently-played"] => page(
            mixed_tracks(3, 30)
                .into_iter()
                .map(|t| json!({ "track": t }))
                .collect(),
            30,
            false,
        ),
        ["me", "top", "tracks"] => page(mixed_tracks(17, 10), 10, false),
        ["me", "top", "artists"] => page(
            (0..ARTISTS.len()).map(artist).collect(),
            ARTISTS.len(),
            false,
        ),
        ["me", "player", "devices"] => json!({ "devices": [] }),
        ["me", "player", "queue"] => json!({ "currently_playing": null, "queue": [] }),

        ["albums", id] => album(index(id, "al"), true),
        ["albums", id, "tracks"] => {
            let n = index(id, "al");
            let total = if n % ALBUMS_PER_ARTIST == 2 {
                3
            } else {
                TRACKS_PER_ALBUM
            };
            page((0..total).map(|t| track(n, t)).collect(), total, false)
        }
        ["artists", id] => artist(index(id, "ar")),
        ["artists", id, "albums"] => {
            let a = index(id, "ar");
            let items = (0..ALBUMS_PER_ARTIST)
                .map(|i| album(a * ALBUMS_PER_ARTIST + i, false))
                .collect();
            page(items, ALBUMS_PER_ARTIST, false)
        }
        ["artists", id, "top-tracks"] => {
            let a = index(id, "ar");
            let tracks: Vec<Value> = (0..10)
                .map(|i| {
                    track(
                        a * ALBUMS_PER_ARTIST + i % 2,
                        (i * 2 + a) % TRACKS_PER_ALBUM,
                    )
                })
                .collect();
            json!({ "tracks": tracks })
        }
        ["artists", id, "related-artists"] => {
            let a = index(id, "ar");
            let artists: Vec<Value> = (1..6).map(|i| artist((a + i) % ARTISTS.len())).collect();
            json!({ "artists": artists })
        }
        ["playlists", id] => playlist(index(id, "pl")),
        ["playlists", id, "items" | "tracks"] => {
            let p = index(id, "pl");
            let total = playlist_len(p);
            let items: Vec<Value> = (offset..(offset + 100).min(total))
                .map(|i| {
                    let n = (p * 3 + i * 5) % album_count();
                    let tracks = if n % ALBUMS_PER_ARTIST == 2 { 3 } else { TRACKS_PER_ALBUM };
                    json!({ "added_at": "2026-10-01T10:00:00Z", "item": track(n, (i + p) % tracks) })
                })
                .collect();
            page(items, total, offset + 100 < total)
        }
        ["shows", id] => show(index(id, "sh")),
        ["shows", id, "episodes"] => {
            let s = index(id, "sh");
            page(
                (offset..12.min(offset + 50))
                    .map(|e| episode(s, e))
                    .collect(),
                12,
                false,
            )
        }
        ["browse", "new-releases"] => {
            let items: Vec<Value> = (0..12)
                .map(|i| album((i * 5 + 2) % album_count(), false))
                .collect();
            json!({ "albums": page(items, 12, false) })
        }
        ["search"] => search(&super::dec(query_param(query, "q").unwrap_or(""))),
        _ => return None,
    })
}

fn search(query: &str) -> Value {
    let needle = query.to_lowercase();
    let needle = needle.trim_start_matches("artist:").trim_matches('"');
    let hit = |name: &str| name.to_lowercase().contains(needle);

    let artists: Vec<Value> = (0..ARTISTS.len())
        .filter(|a| hit(ARTISTS[*a].0) || ARTISTS[*a].1.iter().any(|g| hit(g)))
        .map(artist)
        .collect();
    let albums: Vec<Value> = (0..album_count())
        .filter(|n| {
            hit(ALBUM_WORDS[n % ALBUM_WORDS.len()]) || hit(ARTISTS[n / ALBUMS_PER_ARTIST].0)
        })
        .take(10)
        .map(|n| album(n, false))
        .collect();
    let mut tracks = Vec::new();
    'outer: for n in 0..album_count() {
        for t in 0..TRACKS_PER_ALBUM.min(if n % ALBUMS_PER_ARTIST == 2 {
            3
        } else {
            TRACKS_PER_ALBUM
        }) {
            let word = TRACK_WORDS[(n * 7 + t * 3) % TRACK_WORDS.len()];
            if hit(word) || hit(ARTISTS[n / ALBUMS_PER_ARTIST].0) {
                tracks.push(track(n, t));
                if tracks.len() == 10 {
                    break 'outer;
                }
            }
        }
    }
    let playlists: Vec<Value> = (0..PLAYLISTS.len())
        .filter(|p| hit(PLAYLISTS[*p].0) || hit(PLAYLISTS[*p].1) || !artists.is_empty())
        .take(6)
        .map(playlist)
        .collect();
    let shows: Vec<Value> = (0..3)
        .map(show)
        .filter(|s| hit(s["name"].as_str().unwrap_or("")))
        .collect();

    let wrap = |items: Vec<Value>| {
        let total = items.len();
        page(items, total, false)
    };
    json!({
        "tracks": wrap(tracks),
        "artists": wrap(artists),
        "albums": wrap(albums),
        "playlists": wrap(playlists),
        "shows": wrap(shows),
        "episodes": wrap(Vec::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn parse<T: serde::de::DeserializeOwned>(path: &str) -> T {
        serde_json::from_value(get(path).unwrap_or_else(|| panic!("no demo data for {path}")))
            .unwrap()
    }

    #[test]
    fn demo_catalog_parses_into_the_models() {
        let user: User = parse("/me");
        assert_eq!(user.name().as_ref(), "Demo Listener");

        let playlists: Page<Playlist> = parse("/me/playlists?limit=50&offset=0");
        assert_eq!(playlists.items.len(), PLAYLISTS.len());
        assert!(playlists.items[3].total() > 1000);

        let album: Album = parse("/albums/al4");
        assert_eq!(album.tracks.unwrap().items.len(), TRACKS_PER_ALBUM);

        let items: Page<SavedItem> = parse("/playlists/pl3/items?limit=100&offset=2300");
        assert_eq!(items.items.len(), 100);
        assert!(items.next.is_none());
        assert!(
            items
                .items
                .iter()
                .all(|i| i.item.as_ref().is_some_and(|t| t.playable()))
        );

        let liked: Page<SavedItem> = parse("/me/tracks?limit=50&offset=50");
        assert_eq!(liked.items.len(), 14);
    }

    #[test]
    fn demo_search_finds_artists_and_tracks() {
        let results: SearchResults = parse("/search?q=violet&type=track&limit=10");
        assert_eq!(results.artists.items[0].name.as_ref(), "Violet Hours");
        assert!(!results.tracks.items.is_empty());
        let none: SearchResults = parse("/search?q=zzzzzz&type=track");
        assert!(none.is_empty());
    }
}
