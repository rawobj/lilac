//! Domain types shared by the API layer, the player and the views.
//!
//! These deserialize straight from Spotify's JSON. Strings are `SharedString`
//! so cloning a track into the queue or a list row is a refcount bump.

use std::sync::Arc;

use gpui_kit::SharedString;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// Artwork in up to three sizes. Spotify sends an array sorted large to small;
/// we keep the URLs we actually render and drop the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Images {
    pub small: Option<SharedString>,
    pub medium: Option<SharedString>,
    pub large: Option<SharedString>,
}

impl Images {
    /// Best URL for a square rendered at `px` logical pixels on the
    /// current display. Spotify's sizes are 64, 300 and 640 pixels; each is
    /// used only up to a little below its size, since the small ones are
    /// heavily compressed and look soft when stretched.
    pub fn for_size(&self, px: u32) -> Option<SharedString> {
        let device = px as f32 * display_scale();
        let pick = if device <= 48.0 {
            [&self.small, &self.medium, &self.large]
        } else if device <= 240.0 {
            [&self.medium, &self.large, &self.small]
        } else {
            [&self.large, &self.medium, &self.small]
        };
        pick.into_iter().find_map(|u| u.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.small.is_none() && self.medium.is_none() && self.large.is_none()
    }

    /// The Web API's shape, readable by the deserializer above.
    pub fn to_api_json(&self) -> Value {
        let sized = [(&self.small, 64), (&self.medium, 300), (&self.large, 640)];
        sized
            .into_iter()
            .filter_map(|(url, width)| {
                Some(json!({ "url": url.as_ref()?.as_ref(), "width": width }))
            })
            .collect()
    }
}

/// The window's scale factor, as hundredths, for picking image sizes.
static DISPLAY_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(100);

pub fn set_display_scale(scale: f32) {
    DISPLAY_SCALE.store(
        (scale * 100.0).round() as u32,
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn display_scale() -> f32 {
    DISPLAY_SCALE.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.0
}

#[derive(Deserialize)]
struct RawImage {
    url: Option<String>,
    #[serde(default)]
    width: Option<u32>,
}

impl<'de> Deserialize<'de> for Images {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw: Option<Vec<RawImage>> = Option::deserialize(d)?;
        let mut raw: Vec<(u32, SharedString)> = raw
            .unwrap_or_default()
            .into_iter()
            .filter_map(|i| Some((i.width.unwrap_or(0), SharedString::from(i.url?))))
            .collect();
        if raw.is_empty() {
            return Ok(Images::default());
        }
        // Unknown widths (user-uploaded covers) sort as "large".
        raw.sort_by_key(|(w, _)| if *w == 0 { u32::MAX } else { *w });
        let small = raw.first().map(|(_, u)| u.clone());
        let large = raw.last().map(|(_, u)| u.clone());
        let medium = raw
            .iter()
            .find(|(w, _)| *w >= 250 || *w == 0)
            .map(|(_, u)| u.clone())
            .or_else(|| large.clone());
        Ok(Images {
            small,
            medium,
            large,
        })
    }
}

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ArtistRef {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct AlbumRef {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default)]
    pub images: Images,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    #[default]
    Track,
    Episode,
    #[serde(other)]
    Other,
}

/// A playable item: a music track or a podcast episode.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Track {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub uri: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: Option<AlbumRef>,
    #[serde(default, deserialize_with = "null_default")]
    pub duration_ms: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub explicit: bool,
    #[serde(default, deserialize_with = "null_default")]
    pub track_number: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub disc_number: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub is_local: bool,
    #[serde(default)]
    pub is_playable: Option<bool>,
    #[serde(default, rename = "type")]
    pub kind: ItemKind,

    // Episode-only fields.
    #[serde(default)]
    pub show: Option<ShowRef>,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub description: Option<SharedString>,
    #[serde(default)]
    pub release_date: Option<SharedString>,
}

impl Track {
    /// The Web API's shape, readable by `Track`'s deserializer: enough for
    /// rows and the player bar, for what Lilac keeps on disk.
    pub fn to_api_json(&self) -> Value {
        let artists: Vec<_> = self
            .artists
            .iter()
            .map(|a| json!({ "id": a.id.as_ref(), "name": a.name.as_ref() }))
            .collect();
        json!({
            "id": self.id.as_ref(),
            "uri": self.uri.as_ref(),
            "name": self.name.as_ref(),
            "type": if self.kind == ItemKind::Episode { "episode" } else { "track" },
            "duration_ms": self.duration_ms,
            "explicit": self.explicit,
            "is_local": self.is_local,
            "is_playable": self.is_playable,
            "artists": artists,
            "album": self.album.as_ref().map(|a| json!({
                "id": a.id.as_ref(),
                "name": a.name.as_ref(),
                "images": a.images.to_api_json(),
            })),
            "show": self.show.as_ref().map(|s| json!({
                "id": s.id.as_ref(),
                "name": s.name.as_ref(),
                "images": s.images.to_api_json(),
            })),
            "images": self.images.to_api_json(),
        })
    }

    pub fn artist_line(&self) -> SharedString {
        if self.kind == ItemKind::Episode {
            return self
                .show
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default();
        }
        match self.artists.len() {
            0 => SharedString::default(),
            1 => self.artists[0].name.clone(),
            _ => {
                let mut out = String::new();
                for (ix, a) in self.artists.iter().enumerate() {
                    if ix > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&a.name);
                }
                out.into()
            }
        }
    }

    pub fn cover(&self) -> &Images {
        match &self.album {
            Some(album) if !album.images.is_empty() => &album.images,
            _ => match &self.show {
                Some(show) if self.images.is_empty() => &show.images,
                _ => &self.images,
            },
        }
    }

    pub fn playable(&self) -> bool {
        !self.is_local && self.is_playable.unwrap_or(true) && !self.id.is_empty()
    }

    pub fn is_episode(&self) -> bool {
        self.kind == ItemKind::Episode
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Album {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub uri: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub images: Images,
    #[serde(default, deserialize_with = "null_default")]
    pub release_date: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub total_tracks: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub album_type: SharedString,
    #[serde(default)]
    pub copyrights: Vec<Copyright>,
    /// First page of tracks, present on the full album object.
    #[serde(default)]
    pub tracks: Option<Page<Track>>,
}

impl Album {
    /// The Web API's shape, readable by `Album`'s deserializer: what library
    /// lists show, for the copy Lilac keeps on disk.
    pub fn to_api_json(&self) -> Value {
        let artists: Vec<_> = self
            .artists
            .iter()
            .map(|a| json!({ "id": a.id.as_ref(), "name": a.name.as_ref() }))
            .collect();
        json!({
            "id": self.id.as_ref(),
            "uri": self.uri.as_ref(),
            "name": self.name.as_ref(),
            "artists": artists,
            "images": self.images.to_api_json(),
            "release_date": self.release_date.as_ref(),
            "total_tracks": self.total_tracks,
            "album_type": self.album_type.as_ref(),
            "copyrights": self.copyrights.iter().map(|c| json!({ "text": c.text.as_ref() })).collect::<Vec<_>>(),
        })
    }

    pub fn year(&self) -> &str {
        self.release_date.get(..4).unwrap_or("")
    }

    pub fn artist_line(&self) -> SharedString {
        let names: Vec<&str> = self.artists.iter().map(|a| a.name.as_ref()).collect();
        names.join(", ").into()
    }

    pub fn type_label(&self) -> &'static str {
        match self.album_type.as_ref() {
            "single" => "Single",
            "compilation" => "Compilation",
            "ep" => "EP",
            _ => "Album",
        }
    }

    pub fn as_ref(&self) -> AlbumRef {
        AlbumRef {
            id: self.id.clone(),
            name: self.name.clone(),
            images: self.images.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Copyright {
    #[serde(default)]
    pub text: SharedString,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Followers {
    #[serde(default, deserialize_with = "null_default")]
    pub total: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Artist {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub uri: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default)]
    pub images: Images,
    #[serde(default, deserialize_with = "null_default")]
    pub genres: Vec<SharedString>,
    #[serde(default)]
    pub followers: Option<Followers>,
}

impl Artist {
    /// The Web API's shape, readable by `Artist`'s deserializer.
    pub fn to_api_json(&self) -> Value {
        json!({
            "id": self.id.as_ref(),
            "uri": self.uri.as_ref(),
            "name": self.name.as_ref(),
            "images": self.images.to_api_json(),
            "genres": self.genres.iter().map(|g| g.as_ref()).collect::<Vec<_>>(),
            "followers": self.followers.as_ref().map(|f| json!({ "total": f.total })),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Owner {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default)]
    pub display_name: Option<SharedString>,
}

impl Owner {
    pub fn name(&self) -> SharedString {
        self.display_name.clone().unwrap_or_else(|| self.id.clone())
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct PageRef {
    #[serde(default, deserialize_with = "null_default")]
    pub total: u32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Playlist {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub uri: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default)]
    pub description: Option<SharedString>,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub owner: Owner,
    #[serde(default)]
    pub snapshot_id: Option<SharedString>,
    #[serde(default, deserialize_with = "null_default")]
    pub collaborative: bool,
    #[serde(default)]
    pub public: Option<bool>,
    /// Item count. The API renamed `tracks` to `items` in 2026; accept both.
    #[serde(default, alias = "tracks")]
    pub items: Option<PageRef>,
}

impl Playlist {
    /// The Web API's shape, readable by `Playlist`'s deserializer.
    pub fn to_api_json(&self) -> Value {
        json!({
            "id": self.id.as_ref(),
            "uri": self.uri.as_ref(),
            "name": self.name.as_ref(),
            "description": self.description.as_ref().map(|d| d.as_ref()),
            "images": self.images.to_api_json(),
            "owner": {
                "id": self.owner.id.as_ref(),
                "display_name": self.owner.display_name.as_ref().map(|d| d.as_ref()),
            },
            "snapshot_id": self.snapshot_id.as_ref().map(|s| s.as_ref()),
            "collaborative": self.collaborative,
            "public": self.public,
            "items": { "total": self.total() },
        })
    }

    pub fn total(&self) -> u32 {
        self.items.as_ref().map(|p| p.total).unwrap_or(0)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ShowRef {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default)]
    pub images: Images,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Show {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub uri: SharedString,
    #[serde(default, deserialize_with = "null_default")]
    pub name: SharedString,
    #[serde(default)]
    pub publisher: Option<SharedString>,
    #[serde(default)]
    pub description: Option<SharedString>,
    #[serde(default)]
    pub images: Images,
    #[serde(default, deserialize_with = "null_default")]
    pub total_episodes: u32,
}

impl Show {
    /// The Web API's shape, readable by `Show`'s deserializer.
    pub fn to_api_json(&self) -> Value {
        json!({
            "id": self.id.as_ref(),
            "uri": self.uri.as_ref(),
            "name": self.name.as_ref(),
            "publisher": self.publisher.as_ref().map(|p| p.as_ref()),
            "description": self.description.as_ref().map(|d| d.as_ref()),
            "images": self.images.to_api_json(),
            "total_episodes": self.total_episodes,
        })
    }

    pub fn as_ref(&self) -> ShowRef {
        ShowRef {
            id: self.id.clone(),
            name: self.name.clone(),
            images: self.images.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct User {
    #[serde(default, deserialize_with = "null_default")]
    pub id: SharedString,
    #[serde(default)]
    pub display_name: Option<SharedString>,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub product: Option<SharedString>,
    #[serde(default)]
    pub country: Option<SharedString>,
    #[serde(default)]
    pub followers: Option<Followers>,
}

impl User {
    pub fn name(&self) -> SharedString {
        self.display_name.clone().unwrap_or_else(|| self.id.clone())
    }
}

/// One page of a paginated collection.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Page<T> {
    #[serde(default = "Vec::new", deserialize_with = "skip_null_items")]
    pub items: Vec<T>,
    #[serde(default, deserialize_with = "null_default")]
    pub total: u32,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    pub offset: u32,
}

impl<T> Default for Page<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            total: 0,
            next: None,
            offset: 0,
        }
    }
}

/// Search results contain literal `null` entries for unavailable items.
fn skip_null_items<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let raw: Option<Vec<Option<T>>> = Option::deserialize(d)?;
    Ok(raw.unwrap_or_default().into_iter().flatten().collect())
}

/// Entry of a playlist or of the saved tracks list.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SavedItem {
    /// `item` since 2026, `track` before. Episodes saved to a playlist show up here too.
    #[serde(default, alias = "track", alias = "episode")]
    pub item: Option<Track>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PlayedContext {
    #[serde(default)]
    pub uri: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SavedAlbum {
    #[serde(default)]
    pub album: Album,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SavedShow {
    #[serde(default)]
    pub show: Show,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct SearchResults {
    #[serde(default)]
    pub tracks: Page<Track>,
    #[serde(default)]
    pub artists: Page<Artist>,
    #[serde(default)]
    pub albums: Page<Album>,
    #[serde(default)]
    pub playlists: Page<Playlist>,
    #[serde(default)]
    pub shows: Page<Show>,
    #[serde(default)]
    pub episodes: Page<Track>,
}

impl SearchResults {
    pub fn is_empty(&self) -> bool {
        self.tracks.items.is_empty()
            && self.artists.items.is_empty()
            && self.albums.items.is_empty()
            && self.playlists.items.is_empty()
            && self.shows.items.is_empty()
            && self.episodes.items.is_empty()
    }
}

/// What the account is playing, or last played, as the Web API reports it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemoteState {
    #[serde(default)]
    pub device: Option<RemoteDevice>,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default)]
    pub progress_ms: Option<u32>,
    #[serde(default)]
    pub item: Option<Track>,
    /// What the item plays in; null for a list of tracks.
    #[serde(default)]
    pub context: Option<PlayedContext>,
}

/// A device as the Web API lists it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemoteDevice {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_restricted: bool,
    /// Phones often refuse volume changes from other apps.
    #[serde(default = "yes")]
    pub supports_volume: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemoteDevices {
    #[serde(default)]
    pub devices: Vec<RemoteDevice>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemoteQueue {
    #[serde(default)]
    pub currently_playing: Option<Track>,
    #[serde(default, deserialize_with = "skip_null_items")]
    pub queue: Vec<Track>,
}

/// A line of time-synced lyrics.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LyricLine {
    pub start_ms: u32,
    pub text: SharedString,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lyrics {
    pub lines: Arc<[LyricLine]>,
    pub synced: bool,
    pub provider: SharedString,
    /// The lines in Latin letters, one per line, when the words are in
    /// another script and a reading is known.
    pub romanized: Option<Arc<[SharedString]>>,
}

/// Anything that can be played as a context or opened as a page.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContextUri {
    Playlist(SharedString),
    Album(SharedString),
    Artist(SharedString),
    Show(SharedString),
    Liked,
    /// Ad-hoc list (search results, top tracks) with no server-side identity.
    Tracks(SharedString),
}

pub fn format_duration(ms: u32) -> String {
    let total = ms / 1000;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "3 hr 12 min" style total, as shown under playlist titles.
pub fn format_total(ms: u64) -> String {
    let minutes = ms / 60_000;
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h} hr {m} min")
    } else {
        format!("{m} min {} sec", (ms / 1000) % 60)
    }
}

pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (ix, ch) in s.chars().enumerate() {
        if ix > 0 && (s.len() - ix).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(61_000), "1:01");
        assert_eq!(format_duration(3_723_000), "1:02:03");
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(999), "999");
    }

    #[test]
    fn images_pick_by_size() {
        let images: Images = serde_json::from_str(
            r#"[{"url":"l","width":640},{"url":"m","width":300},{"url":"s","width":64}]"#,
        )
        .unwrap();
        assert_eq!(images.for_size(40).unwrap().as_ref(), "s");
        assert_eq!(images.for_size(56).unwrap().as_ref(), "m");
        assert_eq!(images.for_size(200).unwrap().as_ref(), "m");
        assert_eq!(images.for_size(320).unwrap().as_ref(), "l");
        let none: Images = serde_json::from_str("null").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn track_tolerates_nulls() {
        let t: Track = serde_json::from_str(
            r#"{"id":null,"name":"x","artists":null,"album":null,"type":"episode"}"#,
        )
        .unwrap();
        assert_eq!(t.kind, ItemKind::Episode);
        assert!(!t.playable());
    }

    #[test]
    fn playlist_item_accepts_old_and_new_shape() {
        let old: SavedItem = serde_json::from_str(r#"{"track":{"id":"a","name":"A"}}"#).unwrap();
        let new: SavedItem = serde_json::from_str(r#"{"item":{"id":"a","name":"A"}}"#).unwrap();
        assert_eq!(old.item.unwrap().id, new.item.unwrap().id);
        let p: Playlist = serde_json::from_str(r#"{"id":"p","tracks":{"total":4}}"#).unwrap();
        assert_eq!(p.total(), 4);
    }

    #[test]
    fn page_skips_null_entries() {
        let p: Page<Track> =
            serde_json::from_str(r#"{"items":[null,{"id":"a"}],"total":2}"#).unwrap();
        assert_eq!(p.items.len(), 1);
    }
}
