//! Reads through the streaming session (librespot) for what the Web API
//! refuses Lilac.
//!
//! Lilac's Web API app may only list the tracks of playlists the user owns
//! (403 for everything else since February 2026). The streaming session can
//! read any playlist the account can open, as the official clients do; it is
//! used only when the Web API says no.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{
    Album as RawAlbum, Metadata as _, Playlist as RawPlaylist, Track as RawTrack,
};
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use protobuf::{EnumOrUnknown, Message as _};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::disk_cache;
use crate::model::{Album, Page, Playlist, SavedItem, Track};

/// Matches the Web API's page size for playlist items.
const PAGE: usize = 100;
/// Playlists whose track lists are kept for paging.
const CACHED_PLAYLISTS: usize = 4;
/// A playlist opened again after this is fetched again; within it, its
/// header and first page share one fetch.
const FRESH: Duration = Duration::from_secs(10);
/// How long a read waits for the streaming session to come up.
const SESSION_WAIT: Duration = Duration::from_secs(15);

fn slot() -> &'static watch::Sender<Option<Session>> {
    static SLOT: OnceLock<watch::Sender<Option<Session>>> = OnceLock::new();
    SLOT.get_or_init(|| watch::channel(None).0)
}

/// The player's streaming session, once connected (`None` when it ends).
pub fn set_session(session: Option<Session>) {
    slot().send_replace(session);
}

/// Connect a streaming session for reading only (artist details, lyrics,
/// friend activity) when Soloist does the playing: no player and no
/// Connect device, so Spotify lists Soloist alone. Becomes the session
/// these reads use.
pub async fn connect_session(token: String) -> Result<Session> {
    let config = librespot_core::config::SessionConfig {
        client_id: super::oauth::DESKTOP_CLIENT_ID.to_string(),
        device_id: crate::paths::device_id(),
        ..Default::default()
    };
    // No cache: credentials stay in Lilac's own store.
    let session = Session::new(config, None);
    session
        .connect(
            librespot_core::authentication::Credentials::with_access_token(token),
            false,
        )
        .await
        .map_err(|e| anyhow::anyhow!("could not connect to Spotify: {e}"))?;
    set_session(Some(session.clone()));
    Ok(session)
}

/// The streaming session is not connected (yet); not a refusal.
#[derive(Debug)]
pub struct NoSession;

impl std::fmt::Display for NoSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not connected to Spotify yet")
    }
}

impl std::error::Error for NoSession {}

async fn session() -> Result<Session> {
    let mut rx = slot().subscribe();
    let ready = tokio::time::timeout(SESSION_WAIT, rx.wait_for(|s| s.is_some()))
        .await
        .map_err(|_| anyhow::Error::new(NoSession))?
        .map_err(|_| anyhow::Error::new(NoSession))?;
    Ok(ready.clone().expect("waited for Some"))
}

/// What the people you follow play, as the desktop client reads it.
pub async fn buddylist() -> Result<Vec<u8>> {
    let session = session().await?;
    let bytes = session
        .spclient()
        .request_as_json(
            &reqwest::Method::GET,
            "/presence-view/v1/buddylist",
            None,
            None,
        )
        .await?;
    Ok(bytes.to_vec())
}

/// Track lists of recently opened playlists, so each page is not a new
/// fetch of the whole playlist.
type Cached = (String, Instant, Arc<RawPlaylist>);

fn cache() -> &'static Mutex<Vec<Cached>> {
    static CACHE: OnceLock<Mutex<Vec<Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `fresh`: refetch unless the cached copy is younger than [`FRESH`]
/// (opening a playlist); otherwise any cached copy does (later pages).
async fn raw_playlist(id: &str, fresh: bool) -> Result<Arc<RawPlaylist>> {
    if let Some((_, at, hit)) = cache().lock().unwrap().iter().find(|(key, ..)| key == id)
        && (!fresh || at.elapsed() < FRESH)
    {
        return Ok(hit.clone());
    }
    let session = session().await?;
    let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}"))?;
    let playlist = Arc::new(RawPlaylist::get(&session, &uri).await?);
    let mut cache = cache().lock().unwrap();
    cache.retain(|(key, ..)| key != id);
    cache.insert(0, (id.to_string(), Instant::now(), playlist.clone()));
    cache.truncate(CACHED_PLAYLISTS);
    Ok(playlist)
}

/// A playlist's details, in the Web API's shape. Playlists without an
/// uploaded picture come without a cover (the Web API's mosaic); callers
/// keep one they already know.
pub async fn playlist(id: &str) -> Result<Playlist> {
    use base64::Engine as _;
    let raw = raw_playlist(id, true).await?;
    let attrs = &raw.attributes;
    let owner = match &raw.id {
        SpotifyUri::Playlist {
            user: Some(user), ..
        } => user.clone(),
        _ => String::new(),
    };
    let owner_name = user_names(vec![owner.clone()])
        .await
        .ok()
        .and_then(|names| names.into_iter().next())
        .map(|(_, name)| name)
        .unwrap_or_else(|| owner.clone());
    let sized = |names: &[&str]| {
        attrs
            .picture_sizes
            .iter()
            .find(|p| names.contains(&p.target_name.as_str()) && !p.url.is_empty())
            .map(|p| p.url.clone())
    };
    let mut images: Vec<Value> = [
        (sized(&["small"]), 64),
        (sized(&["default", "medium"]), 300),
        (sized(&["large", "xlarge"]), 640),
    ]
    .into_iter()
    .filter_map(|(url, width)| Some(json!({ "url": url?, "width": width })))
    .collect();
    if images.is_empty() && !attrs.picture.is_empty() {
        images.push(json!({ "url": image_url(&attrs.picture), "width": 300 }));
    }
    let value = json!({
        "id": id,
        "uri": format!("spotify:playlist:{id}"),
        "name": attrs.name,
        "description": Some(&attrs.description).filter(|d| !d.is_empty()),
        "images": images,
        "owner": { "id": owner, "display_name": owner_name },
        // Changes with every edit, like the Web API's snapshot id.
        "snapshot_id": base64::engine::general_purpose::STANDARD.encode(&raw.revision),
        "collaborative": attrs.is_collaborative,
        "items": { "total": raw.length.max(0) },
    });
    Ok(serde_json::from_value(value)?)
}
/// The signed-in profile from the streaming session: the account's own
/// details from the login, name and picture from librespot's
/// `get_user_profile`. Not under the Web API's rate limit.
pub async fn profile() -> Result<crate::model::User> {
    let session = session().await?;
    let id = session.username();
    let bytes = session
        .spclient()
        .get_user_profile(&id, Some(0), Some(0))
        .await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let image = value["image_url"].as_str().filter(|u| !u.is_empty());
    let user = json!({
        "id": id,
        "display_name": value["name"].as_str().filter(|n| !n.is_empty()),
        "images": image.map(|url| json!([{ "url": url, "width": 300 }])).unwrap_or(json!([])),
        "product": session.get_user_attribute("type"),
        "country": Some(session.country()).filter(|c| !c.is_empty()),
        "followers": value["followers_count"].as_u64().map(|total| json!({ "total": total })),
    });
    Ok(serde_json::from_value(user)?)
}

/// Display names for user names (playlist owners), via librespot's
/// `get_user_profile`. Names that fail to resolve are left out.
pub async fn user_names(users: Vec<String>) -> Result<Vec<(String, String)>> {
    static KNOWN: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let session = session().await?;
    let lookups = users.into_iter().map(|user| {
        let session = session.clone();
        async move {
            if let Some(name) = known.lock().unwrap().get(&user) {
                return Some((user, name.clone()));
            }
            let bytes = session
                .spclient()
                .get_user_profile(&user, Some(0), Some(0))
                .await
                .ok()?;
            let value: Value = serde_json::from_slice(&bytes).ok()?;
            let name = value["name"].as_str().filter(|n| !n.is_empty())?;
            let mut known = known.lock().unwrap();
            if known.len() > 500 {
                known.clear();
            }
            known.insert(user.clone(), name.to_string());
            Some((user, name.to_string()))
        }
    });
    Ok(futures_util::future::join_all(lookups)
        .await
        .into_iter()
        .flatten()
        .collect())
}

/// One page of a playlist's tracks, in the Web API's shape.
pub async fn playlist_items(id: &str, offset: u32) -> Result<Page<SavedItem>> {
    let raw = raw_playlist(id, offset == 0).await?;
    let all: Vec<&SpotifyUri> = raw.tracks().collect();
    let from = (offset as usize).min(all.len());
    let page = &all[from..(from + PAGE).min(all.len())];
    let tracks = tracks(page).await?;
    let items: Vec<Value> = page
        .iter()
        .map(|uri| {
            let uri = uri.to_uri().unwrap_or_default();
            let track = tracks.iter().find(|t| t["uri"] == uri.as_str());
            json!({ "item": track })
        })
        .collect();
    let next = (from + PAGE < all.len()).then(|| format!("offset={}", from + PAGE));
    let value = json!({
        "items": items,
        "total": all.len(),
        "offset": from,
        "next": next,
    });
    Ok(serde_json::from_value(value)?)
}

/// The language names are asked for in (see [`get_many`]).
pub fn language() -> &'static str {
    static LANGUAGE: OnceLock<String> = OnceLock::new();
    LANGUAGE.get_or_init(super::accept_language)
}

/// Items per extended-metadata request.
const BATCH: usize = 200;

/// librespot's metadata for many items, in one extended-metadata request
/// per [`BATCH`], parsed by librespot. Sent with the user's language:
/// librespot's own getters send none, and Spotify then names artists in
/// their own script ("ヨルシカ" rather than "Yorushika").
async fn get_many<T: librespot_metadata::Metadata>(
    kind: ExtensionKind,
    uris: &[SpotifyUri],
) -> Result<Vec<T>> {
    if uris.is_empty() {
        return Ok(Vec::new());
    }
    let session = session().await?;
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(value) = language().parse() {
        headers.insert(reqwest::header::ACCEPT_LANGUAGE, value);
    }
    let requests = uris.chunks(BATCH).map(|chunk| {
        let request = BatchedEntityRequest {
            entity_request: chunk
                .iter()
                .filter_map(|uri| uri.to_uri().ok())
                .map(|entity_uri| EntityRequest {
                    entity_uri,
                    query: vec![ExtensionQuery {
                        extension_kind: EnumOrUnknown::new(kind),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let (session, headers) = (session.clone(), headers.clone());
        async move {
            session
                .spclient()
                .request_with_protobuf(
                    &reqwest::Method::POST,
                    "/extended-metadata/v0/extended-metadata",
                    Some(headers),
                    &request,
                )
                .await
        }
    });
    let mut out = Vec::new();
    for response in futures_util::future::join_all(requests).await {
        let response = BatchedExtensionResponse::parse_from_bytes(&response?)?;
        for array in response.extended_metadata {
            for data in array.extension_data {
                let Some(any) = data.extension_data.as_ref() else {
                    continue;
                };
                let Ok(message) = T::Message::parse_from_bytes(&any.value) else {
                    continue;
                };
                let Ok(uri) = SpotifyUri::from_uri(&data.entity_uri) else {
                    continue;
                };
                if let Ok(item) = T::parse(&message, &uri) {
                    out.push(item);
                }
            }
        }
    }
    Ok(out)
}

/// Track details for `uris`, in the Web API's shape and in their order.
/// Kept on disk ([`disk_cache::TRACKS`]); only the ones not kept are asked
/// for. Episodes and local files are left out; their rows stay empty.
async fn tracks(uris: &[&SpotifyUri]) -> Result<Vec<Value>> {
    let ids: Vec<String> = uris
        .iter()
        .filter(|uri| matches!(uri, SpotifyUri::Track { .. }))
        .map(|uri| base62(uri))
        .collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let lookup = ids.clone();
    let mut known: HashMap<String, Value> = tokio::task::spawn_blocking(move || {
        lookup
            .into_iter()
            .filter_map(|id| {
                let hit = disk_cache::TRACKS.get(&id)?;
                (hit.value["lang"] == language()).then_some(())?;
                Some((id, hit.value["track"].clone()))
            })
            .collect()
    })
    .await
    .unwrap_or_default();

    let missing: Vec<SpotifyUri> = ids
        .iter()
        .filter(|id| !known.contains_key(*id))
        .filter_map(|id| SpotifyUri::from_uri(&format!("spotify:track:{id}")).ok())
        .collect();
    if !missing.is_empty() {
        let fetched: Vec<(String, Value)> = get_many::<RawTrack>(ExtensionKind::TRACK_V4, &missing)
            .await?
            .iter()
            .map(|track| {
                let uri = track.id.to_uri().unwrap_or_default();
                (base62(&track.id), track_json(track, &uri))
            })
            .collect();
        let keep = fetched.clone();
        tokio::task::spawn_blocking(move || {
            for (id, track) in keep {
                disk_cache::TRACKS.put(&id, &json!({ "lang": language(), "track": track }));
            }
        });
        known.extend(fetched);
    }
    Ok(ids.iter().filter_map(|id| known.remove(id)).collect())
}
/// One track's details (kept on disk like any other; see [`tracks`]).
pub async fn track(uri: &str) -> Result<Track> {
    let uri = SpotifyUri::from_uri(uri)?;
    let value = tracks(&[&uri])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("track not found"))?;
    Ok(serde_json::from_value(value)?)
}

/// An album and its tracks, in the Web API's shape.
pub async fn album(id: &str) -> Result<(Album, Vec<Track>)> {
    let uri = SpotifyUri::from_uri(&format!("spotify:album:{id}"))?;
    let raw: RawAlbum = get_many(ExtensionKind::ALBUM_V4, &[uri])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("album not found"))?;
    let track_uris: Vec<&SpotifyUri> = raw.tracks().collect();
    let tracks: Vec<Track> = tracks(&track_uris)
        .await?
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    let mut value = album_json(&raw);
    value["total_tracks"] = json!(track_uris.len());
    value["copyrights"] = raw
        .copyrights
        .iter()
        .map(|c| json!({ "text": c.text }))
        .collect();
    Ok((serde_json::from_value(value)?, tracks))
}

/// Spotify's own lyrics for a track (color-lyrics JSON).
pub async fn lyrics(track_id: &str) -> Result<Value> {
    let session = session().await?;
    let id = librespot_core::SpotifyId::from_base62(track_id)?;
    let bytes = session.spclient().get_lyrics(&id).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn images_json(images: &librespot_metadata::image::Images) -> Value {
    use librespot_metadata::image::ImageSize;
    images
        .iter()
        .map(|image| {
            let width = match (image.width, image.size) {
                (w, _) if w > 0 => w,
                (_, ImageSize::SMALL) => 64,
                (_, ImageSize::LARGE) => 640,
                (_, ImageSize::XLARGE) => 1280,
                _ => 300,
            };
            json!({ "url": image_url(&image.id.0), "width": width })
        })
        .collect()
}

fn album_json(album: &RawAlbum) -> Value {
    use librespot_metadata::album::AlbumType;
    let kind = match album.album_type {
        AlbumType::SINGLE => "single",
        AlbumType::COMPILATION => "compilation",
        AlbumType::EP => "ep",
        _ => "album",
    };
    let date = album.date.0;
    json!({
        "id": base62(&album.id),
        "uri": album.id.to_uri().unwrap_or_default(),
        "name": album.name,
        "images": images_json(&album.covers),
        "release_date": format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day()),
        "album_type": kind,
        "artists": album.artists.iter().map(|a| json!({ "id": base62(&a.id), "name": a.name })).collect::<Vec<_>>(),
    })
}

fn image_url(file_id: &[u8]) -> String {
    let hex: String = file_id.iter().map(|b| format!("{b:02x}")).collect();
    format!("https://i.scdn.co/image/{hex}")
}

fn base62(uri: &SpotifyUri) -> String {
    uri.to_id().unwrap_or_default()
}

/// What librespot would refuse to play: no audio and no stand-in, or not
/// released yet (playlists list announced albums' tracks before that).
fn playable(track: &RawTrack) -> bool {
    let now = librespot_core::date::Date::now_utc().as_timestamp_ms();
    let has_audio = !track.files.is_empty() || !track.alternatives.is_empty();
    let released = track.earliest_live_timestamp.as_timestamp_ms() <= now
        && (track.availability.is_empty()
            || track
                .availability
                .iter()
                .any(|a| a.start.as_timestamp_ms() <= now));
    has_audio && released
}

fn track_json(track: &RawTrack, uri: &str) -> Value {
    use librespot_metadata::image::ImageSize;
    let images: Vec<Value> = track
        .album
        .covers
        .iter()
        .map(|image| {
            let width = match (image.width, image.size) {
                (w, _) if w > 0 => w,
                (_, ImageSize::SMALL) => 64,
                (_, ImageSize::LARGE) => 640,
                (_, ImageSize::XLARGE) => 1280,
                _ => 300,
            };
            json!({ "url": image_url(&image.id.0), "width": width })
        })
        .collect();
    let artists: Vec<Value> = track
        .artists
        .iter()
        .map(|a| json!({ "id": base62(&a.id), "name": a.name }))
        .collect();
    json!({
        "id": base62(&track.id),
        "uri": uri,
        "is_playable": playable(track),
        "name": track.name,
        "type": "track",
        "duration_ms": track.duration.max(0),
        "explicit": track.is_explicit,
        "track_number": track.number.max(0),
        "disc_number": track.disc_number.max(0),
        "artists": artists,
        "album": {
            "id": base62(&track.album.id),
            "name": track.album.name,
            "images": images,
        },
    })
}
