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
    Album as RawAlbum, Artist as RawArtist, Metadata as _, Playlist as RawPlaylist,
    Track as RawTrack,
};
use librespot_protocol::extended_metadata::{
    BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery,
};
use librespot_protocol::extension_kind::ExtensionKind;
use protobuf::{EnumOrUnknown, Message as _};
use serde_json::{Value, json};
use tokio::sync::watch;

use gpui_kit::SharedString;

use crate::disk_cache;
use crate::model::{Album, Artist, Page, Playlist, SavedItem, Track};

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

/// The persisted query Spotify's web player sends for an artist's page.
/// Spotify changes it with web player releases; a stale one is answered
/// with an error, which callers show as "unavailable".
const ARTIST_OVERVIEW_HASH: &str =
    "9f8134ef565e78621f1e1793555bd6633c5ac144ae0f89604ed3ae3f80b3c8e6";

/// What the official apps show about an artist beyond the catalog.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ArtistOverview {
    pub monthly_listeners: Option<u64>,
    pub followers: Option<u64>,
    pub world_rank: Option<u64>,
    pub biography: Option<String>,
    /// Instagram, Wikipedia, X…: (name, url).
    pub links: Vec<(String, String)>,
    pub header_image: Option<String>,
    /// The main color of the artist's picture, as Spotify extracted it.
    pub color: Option<u32>,
    /// Photos the artist chose for their About section.
    pub gallery: Vec<String>,
    pub concerts: Vec<Concert>,
    /// Where people listen: (city, country code, listeners).
    pub top_cities: Vec<(String, String, u64)>,
    /// Playlists that brought listeners to them: (id, name, image).
    pub discovered_on: Vec<(String, String, Option<String>)>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Concert {
    pub title: String,
    /// ISO date-time as Spotify gives it.
    pub date: String,
    pub venue: String,
    pub city: String,
}

/// The artist overview of Spotify's own web player. Undocumented and
/// subject to change; every field is optional.
/// `login` is the desktop client's login (the user's own sign-in); the
/// streaming session's token is tried after it. Each is sent as the web
/// player and as a plain client: Spotify accepts different combinations
/// at different times, and one refusal should not hide the rest.
pub async fn artist_overview(id: &str, login: &super::api::Auth) -> Result<ArtistOverview> {
    let session = session().await?;
    let client_token = session.spclient().client_token().await.unwrap_or_default();
    let mut tokens = Vec::new();
    if let Ok(token) = login.token().await {
        tokens.push(token);
    }
    if let Ok(token) = session.login5().auth_token().await {
        tokens.push(token.access_token);
    }
    if tokens.is_empty() {
        anyhow::bail!("not signed in to Spotify");
    }
    let body = json!({
        "variables": { "uri": format!("spotify:artist:{id}"), "locale": "", "includePrerelease": true },
        "operationName": "queryArtistOverview",
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": ARTIST_OVERVIEW_HASH } },
    });
    let mut last = anyhow::anyhow!("Spotify did not answer");

    // First as the desktop app asks, through the session's own client.
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let raw = serde_json::to_vec(&body)?;
    match session
        .spclient()
        .request(
            &reqwest::Method::POST,
            "/pathfinder/v2/query",
            Some(headers),
            Some(&raw),
        )
        .await
    {
        Ok(bytes) => {
            if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(artist) = value.pointer("/data/artistUnion").filter(|a| a.is_object()) {
                    return Ok(parse_overview(artist));
                }
                if let Some(error) = value.pointer("/errors/0/message").and_then(Value::as_str) {
                    last = anyhow::anyhow!("Spotify's web player changed ({error})");
                }
            }
        }
        Err(err) => last = anyhow::anyhow!("Spotify refused the request ({err})"),
    }

    // Then directly, introducing itself as a Spotify client does.
    let agent = format!(
        "Spotify/{} Linux/0 ({})",
        librespot_core::version::spotify_version(),
        librespot_core::version::VERSION_STRING
    );
    for token in &tokens {
        for platform in [Some("WebPlayer"), None] {
            let mut request = super::http()
                .post("https://api-partner.spotify.com/pathfinder/v2/query")
                .bearer_auth(token)
                .header(reqwest::header::USER_AGENT, &agent)
                .json(&body);
            if let Some(platform) = platform {
                request = request.header("app-platform", platform);
            }
            if !client_token.is_empty() {
                request = request.header("client-token", &client_token);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(err) => {
                    last = err.into();
                    continue;
                }
            };
            let status = response.status();
            if !status.is_success() {
                last = if matches!(status.as_u16(), 401 | 403) {
                    anyhow::anyhow!("Spotify refused the request ({status})")
                } else {
                    anyhow::anyhow!("Spotify answered with an error ({status})")
                };
                continue;
            }
            let value: Value = response.json().await?;
            if let Some(error) = value.pointer("/errors/0/message").and_then(Value::as_str) {
                // A stale query is stale for every token.
                anyhow::bail!("Spotify's web player changed ({error})");
            }
            let artist = value
                .pointer("/data/artistUnion")
                .filter(|a| a.is_object())
                .ok_or_else(|| anyhow::anyhow!("Spotify sent no artist"))?;
            return Ok(parse_overview(artist));
        }
    }
    Err(last)
}

fn parse_overview(artist: &Value) -> ArtistOverview {
    let text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let number = |path: &str| {
        artist
            .pointer(path)
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
    };
    let links = artist
        .pointer("/profile/externalLinks/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|l| Some((text(l.get("name"))?, text(l.get("url"))?)))
                .collect()
        })
        .unwrap_or_default();
    let concerts = artist
        .pointer("/goods/concerts/items")
        .or_else(|| artist.pointer("/goods/events/concerts/items"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| item.get("data").unwrap_or(item))
                .filter_map(|c| {
                    let date = text(c.get("startDateIsoString"))
                        .or_else(|| text(c.pointer("/date/isoString")))?;
                    Some(Concert {
                        title: text(c.get("title")).unwrap_or_default(),
                        date,
                        venue: text(c.pointer("/location/name"))
                            .or_else(|| text(c.pointer("/venue/name")))
                            .unwrap_or_default(),
                        city: text(c.pointer("/location/city"))
                            .or_else(|| text(c.pointer("/venue/location/name")))
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let top_cities = artist
        .pointer("/stats/topCities/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|c| {
                    Some((
                        text(c.get("city"))?,
                        text(c.get("country")).unwrap_or_default(),
                        c.get("numberOfListeners")?.as_u64()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let gallery = artist
        .pointer("/visuals/gallery/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| text(i.pointer("/sources/0/url")))
                .collect()
        })
        .unwrap_or_default();
    let discovered_on = artist
        .pointer("/relatedContent/discoveredOnV2/items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get("data"))
                .filter(|d| d.get("__typename").and_then(Value::as_str) == Some("Playlist"))
                .filter_map(|d| {
                    Some((
                        text(d.get("id"))?,
                        text(d.get("name")).unwrap_or_default(),
                        text(d.pointer("/images/items/0/sources/0/url")),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    ArtistOverview {
        monthly_listeners: number("/stats/monthlyListeners"),
        followers: number("/stats/followers"),
        world_rank: number("/stats/worldRank"),
        biography: text(artist.pointer("/profile/biography/text")).map(|b| strip_tags(&b)),
        links,
        header_image: text(artist.pointer("/headerImage/data/sources/0/url"))
            .or_else(|| text(artist.pointer("/visuals/headerImage/sources/0/url"))),
        color: text(artist.pointer("/visuals/avatarImage/extractedColors/colorRaw/hex"))
            .and_then(|hex| u32::from_str_radix(hex.trim_start_matches('#'), 16).ok()),
        gallery,
        concerts,
        top_cities,
        discovered_on,
    }
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

/// Rootlist entries read per request; Spotify's own clients ask for 120.
const ROOTLIST_PAGE: usize = 120;
/// Stops a runaway rootlist (Spotify caps it far below this).
const ROOTLIST_MAX: usize = 2000;

/// The playlists in the user's library, in their sidebar order, read with
/// librespot's `get_rootlist`. This is the official clients' own list and is
/// not under the Web API's per-app rate limit.
///
/// Folders are flattened. Owners come back as user names only; the caller
/// fills in display names it already knows.
pub async fn rootlist() -> Result<Vec<Playlist>> {
    use librespot_protocol::playlist4_external::SelectedListContent;

    let session = session().await?;
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let bytes = session
            .spclient()
            .get_rootlist(from, Some(ROOTLIST_PAGE))
            .await?;
        // Parsed from the message itself: librespot's typed wrapper reads
        // each item's revision as a 16-byte id, which rootlist revisions
        // are not, and fails the whole list.
        let list = SelectedListContent::parse_from_bytes(&bytes)?;
        let contents = list.contents.get_or_default();
        for (index, item) in contents.items.iter().enumerate() {
            let Some(id) = item.uri().strip_prefix("spotify:playlist:") else {
                continue;
            };
            let meta = contents.meta_items.get(index);
            let playlist = rootlist_playlist(id, meta);
            // Playlists deleted or no longer available come back nameless.
            if !playlist.name.is_empty() {
                out.push(playlist);
            }
        }
        from += contents.items.len();
        let total = list.length().max(0) as usize;
        if contents.items.is_empty() || from >= total || from >= ROOTLIST_MAX {
            return Ok(out);
        }
    }
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

fn rootlist_playlist(
    id: &str,
    meta: Option<&librespot_protocol::playlist4_external::MetaItem>,
) -> Playlist {
    let mut playlist = Playlist {
        id: id.to_string().into(),
        uri: format!("spotify:playlist:{id}").into(),
        ..Default::default()
    };
    let Some(meta) = meta else {
        return playlist;
    };
    let attrs = meta.attributes.get_or_default();
    playlist.name = attrs.name().to_string().into();
    playlist.description = Some(attrs.description())
        .filter(|d| !d.is_empty())
        .map(|d| d.to_string().into());
    playlist.collaborative = attrs.collaborative();
    playlist.owner.id = meta.owner_username().to_string().into();
    playlist.items = Some(crate::model::PageRef {
        total: meta.length().max(0) as u32,
    });
    // Spotify's ready-made sizes (mosaics included); else the uploaded picture.
    let sized = |names: &[&str]| {
        attrs
            .picture_size
            .iter()
            .find(|p| names.contains(&p.target_name()) && !p.url().is_empty())
            .map(|p| SharedString::from(p.url().to_string()))
    };
    playlist.images.small = sized(&["small"]);
    playlist.images.medium = sized(&["default", "medium"]);
    playlist.images.large = sized(&["large", "xlarge"]);
    if playlist.images.is_empty() && !attrs.picture().is_empty() {
        playlist.images.medium = Some(image_url(attrs.picture()).into());
    }
    playlist
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

/// One entry of a library list (liked songs and saved albums, followed
/// artists, saved podcasts) from Spotify's collection service.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectionItem {
    pub uri: String,
    /// Unix seconds.
    pub added_at: i64,
    /// Only in changes: the entry was taken out.
    pub removed: bool,
}

/// Entries per page of a full library list.
const COLLECTION_PAGE: i32 = 300;
/// Stops a runaway list.
const COLLECTION_MAX: usize = 20_000;

/// The collection service's messages (`collection2v2.proto`, which
/// librespot ships but does not compile), written and read by hand: a few
/// strings, numbers and flags.
mod wire {
    use protobuf::{CodedInputStream, CodedOutputStream};

    use super::CollectionItem;

    /// PageRequest {1 username, 2 set, 3 pagination_token, 4 limit} and
    /// DeltaRequest {1 username, 2 set, 3 last_sync_token}.
    pub fn request(username: &str, set: &str, token: &str, limit: Option<i32>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut os = CodedOutputStream::vec(&mut out);
            let _ = os.write_string(1, username);
            let _ = os.write_string(2, set);
            if !token.is_empty() {
                let _ = os.write_string(3, token);
            }
            if let Some(limit) = limit {
                let _ = os.write_int32(4, limit);
            }
            let _ = os.flush();
        }
        out
    }

    /// WriteRequest {1 username, 2 set, 3 items, 4 client_update_id}, each
    /// item CollectionItem {1 uri, 2 added_at, 3 is_removed}.
    pub fn write_request(
        username: &str,
        set: &str,
        uris: &[String],
        removed: bool,
        now: i32,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut os = CodedOutputStream::vec(&mut out);
            let _ = os.write_string(1, username);
            let _ = os.write_string(2, set);
            for uri in uris {
                let mut item = Vec::new();
                {
                    let mut is = CodedOutputStream::vec(&mut item);
                    let _ = is.write_string(1, uri);
                    let _ = is.write_int32(2, now);
                    if removed {
                        let _ = is.write_bool(3, true);
                    }
                    let _ = is.flush();
                }
                let _ = os.write_bytes(3, &item);
            }
            let _ = os.write_string(4, &format!("lilac-{now}"));
            let _ = os.flush();
        }
        out
    }

    /// PageResponse {1 items, 2 next_page_token, 3 sync_token} and
    /// DeltaResponse {1 delta_update_possible, 2 items, 3 sync_token}.
    pub struct Response {
        pub items: Vec<CollectionItem>,
        pub next_page_token: String,
        pub sync_token: String,
        pub delta_possible: bool,
    }

    pub fn response(bytes: &[u8], delta: bool) -> protobuf::Result<Response> {
        let mut out = Response {
            items: Vec::new(),
            next_page_token: String::new(),
            sync_token: String::new(),
            delta_possible: false,
        };
        let items_field = if delta { 2 } else { 1 };
        let mut is = CodedInputStream::from_bytes(bytes);
        while let Some(tag) = is.read_raw_tag_or_eof()? {
            let (field, wire_type) = (tag >> 3, tag & 7);
            match (field, wire_type) {
                (f, 2) if f == items_field => {
                    let raw = is.read_bytes()?;
                    out.items.push(item(&raw)?);
                }
                (2, 2) if !delta => out.next_page_token = is.read_string()?,
                (1, 0) if delta => out.delta_possible = is.read_bool()?,
                (3, 2) => out.sync_token = is.read_string()?,
                _ => skip(&mut is, wire_type)?,
            }
        }
        Ok(out)
    }

    fn skip(is: &mut CodedInputStream, wire_type: u32) -> protobuf::Result<()> {
        match wire_type {
            0 => drop(is.read_raw_varint64()?),
            1 => drop(is.read_fixed64()?),
            2 => drop(is.read_bytes()?),
            5 => drop(is.read_fixed32()?),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unexpected protobuf wire type",
                )
                .into());
            }
        }
        Ok(())
    }

    /// CollectionItem {1 uri, 2 added_at, 3 is_removed, 4 context_uri}.
    fn item(bytes: &[u8]) -> protobuf::Result<CollectionItem> {
        let mut item = CollectionItem {
            uri: String::new(),
            added_at: 0,
            removed: false,
        };
        let mut is = CodedInputStream::from_bytes(bytes);
        while let Some(tag) = is.read_raw_tag_or_eof()? {
            match (tag >> 3, tag & 7) {
                (1, 2) => item.uri = is.read_string()?,
                (2, 0) => item.added_at = is.read_int32()? as i64,
                (3, 0) => item.removed = is.read_bool()?,
                (_, wire_type) => skip(&mut is, wire_type)?,
            }
        }
        Ok(item)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reads_what_it_writes_shaped_like_spotify() {
            // A PageResponse with one item, a next page and a sync token.
            let mut item_bytes = Vec::new();
            {
                let mut os = CodedOutputStream::vec(&mut item_bytes);
                os.write_string(1, "spotify:track:a").unwrap();
                os.write_int32(2, 1_700_000_000).unwrap();
                os.flush().unwrap();
            }
            let mut page = Vec::new();
            {
                let mut os = CodedOutputStream::vec(&mut page);
                os.write_bytes(1, &item_bytes).unwrap();
                os.write_string(2, "next").unwrap();
                os.write_string(3, "sync").unwrap();
                os.write_int32(9, 7).unwrap();
                os.flush().unwrap();
            }
            let read = response(&page, false).unwrap();
            assert_eq!(read.items.len(), 1);
            assert_eq!(read.items[0].uri, "spotify:track:a");
            assert_eq!(read.items[0].added_at, 1_700_000_000);
            assert_eq!(read.next_page_token, "next");
            assert_eq!(read.sync_token, "sync");
            let req = request("u", "collection", "", Some(300));
            assert!(!req.is_empty());
        }
    }
}

/// The collection service wants its own content type; plain protobuf is
/// refused with 400.
const COLLECTION_TYPE: &str = "application/vnd.collection-v2.spotify.proto";

async fn collection_call(endpoint: &str, body: Vec<u8>) -> Result<Vec<u8>> {
    let session = session().await?;
    let mut headers = reqwest::header::HeaderMap::new();
    let kind = reqwest::header::HeaderValue::from_static(COLLECTION_TYPE);
    headers.insert(reqwest::header::CONTENT_TYPE, kind.clone());
    headers.insert(reqwest::header::ACCEPT, kind);
    Ok(session
        .spclient()
        .request(&reqwest::Method::POST, endpoint, Some(headers), Some(&body))
        .await?
        .to_vec())
}

/// A whole library list (`set`: "collection" for liked songs and saved
/// albums, "artist", "show"), newest first, and the token that later asks
/// only for what changed ([`collection_changes`]). Spotify's own clients
/// sync their library this way; the request is ours (librespot has the
/// message formats but no call for it).
pub async fn collection(set: &str) -> Result<(Vec<CollectionItem>, String)> {
    let username = session().await?.username();
    let mut items = Vec::new();
    let mut token = String::new();
    loop {
        let body = wire::request(&username, set, &token, Some(COLLECTION_PAGE));
        let page = wire::response(
            &collection_call("/collection/v2/paging", body).await?,
            false,
        )?;
        items.extend(page.items);
        if page.next_page_token.is_empty() || items.len() >= COLLECTION_MAX {
            let mut items: Vec<CollectionItem> = items.into_iter().filter(|i| !i.removed).collect();
            items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
            return Ok((items, page.sync_token));
        }
        token = page.next_page_token;
    }
}

/// What changed in a library list since `sync_token`, and the next token.
/// `None` when Spotify cannot say (the token is too old): load it whole.
pub async fn collection_changes(
    set: &str,
    sync_token: &str,
) -> Result<Option<(Vec<CollectionItem>, String)>> {
    let username = session().await?.username();
    let body = wire::request(&username, set, sync_token, None);
    let delta = wire::response(&collection_call("/collection/v2/delta", body).await?, true)?;
    Ok(delta
        .delta_possible
        .then_some((delta.items, delta.sync_token)))
}

/// Add to (or take out of) a library list: liked songs and saved albums
/// ("collection"), followed artists ("artist"), saved podcasts ("show"), as
/// Spotify's own clients write them.
pub async fn collection_write(set: &str, uris: &[String], saved: bool) -> Result<()> {
    if uris.is_empty() {
        return Ok(());
    }
    let username = session().await?.username();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i32)
        .unwrap_or(0);
    let body = wire::write_request(&username, set, uris, !saved, now);
    collection_call("/collection/v2/write", body).await?;
    Ok(())
}

/// Tracks by URI, in the Web API's shape and in the given order (kept on
/// disk; only the ones not kept are asked for).
pub async fn tracks_by_uri(uris: &[String]) -> Result<Vec<Track>> {
    let parsed: Vec<SpotifyUri> = uris
        .iter()
        .filter_map(|u| SpotifyUri::from_uri(u).ok())
        .collect();
    let refs: Vec<&SpotifyUri> = parsed.iter().collect();
    Ok(tracks(&refs)
        .await?
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

/// Albums by URI, in the given order.
pub async fn albums_by_uri(uris: &[String]) -> Result<Vec<Album>> {
    let parsed: Vec<SpotifyUri> = uris
        .iter()
        .filter_map(|u| SpotifyUri::from_uri(u).ok())
        .collect();
    let found: Vec<RawAlbum> = get_many(ExtensionKind::ALBUM_V4, &parsed).await?;
    Ok(in_order(
        uris,
        found.iter().map(|album| {
            let mut value = album_json(album);
            value["total_tracks"] = json!(album.tracks().count());
            (album.id.to_uri().unwrap_or_default(), value)
        }),
    ))
}

/// Artists by URI, in the given order.
pub async fn artists_by_uri(uris: &[String]) -> Result<Vec<Artist>> {
    let parsed: Vec<SpotifyUri> = uris
        .iter()
        .filter_map(|u| SpotifyUri::from_uri(u).ok())
        .collect();
    let found: Vec<RawArtist> = get_many(ExtensionKind::ARTIST_V4, &parsed).await?;
    Ok(in_order(
        uris,
        found
            .iter()
            .map(|artist| (artist.id.to_uri().unwrap_or_default(), artist_json(artist))),
    ))
}

/// Podcasts by URI, in the given order.
pub async fn shows_by_uri(uris: &[String]) -> Result<Vec<crate::model::Show>> {
    use librespot_metadata::Show as RawShow;
    let parsed: Vec<SpotifyUri> = uris
        .iter()
        .filter_map(|u| SpotifyUri::from_uri(u).ok())
        .collect();
    let found: Vec<RawShow> = get_many(ExtensionKind::SHOW_V4, &parsed).await?;
    Ok(in_order(
        uris,
        found.iter().map(|show| {
            let uri = show.id.to_uri().unwrap_or_default();
            let value = json!({
                "id": base62(&show.id),
                "uri": uri,
                "name": show.name,
                "publisher": show.publisher,
                "description": show.description,
                "images": images_json(&show.covers),
                "total_episodes": show.episodes.len(),
            });
            (uri, value)
        }),
    ))
}

/// Parsed items put back in the order of `uris`.
fn in_order<T: serde::de::DeserializeOwned>(
    uris: &[String],
    found: impl Iterator<Item = (String, Value)>,
) -> Vec<T> {
    let mut found: HashMap<String, Value> = found.collect();
    uris.iter()
        .filter_map(|uri| found.remove(uri))
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect()
}

/// A podcast, in the Web API's shape.
pub async fn show(id: &str) -> Result<crate::model::Show> {
    let uri = format!("spotify:show:{id}");
    shows_by_uri(std::slice::from_ref(&uri))
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("podcast not found"))
}

/// Episodes per page, as the Web API pages them.
const EPISODE_PAGE: usize = 50;

/// One page of a podcast's episodes, newest first, in the Web API's shape.
pub async fn show_episodes(show: &crate::model::Show, offset: u32) -> Result<Page<Track>> {
    use librespot_metadata::{Episode as RawEpisode, Show as RawShow};
    let uri = SpotifyUri::from_uri(&format!("spotify:show:{}", show.id))?;
    let raw: RawShow = get_many(ExtensionKind::SHOW_V4, &[uri])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("podcast not found"))?;
    let all: Vec<SpotifyUri> = raw.episodes.iter().cloned().collect();
    let from = (offset as usize).min(all.len());
    let slice = &all[from..(from + EPISODE_PAGE).min(all.len())];
    let mut episodes: Vec<RawEpisode> = get_many(ExtensionKind::EPISODE_V4, slice).await?;
    episodes.sort_by_key(|e| std::cmp::Reverse(e.publish_time.0));
    let show_ref = json!({
        "id": show.id.as_ref(),
        "name": show.name.as_ref(),
        "images": show.images.to_api_json(),
    });
    let items: Vec<Value> = episodes
        .iter()
        .map(|e| {
            let date = e.publish_time.0;
            json!({
                "id": base62(&e.id),
                "uri": e.id.to_uri().unwrap_or_default(),
                "name": e.name,
                "type": "episode",
                "duration_ms": e.duration.max(0),
                "explicit": e.is_explicit,
                "is_playable": true,
                "description": e.description,
                "release_date": format!("{:04}-{:02}-{:02}", date.year(), u8::from(date.month()), date.day()),
                "images": images_json(&e.covers),
                "show": show_ref,
            })
        })
        .collect();
    let next = (from + EPISODE_PAGE < all.len()).then(|| format!("offset={}", from + EPISODE_PAGE));
    Ok(serde_json::from_value(json!({
        "items": items,
        "total": all.len(),
        "offset": from,
        "next": next,
    }))?)
}

/// Playlists read at once while making covers.
const COVER_PARALLEL: usize = 4;

/// Covers for playlists without an uploaded picture, made the way Spotify
/// makes them: the covers of the first four different albums in the
/// playlist, tiled (`mosaic.scdn.co`), or the first album's cover when there
/// are fewer than four. Each playlist is read once (four at a time), and
/// the albums' covers come in one batched lookup (kept on disk).
pub async fn playlist_covers(ids: Vec<String>) -> Result<Vec<(String, crate::model::Images)>> {
    use futures_util::StreamExt as _;
    let session = session().await?;
    let firsts: Vec<(String, Vec<SpotifyUri>)> = futures_util::stream::iter(ids)
        .map(|id| {
            let session = session.clone();
            async move {
                let uri = SpotifyUri::from_uri(&format!("spotify:playlist:{id}")).ok()?;
                let playlist = RawPlaylist::get(&session, &uri).await.ok()?;
                let tracks: Vec<SpotifyUri> = playlist
                    .tracks()
                    .filter(|t| matches!(t, SpotifyUri::Track { .. }))
                    .take(12)
                    .cloned()
                    .collect();
                Some((id, tracks))
            }
        })
        .buffer_unordered(COVER_PARALLEL)
        .filter_map(|found| async move { found })
        .collect()
        .await;
    let mut all: Vec<&SpotifyUri> = firsts.iter().flat_map(|(_, t)| t.iter()).collect();
    all.dedup();
    let details: HashMap<String, Value> = tracks(&all)
        .await?
        .into_iter()
        .filter_map(|t| Some((t["uri"].as_str()?.to_string(), t)))
        .collect();
    // An image id (40 hex digits) of a cover at roughly `width`.
    let cover = |track: &Value, width: u64| -> Option<String> {
        let images = track.pointer("/album/images")?.as_array()?;
        let best = images
            .iter()
            .min_by_key(|i| i["width"].as_u64().unwrap_or(0).abs_diff(width))?;
        Some(best["url"].as_str()?.rsplit('/').next()?.to_string())
    };
    Ok(firsts
        .into_iter()
        .filter_map(|(id, tracks)| {
            let mut albums: Vec<&Value> = Vec::new();
            for uri in &tracks {
                let Some(track) = uri.to_uri().ok().and_then(|u| details.get(&u)) else {
                    continue;
                };
                let album = &track["album"]["id"];
                if !albums.iter().any(|a| &a["album"]["id"] == album) {
                    albums.push(track);
                }
            }
            let first = albums.first()?;
            let images = if albums.len() >= 4 {
                let tile = |width: u64| -> Option<SharedString> {
                    let ids: Option<Vec<String>> =
                        albums.iter().take(4).map(|t| cover(t, width)).collect();
                    let size = if width >= 640 { 640 } else { 300 };
                    Some(format!("https://mosaic.scdn.co/{size}/{}", ids?.concat()).into())
                };
                crate::model::Images {
                    small: tile(300),
                    medium: tile(300),
                    large: tile(640),
                }
            } else {
                let url = |width: u64| {
                    cover(first, width)
                        .map(|id| SharedString::from(format!("https://i.scdn.co/image/{id}")))
                };
                crate::model::Images {
                    small: url(64),
                    medium: url(300),
                    large: url(640),
                }
            };
            Some((id, images))
        })
        .collect())
}

/// Releases per end of an artist's album and single lists looked at for
/// [`latest_releases`]. Spotify does not say which end is newest, so both
/// ends are read.
const LATEST_PER_END: usize = 4;

/// Recent albums and singles of several artists, for "What's New": two
/// batched lookups for all of them (artists, then their releases) instead
/// of one Web API request per artist.
pub async fn latest_releases(artist_ids: &[String]) -> Result<Vec<Album>> {
    let artist_uris: Vec<SpotifyUri> = artist_ids
        .iter()
        .filter_map(|id| SpotifyUri::from_uri(&format!("spotify:artist:{id}")).ok())
        .collect();
    let artists: Vec<RawArtist> = get_many(ExtensionKind::ARTIST_V4, &artist_uris).await?;
    let mut releases: Vec<SpotifyUri> = Vec::new();
    for artist in &artists {
        for groups in [&artist.albums, &artist.singles] {
            let firsts: Vec<&SpotifyUri> = groups.iter().filter_map(|g| g.0.first()).collect();
            let ends = firsts
                .iter()
                .take(LATEST_PER_END)
                .chain(firsts.iter().rev().take(LATEST_PER_END));
            for uri in ends {
                if !releases.contains(uri) {
                    releases.push((*uri).clone());
                }
            }
        }
    }
    Ok(get_many::<RawAlbum>(ExtensionKind::ALBUM_V4, &releases)
        .await?
        .iter()
        .filter_map(|album| serde_json::from_value(album_json(album)).ok())
        .collect())
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

/// An artist's name and pictures, in the Web API's shape. The session
/// has no genres or follower count (the overview has followers).
pub async fn artist_profile(id: &str) -> Result<Artist> {
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))?;
    let raw: RawArtist = get_many(ExtensionKind::ARTIST_V4, &[uri])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("artist not found"))?;
    Ok(serde_json::from_value(artist_json(&raw))?)
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

/// What an artist page needs beyond the Web API, which no longer lists an
/// artist's top tracks or related artists for third-party apps.
#[derive(Clone, Debug, Default)]
pub struct ArtistExtras {
    pub top_tracks: Vec<Track>,
    pub appears_on: Vec<Album>,
    /// Albums, singles and compilations, newest first.
    pub discography: Vec<Album>,
    pub related: Vec<Artist>,
    pub biography: Option<String>,
}

impl ArtistExtras {
    /// The Web API's shapes, for the copy kept on disk.
    pub fn to_json(&self) -> Value {
        json!({
            "top_tracks": self.top_tracks.iter().map(Track::to_api_json).collect::<Vec<_>>(),
            "appears_on": self.appears_on.iter().map(Album::to_api_json).collect::<Vec<_>>(),
            "discography": self.discography.iter().map(Album::to_api_json).collect::<Vec<_>>(),
            "related": self.related.iter().map(Artist::to_api_json).collect::<Vec<_>>(),
            "biography": self.biography,
        })
    }

    pub fn from_json(value: &Value) -> Option<ArtistExtras> {
        fn list<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Option<Vec<T>> {
            serde_json::from_value(value.get(key)?.clone()).ok()
        }
        Some(ArtistExtras {
            top_tracks: list(value, "top_tracks")?,
            appears_on: list(value, "appears_on")?,
            discography: list(value, "discography")?,
            related: list(value, "related")?,
            biography: value["biography"].as_str().map(str::to_string),
        })
    }
}

/// An artist's top tracks (for the account's country), the releases they
/// appear on, similar artists and their biography.
pub async fn artist(id: &str) -> Result<ArtistExtras> {
    let session = session().await?;
    let uri = SpotifyUri::from_uri(&format!("spotify:artist:{id}"))?;
    let raw: RawArtist = get_many(ExtensionKind::ARTIST_V4, &[uri])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("artist not found"))?;

    let country = session.country();
    let top = raw
        .top_tracks
        .iter()
        .find(|t| t.country == country)
        .or_else(|| raw.top_tracks.first());
    let top_uris: Vec<&SpotifyUri> = top.map(|t| t.tracks.iter().collect()).unwrap_or_default();
    let top_tracks: Vec<Track> = tracks(&top_uris)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();

    let appears: Vec<SpotifyUri> = raw
        .appears_on_albums
        .iter()
        .filter_map(|group| group.0.first().cloned())
        .take(16)
        .collect();
    let appears_on = get_many::<RawAlbum>(ExtensionKind::ALBUM_V4, &appears)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|album| serde_json::from_value(album_json(album)).ok())
        .collect();

    // Each group is one release in its market variants; the first will do.
    let releases: Vec<SpotifyUri> = [&raw.albums, &raw.singles, &raw.compilations]
        .into_iter()
        .flat_map(|groups| groups.iter().filter_map(|g| g.0.first().cloned()).take(40))
        .collect();
    let mut discography: Vec<Album> = get_many::<RawAlbum>(ExtensionKind::ALBUM_V4, &releases)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|album| serde_json::from_value(album_json(album)).ok())
        .collect();
    discography.sort_by(|a: &Album, b: &Album| b.release_date.cmp(&a.release_date));

    let related_uris: Vec<SpotifyUri> = raw.related.iter().take(12).map(|a| a.id.clone()).collect();
    let related = get_many::<RawArtist>(ExtensionKind::ARTIST_V4, &related_uris)
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|artist| serde_json::from_value(artist_json(artist)).ok())
        .collect();

    let biography = raw
        .biographies
        .iter()
        .map(|b| strip_tags(&b.text))
        .find(|text| !text.trim().is_empty());

    Ok(ArtistExtras {
        top_tracks,
        appears_on,
        discography,
        related,
        biography,
    })
}

/// Who made a track: performers, writers, producers.
#[derive(Clone, Debug, Default)]
pub struct Credits {
    pub roles: Vec<CreditRole>,
    /// Where the information comes from (labels, publishers).
    pub sources: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CreditRole {
    pub title: String,
    pub people: Vec<CreditPerson>,
}

#[derive(Clone, Debug, Default)]
pub struct CreditPerson {
    pub name: String,
    /// Set when the person is an artist on Spotify, for a link.
    pub artist_id: Option<String>,
    pub subroles: String,
}

/// A track's credits, as the official apps show them. Best effort: the
/// endpoint is Spotify's own and may refuse.
pub async fn credits(track_id: &str) -> Result<Credits> {
    let session = session().await?;
    let endpoint = format!("/track-credits-view/v0/experimental/{track_id}/credits");
    let bytes = session
        .spclient()
        .request_as_json(&reqwest::Method::GET, &endpoint, None, None)
        .await?;
    let value: Value = serde_json::from_slice(&bytes)?;
    let roles = value
        .get("roleCredits")
        .and_then(Value::as_array)
        .map(|roles| {
            roles
                .iter()
                .map(|role| CreditRole {
                    title: role
                        .get("roleTitle")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    people: role
                        .get("artists")
                        .and_then(Value::as_array)
                        .map(|people| {
                            people
                                .iter()
                                .map(|p| CreditPerson {
                                    name: p
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    artist_id: p
                                        .get("uri")
                                        .and_then(Value::as_str)
                                        .and_then(|u| u.strip_prefix("spotify:artist:"))
                                        .map(str::to_string),
                                    subroles: p
                                        .get("subroles")
                                        .and_then(Value::as_array)
                                        .map(|s| {
                                            s.iter()
                                                .filter_map(Value::as_str)
                                                .collect::<Vec<_>>()
                                                .join(", ")
                                        })
                                        .unwrap_or_default(),
                                })
                                .filter(|p| !p.name.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .filter(|r| !r.people.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let sources = value
        .get("sourceNames")
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(Credits { roles, sources })
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

fn artist_json(artist: &RawArtist) -> Value {
    let images = if artist.portrait_group.is_empty() {
        &artist.portraits
    } else {
        &artist.portrait_group
    };
    json!({
        "id": base62(&artist.id),
        "uri": artist.id.to_uri().unwrap_or_default(),
        "name": artist.name,
        "images": images_json(images),
    })
}

/// Biographies come with links as HTML and characters escaped as HTML
/// entities (`&amp;`, `&#34;`, `&#xff1a;`).
pub fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out)
}

pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let decoded = tail.find(';').filter(|end| *end <= 12).and_then(|end| {
            let name = &tail[1..end];
            let c = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                Some(match name {
                    "amp" => '&',
                    "quot" => '"',
                    "apos" => '\'',
                    "lt" => '<',
                    "gt" => '>',
                    "nbsp" => ' ',
                    "hellip" => '…',
                    "mdash" => '—',
                    "ndash" => '–',
                    "lsquo" => '‘',
                    "rsquo" => '’',
                    "ldquo" => '“',
                    "rdquo" => '”',
                    _ => return None,
                })
            }?;
            Some((c, end + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &tail[used..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_artist_overview() {
        let artist = json!({
            "stats": { "monthlyListeners": 4200000, "followers": 900000, "worldRank": 0 },
            "profile": {
                "biography": { "text": "Formed in <a href=\"x\">2017</a> &amp; more" },
                "externalLinks": { "items": [ { "name": "INSTAGRAM", "url": "https://instagram.com/x" } ] }
            },
            "headerImage": { "data": { "sources": [ { "url": "https://i.scdn.co/image/h" } ] } },
            "goods": { "concerts": { "items": [
                { "data": { "title": "Tour", "startDateIsoString": "2027-03-01T19:00:00Z",
                            "location": { "name": "Hall", "city": "Tokyo" } } }
            ] } },
            "relatedContent": { "discoveredOnV2": { "items": [
                { "data": { "__typename": "GenericError" } },
                { "data": { "__typename": "Playlist", "id": "p1", "name": "This Is X" } }
            ] } }
        });
        let o = parse_overview(&artist);
        assert_eq!(o.monthly_listeners, Some(4_200_000));
        assert_eq!(o.world_rank, None);
        assert_eq!(o.biography.as_deref(), Some("Formed in 2017 & more"));
        assert_eq!(o.links[0].0, "INSTAGRAM");
        assert_eq!(o.header_image.as_deref(), Some("https://i.scdn.co/image/h"));
        assert_eq!(o.concerts[0].city, "Tokyo");
        assert_eq!(o.concerts[0].venue, "Hall");
        assert_eq!(o.discovered_on.len(), 1);
        assert!(parse_overview(&json!({})).concerts.is_empty());
        assert_eq!(
            strip_tags("&#34;Tokyo Ghoul&#xff1a;re&#34; &amp; <b>more</b> &unknown; a&b"),
            "\"Tokyo Ghoul：re\" & more &unknown; a&b"
        );
    }
}
