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
const DONE_MESSAGE: &str = "You are logged in to Lilac. You can close this tab.";

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

/// A sign-in waiting for the user to approve `client` at [`Login::url`].
///
/// The authorization code flow with PKCE, run by Lilac itself rather than
/// librespot-oauth: its loopback listener takes the first request that
/// arrives as the redirect, and browsers often ask for `/favicon.ico`
/// first, which failed the login and closed the port before the real
/// redirect came. This one answers stray requests and keeps waiting.
/// [`cancel_login`] ends it early.
///
/// Lilac does not open the URL itself: the browser logged in to Spotify is
/// not always the default one, so the user opens or copies it.
pub struct Login {
    pub url: String,
    client_id: &'static str,
    verifier: String,
    state: String,
    listener: std::net::TcpListener,
}

impl Login {
    pub fn start(client: &Client) -> Result<Self> {
        let verifier = random_token(48);
        let state = random_token(16);
        let challenge = {
            use sha2::Digest as _;
            base64_url(&sha2::Sha256::digest(verifier.as_bytes()))
        };
        let url = format!(
            "https://accounts.spotify.com/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={challenge}&state={state}",
            client.id,
            super::enc(REDIRECT_URI),
            super::enc(&client.scopes.join(" ")),
        );
        let listener = std::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT)).with_context(
            || format!("port {REDIRECT_PORT} is in use; is another Lilac signing in?"),
        )?;
        Ok(Self {
            url,
            client_id: client.id,
            verifier,
            state,
            listener,
        })
    }

    /// Wait for the approval and trade its code for tokens.
    pub async fn finish(self) -> Result<Credentials> {
        let Self {
            client_id,
            verifier,
            state,
            listener,
            ..
        } = self;
        let code = tokio::task::spawn_blocking(move || wait_for_code(listener, &state))
            .await
            .map_err(|e| anyhow!("login task failed: {e}"))??;
        exchange(client_id, &code, &verifier).await
    }
}

async fn exchange(client_id: &str, code: &str, verifier: &str) -> Result<Credentials> {
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={verifier}",
        super::enc(code),
        super::enc(REDIRECT_URI),
        client_id,
    );
    let response = super::http()
        .post("https://accounts.spotify.com/api/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("could not reach Spotify to finish signing in")?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("Spotify did not accept the sign-in ({status}): {text}");
    }
    #[derive(Deserialize)]
    struct Token {
        access_token: String,
        #[serde(default)]
        refresh_token: String,
        #[serde(default)]
        expires_in: u64,
    }
    let token: Token = response.json().await?;
    Ok(Credentials {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at: now() + token.expires_in.max(60),
        client_id: client_id.to_string(),
    })
}

/// Serve the loopback port until the redirect with a code (or an error, or
/// a cancel) arrives; anything else gets a 404 and is ignored.
fn wait_for_code(listener: std::net::TcpListener, state: &str) -> Result<String> {
    use std::io::{BufRead as _, BufReader, Write as _};
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        // "GET /login?code=…&state=… HTTP/1.1"
        let target = line.split_whitespace().nth(1).unwrap_or_default();
        let Some(query) = target.strip_prefix("/login?") else {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            continue;
        };
        let param = |name: &str| {
            query.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                (key == name).then(|| decode(value))
            })
        };
        let (status, message, result) = match (param("code"), param("error")) {
            (_, Some(error)) => (
                "400 Bad Request",
                "Signing in was cancelled. You can close this tab.",
                Err(anyhow!("login failed: {error}")),
            ),
            (Some(code), None) if param("state").as_deref() == Some(state) => {
                ("200 OK", DONE_MESSAGE, Ok(code))
            }
            _ => {
                // Not ours (a stale tab, a forged request): keep waiting.
                let _ = stream.write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
        };
        let page = format!(
            "<!doctype html><meta charset=utf-8><title>Lilac</title><body style=\"font-family:sans-serif;background:#141018;color:#eee;display:grid;place-items:center;height:100vh;margin:0\"><p>{message}</p>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        return result;
    }
    Err(anyhow!("login failed: the sign-in listener stopped"))
}

/// Percent-decoding for the redirect's query values.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, byte) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn random_token(bytes: usize) -> String {
    use rand::RngCore as _;
    let mut raw = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut raw);
    base64_url(&raw)
}

fn base64_url(raw: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
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
    fn decodes_redirect_values() {
        assert_eq!(decode("AQB%2Fx-y_z"), "AQB/x-y_z");
        assert_eq!(decode("a+b%20c"), "a b c");
        assert_eq!(decode("bad%2"), "bad%2");
    }

    #[test]
    fn ignores_stray_requests_until_the_redirect() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let waiter = std::thread::spawn(move || wait_for_code(listener, "s1"));
        let mut favicon = std::net::TcpStream::connect(addr).unwrap();
        favicon
            .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        let _ = favicon.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 404"));
        let mut redirect = std::net::TcpStream::connect(addr).unwrap();
        redirect
            .write_all(b"GET /login?code=abc%2F1&state=s1 HTTP/1.1\r\n\r\n")
            .unwrap();
        assert_eq!(waiter.join().unwrap().unwrap(), "abc/1");
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
