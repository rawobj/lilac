//! Spotify account login.
//!
//! librespot-oauth runs the authorization code flow with PKCE: it opens the
//! browser at accounts.spotify.com and catches the redirect on a loopback
//! port. The tokens are for the Web API and are kept in our own
//! credential file.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use librespot_oauth::{OAuthClient, OAuthClientBuilder, OAuthToken};
use serde::{Deserialize, Serialize};

/// lilac's client ID for the Web API.
pub const CLIENT_ID: &str = "effa886c10bf45de8ca4ecdc50ca3dfb";
const REDIRECT_PORT: u16 = 8898;

const SCOPES: &[&str] = &[
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

fn redirect_uri() -> String {
    format!("http://127.0.0.1:{REDIRECT_PORT}/login")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

impl Credentials {
    pub fn expired(&self) -> bool {
        now() + 60 >= self.expires_at
    }

    pub fn load() -> Option<Self> {
        let raw = std::fs::read(crate::paths::credentials_file()).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    pub fn save(&self) -> Result<()> {
        let raw = serde_json::to_vec(self)?;
        crate::paths::write_private(&crate::paths::credentials_file(), &raw)
            .context("could not store credentials")
    }

    pub fn forget() {
        let _ = std::fs::remove_file(crate::paths::credentials_file());
    }

    fn from_token(token: OAuthToken, previous_refresh: Option<&str>) -> Self {
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
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn client(open_browser: bool) -> Result<OAuthClient> {
    let builder = OAuthClientBuilder::new(CLIENT_ID, &redirect_uri(), SCOPES.to_vec())
        .with_custom_message(DONE_MESSAGE);
    let builder = if open_browser {
        builder.open_in_browser()
    } else {
        builder
    };
    builder.build().map_err(|e| anyhow!("{e}"))
}

/// Open the browser and wait for the user to approve.
///
/// The loopback wait blocks a thread, so it runs off the async workers.
/// [`cancel_login`] ends it early.
pub async fn login() -> Result<Credentials> {
    let client = client(true)?;
    let token = tokio::task::spawn_blocking(move || client.get_access_token())
        .await
        .map_err(|e| anyhow!("login task failed: {e}"))?
        .map_err(|e| anyhow!("login failed: {e}"))?;
    Ok(Credentials::from_token(token, None))
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

pub async fn refresh(refresh_token: &str) -> Result<Credentials> {
    let token = client(false)?
        .refresh_token_async(refresh_token)
        .await
        .map_err(|e| anyhow!("could not refresh the login: {e}"))?;
    Ok(Credentials::from_token(token, Some(refresh_token)))
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
        let credentials = Credentials::from_token(token, Some("old"));
        assert_eq!(credentials.refresh_token, "old");
        assert!(!credentials.expired());
        assert!(credentials.expires_at >= now() + 3590);
    }

    #[test]
    fn client_builds_with_loopback_redirect() {
        assert!(client(false).is_ok());
        assert_eq!(redirect_uri(), "http://127.0.0.1:8898/login");
    }
}
