//! Reads through the streaming session (librespot) for what the Web API
//! refuses Lilac.
//!
//! Lilac's Web API app may only list the tracks of playlists the user owns
//! (403 for everything else since February 2026). The streaming session can
//! read any playlist the account can open, as the official clients do; it is
//! used only when the Web API says no.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use librespot_core::{Session, SpotifyUri};
use librespot_metadata::{Metadata as _, Playlist as RawPlaylist, Track as RawTrack};
use librespot_protocol::extended_metadata::{BatchedEntityRequest, EntityRequest, ExtensionQuery};
use librespot_protocol::extension_kind::ExtensionKind;
use librespot_protocol::metadata::Track as TrackMessage;
use protobuf::{EnumOrUnknown, Message as _};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::model::{Page, Playlist, SavedItem};

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

async fn session() -> Result<Session> {
    let mut rx = slot().subscribe();
    let ready = tokio::time::timeout(SESSION_WAIT, rx.wait_for(|s| s.is_some()))
        .await
        .context("the streaming session is not connected")?
        .map_err(|_| anyhow!("the streaming session is not connected"))?;
    Ok(ready.clone().expect("waited for Some"))
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

/// A playlist's details, in the Web API's shape.
pub async fn playlist(id: &str) -> Result<Playlist> {
    let raw = raw_playlist(id, true).await?;
    let attrs = &raw.attributes;
    let owner = match &raw.id {
        SpotifyUri::Playlist {
            user: Some(user), ..
        } => user.clone(),
        _ => String::new(),
    };
    let images = if attrs.picture.is_empty() {
        json!([])
    } else {
        json!([{ "url": image_url(&attrs.picture), "width": 300 }])
    };
    let value = json!({
        "id": id,
        "uri": format!("spotify:playlist:{id}"),
        "name": attrs.name,
        "description": attrs.description,
        "images": images,
        "owner": { "id": owner, "display_name": owner },
        "collaborative": attrs.is_collaborative,
        "items": { "total": raw.length.max(0) },
    });
    Ok(serde_json::from_value(value)?)
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

/// Track details for `uris` in one request (episodes and local files are
/// left out; their rows stay empty).
async fn tracks(uris: &[&SpotifyUri]) -> Result<Vec<Value>> {
    let entity_request: Vec<EntityRequest> = uris
        .iter()
        .filter(|uri| matches!(uri, SpotifyUri::Track { .. }))
        .filter_map(|uri| uri.to_uri().ok())
        .map(|entity_uri| EntityRequest {
            entity_uri,
            query: vec![ExtensionQuery {
                extension_kind: EnumOrUnknown::new(ExtensionKind::TRACK_V4),
                ..Default::default()
            }],
            ..Default::default()
        })
        .collect();
    if entity_request.is_empty() {
        return Ok(Vec::new());
    }
    let session = session().await?;
    let response = session
        .spclient()
        .get_extended_metadata(BatchedEntityRequest {
            entity_request,
            ..Default::default()
        })
        .await?;
    let mut out = Vec::new();
    for array in response.extended_metadata {
        for data in array.extension_data {
            let Some(any) = data.extension_data.as_ref() else {
                continue;
            };
            let Ok(message) = TrackMessage::parse_from_bytes(&any.value) else {
                continue;
            };
            let Ok(uri) = SpotifyUri::from_uri(&data.entity_uri) else {
                continue;
            };
            if let Ok(track) = RawTrack::parse(&message, &uri) {
                out.push(track_json(&track, &data.entity_uri));
            }
        }
    }
    Ok(out)
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
            || track.availability.iter().any(|a| a.start.as_timestamp_ms() <= now));
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
