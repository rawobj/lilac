//! Spotify account login.
//!
//! librespot-oauth runs the authorization code flow with PKCE: it opens the
//! browser at accounts.spotify.com and catches the redirect on a loopback
//! port. Tokens are kept in our own credential files.
//!
//! Two OAuth clients are involved. The Web API uses lilac's own client ID.
//! The streaming access point only accepts tokens issued to Spotify's
//! desktop client, so playback gets a login of its own.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use serde::{Deserialize, Serialize};

/// lilac's client ID for the Web API.
pub const CLIENT_ID: &str = "effa886c10bf45de8ca4ecdc50ca3dfb";
/// Client ID of the desktop client, the only one allowed to open streaming sessions.
pub const DESKTOP_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_PORT: u16 = 8898;
const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";

const STREAMING_SCOPES: &[&str] = &["streaming", "user-read-private"];
const WEB_SCOPES: &[&str] = &[
    "user-read-private",
    "user-read-email",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-playback-position",
    "user-read-recently-played",
    "user-top-read",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "user-follow-modify",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-public",
    "playlist-modify-private",
    "ugc-image-upload",
];

/// Shown in the browser tab once the redirect arrives.
const DONE_MESSAGE: &str = "You are logged in to lilac. You can close this tab.";

/// One OAuth client and where its tokens are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub id: &'static str,
    scopes: &'static [&'static str],
    file: PathBuf,
}

impl Client {
    /// lilac's own client, for the Web API.
    pub fn web() -> Client {
        Client {
            id: CLIENT_ID,
            scopes: WEB_SCOPES,
            file: crate::paths::credentials_file(),
        }
    }

    /// The desktop client, for the streaming session.
    pub fn streaming() -> Client {
        Client {
            id: DESKTOP_CLIENT_ID,
            scopes: STREAMING_SCOPES,
            file: crate::paths::streaming_credentials_file(),
        }
    }

    fn oauth(&self, open_browser: bool) -> Result<OAuthClient> {
        let builder = OAuthClientBuilder::new(self.id, REDIRECT_URI, self.scopes.to_vec())
            .with_custom_message(DONE_MESSAGE);
        let builder = if open_browser {
            builder.open_in_browser()
        } else {
            builder
        };
        builder.build().map_err(|e| anyhow!("{e}"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: u64,
    /// The client that issued these tokens; a refresh must use the same one.
    #[serde(default)]
    pub client_id: String,
}

impl Credentials {
    pub fn expired(&self) -> bool {
        now() + 60 >= self.expires_at
    }

    /// Stored credentials for `client`, unless they were issued to another one.
    pub fn load(client: &Client) -> Option<Self> {
        let raw = std::fs::read(&client.file).ok()?;
        let credentials: Credentials = serde_json::from_slice(&raw).ok()?;
        (credentials.client_id == client.id).then_some(credentials)
    }

    pub fn save(&self, client: &Client) -> Result<()> {
        let raw = serde_json::to_vec(self)?;
        crate::paths::write_private(&client.file, &raw).context("could not store credentials")
    }

    pub fn forget_all() {
        let _ = std::fs::remove_file(crate::paths::credentials_file());
        let _ = std::fs::remove_file(crate::paths::streaming_credentials_file());
    }

    fn from_token(token: OAuthToken, client: &Client, previous_refresh: Option<&str>) -> Self {
        let lifetime = token.expires_at.saturating_duration_since(Instant::now());
        // Spotify may omit the refresh token on refresh; the old one stays valid then.
        let refresh_token = if token.refresh_token.is_empty() {
            previous_refresh.unwrap_or_default().to_string()
        } else {
            token.refresh_token
        };
        Self {
            access_token: token.access_token,
            refresh_token,
            expires_at: now() + lifetime.as_secs(),
            client_id: client.id.to_string(),
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Open the browser and wait for the user to approve `client`.
///
/// The loopback wait blocks a thread, so it runs off the async workers.
/// [`cancel_login`] ends it early.
pub async fn login(client: &Client) -> Result<Credentials> {
    let oauth = client.oauth(true)?;
    let token = tokio::task::spawn_blocking(move || oauth.get_access_token())
        .await
        .map_err(|e| anyhow!("login task failed: {e}"))?
        .map_err(|e| anyhow!("login failed: {e}"))?;
    Ok(Credentials::from_token(token, client, None))
}

/// Abandon a login in progress by answering the loopback listener ourselves,
/// which makes it return without a code and frees the port.
pub fn cancel_login() {
    use std::io::Write;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], REDIRECT_PORT));
    if let Ok(mut stream) =
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200))
    {
        let _ = stream.write_all(b"GET /login?error=cancelled HTTP/1.1\r\n\r\n");
    }
}

pub async fn refresh(client: &Client, refresh_token: &str) -> Result<Credentials> {
    let token = client
        .oauth(false)?
        .refresh_token_async(refresh_token)
        .await
        .map_err(|e| anyhow!("could not refresh the login: {e}"))?;
    Ok(Credentials::from_token(token, client, Some(refresh_token)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_old_refresh_token_when_none_is_returned() {
        let token = OAuthToken {
            access_token: "a".into(),
            refresh_token: String::new(),
            expires_at: Instant::now() + Duration::from_secs(3600),
            token_type: "Bearer".into(),
            scopes: Vec::new(),
        };
        let client = Client::web();
        let credentials = Credentials::from_token(token, &client, Some("old"));
        assert_eq!(credentials.refresh_token, "old");
        assert_eq!(credentials.client_id, CLIENT_ID);
        assert!(!credentials.expired());
        assert!(credentials.expires_at >= now() + 3590);
    }

    #[test]
    fn streaming_and_web_are_separate_logins() {
        let web = Client::web();
        let streaming = Client::streaming();
        assert_eq!(streaming.id, DESKTOP_CLIENT_ID);
        assert_ne!(streaming.file, web.file);
        assert!(!web.scopes.contains(&"streaming"));
        assert!(streaming.scopes.contains(&"streaming"));
        assert!(web.oauth(false).is_ok());
        assert!(streaming.oauth(false).is_ok());
    }
}
