//! Friend activity: what the people you follow are listening to.
//!
//! There is no Web API for this. The desktop client reads spclient's
//! `presence-view` endpoint, which is undocumented and may refuse tokens
//! that were not issued to an official client; callers treat a refusal as
//! "unavailable", not as an error.

use anyhow::{Result, bail};
use serde::Deserialize;

use super::api::Api;
use super::http;

const URL: &str = "https://spclient.wg.spotify.com/presence-view/v1/buddylist";

#[derive(Clone, Debug, PartialEq)]
pub struct Friend {
    pub name: String,
    pub user_uri: String,
    pub image: Option<String>,
    pub track: String,
    pub track_uri: String,
    pub artist: String,
    pub artist_uri: String,
    /// Playlist, album or artist they are playing from.
    pub context: String,
    pub context_uri: String,
    /// Unix milliseconds of the last play.
    pub timestamp_ms: u64,
}

/// The endpoint answered, but not for us.
#[derive(Debug)]
pub struct Unavailable;

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("friend activity is not available for this login")
    }
}

impl std::error::Error for Unavailable {}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    friends: Vec<RawFriend>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFriend {
    #[serde(default)]
    timestamp: u64,
    user: RawUser,
    track: RawTrack,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    image_url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawRef {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct RawTrack {
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: RawRef,
    #[serde(default)]
    album: RawRef,
    #[serde(default)]
    context: Option<RawRef>,
}

pub async fn fetch(api: &Api) -> Result<Vec<Friend>> {
    if api.is_demo() {
        return Ok(super::demo::friends());
    }
    // As the desktop client asks: through the streaming session, whose
    // login Spotify serves this to.
    // When that is refused or not connected yet, Lilac's own login is
    // tried; its answer tells a refusal (Unavailable) from an outage.
    match super::streaming::buddylist().await {
        Ok(body) => return parse(&body),
        Err(err) if err.downcast_ref::<super::streaming::NoSession>().is_some() => return Err(err),
        Err(_) => {}
    }
    let token = api.auth.token().await?;
    let response = http()
        .get(URL)
        .bearer_auth(token)
        .header("Accept", "application/json")
        .send()
        .await?;
    let status = response.status().as_u16();
    if matches!(status, 400 | 401 | 403 | 404 | 405 | 410) {
        bail!(Unavailable);
    }
    if !response.status().is_success() {
        bail!("Spotify answered with an error ({status})");
    }
    parse(&response.bytes().await?)
}

fn parse(body: &[u8]) -> Result<Vec<Friend>> {
    let response: Response = serde_json::from_slice(body)?;
    let mut friends: Vec<Friend> = response
        .friends
        .into_iter()
        .map(|f| {
            // Without a context the album is what they played from.
            let context = f.track.context.unwrap_or_default();
            let (context, context_uri) = if context.uri.is_empty() {
                (f.track.album.name, f.track.album.uri)
            } else {
                (context.name, context.uri)
            };
            Friend {
                name: f.user.name,
                user_uri: f.user.uri,
                image: f.user.image_url.filter(|u| !u.is_empty()),
                track: f.track.name,
                track_uri: f.track.uri,
                artist: f.track.artist.name,
                artist_uri: f.track.artist.uri,
                context,
                context_uri,
                timestamp_ms: f.timestamp,
            }
        })
        .collect();
    friends.sort_by_key(|f| std::cmp::Reverse(f.timestamp_ms));
    Ok(friends)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_buddylist() {
        let body = br#"{"friends":[
            {"timestamp":1000,"user":{"uri":"spotify:user:a","name":"Ana"},
             "track":{"uri":"spotify:track:1","name":"Song","imageUrl":"x",
                      "album":{"uri":"spotify:album:2","name":"Album"},
                      "artist":{"uri":"spotify:artist:3","name":"Artist"}}},
            {"timestamp":2000,"user":{"uri":"spotify:user:b","name":"Bo","imageUrl":"http://i"},
             "track":{"uri":"spotify:track:4","name":"Other",
                      "album":{"uri":"spotify:album:5","name":"Record"},
                      "artist":{"uri":"spotify:artist:6","name":"Band"},
                      "context":{"uri":"spotify:playlist:7","name":"Mix","index":0}}}
        ]}"#;
        let friends = parse(body).unwrap();
        assert_eq!(friends.len(), 2);
        // Most recent first.
        assert_eq!(friends[0].name, "Bo");
        assert_eq!(friends[0].context, "Mix");
        assert_eq!(friends[0].image.as_deref(), Some("http://i"));
        assert_eq!(friends[1].context_uri, "spotify:album:2");
        assert_eq!(friends[1].image, None);
    }

    #[test]
    fn empty_list() {
        assert!(parse(b"{}").unwrap().is_empty());
    }
}
