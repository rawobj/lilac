//! Spotify Web API client.
//!
//! Thin, typed wrappers over `api.spotify.com/v1`. Tokens are refreshed
//! transparently and rate limits are honoured with a bounded retry.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;

use super::http;
use super::oauth::{self, Client, Credentials};
use crate::model::User;

mod catalog;
mod library;
mod playback;

const BASE: &str = "https://api.spotify.com/v1";
const MAX_RETRIES: u32 = 3;

/// Error returned by the Web API with its HTTP status.
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Spotify API error {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

/// HTTP status of an error produced by [`Api`], if it came from the server.
pub fn status_of(err: &anyhow::Error) -> Option<u16> {
    err.downcast_ref::<ApiError>().map(|e| e.status)
}

/// True when the endpoint is gone or forbidden for this client, as opposed to a transient failure.
pub(super) fn is_unsupported(err: &anyhow::Error) -> bool {
    matches!(status_of(err), Some(400 | 403 | 404 | 405 | 410))
}

/// Holds one client's OAuth credentials and refreshes them on demand.
pub struct Auth {
    pub client: Client,
    credentials: Mutex<Option<Credentials>>,
}

impl Auth {
    /// Starts with whatever this client has stored.
    pub fn load(client: Client) -> Self {
        let credentials = Credentials::load(&client);
        Self {
            client,
            credentials: Mutex::new(credentials),
        }
    }

    pub fn has_credentials(&self) -> bool {
        self.credentials.try_lock().is_ok_and(|c| c.is_some())
    }

    pub async fn set(&self, credentials: Option<Credentials>) {
        *self.credentials.lock().await = credentials;
    }

    /// A valid access token, refreshing first if the current one is about to expire.
    pub async fn token(&self) -> Result<String> {
        let mut guard = self.credentials.lock().await;
        let current = guard.as_ref().ok_or_else(|| anyhow!("not logged in"))?;
        if !current.expired() {
            return Ok(current.access_token.clone());
        }
        let fresh = oauth::refresh(&self.client, &current.refresh_token).await?;
        let _ = fresh.save(&self.client);
        let token = fresh.access_token.clone();
        *guard = Some(fresh);
        Ok(token)
    }

    /// Drop the cached access token so the next call refreshes it.
    async fn invalidate(&self) {
        if let Some(credentials) = self.credentials.lock().await.as_mut() {
            credentials.expires_at = 0;
        }
    }
}

#[derive(Clone)]
pub struct Api {
    pub auth: Arc<Auth>,
    /// Serve everything from the bundled demo catalog instead of the network.
    demo: bool,
}

impl Api {
    pub fn new(auth: Arc<Auth>) -> Self {
        Self { auth, demo: false }
    }

    pub fn demo() -> Self {
        Self {
            auth: Arc::new(Auth {
                client: Client::web(),
                credentials: Mutex::new(None),
            }),
            demo: true,
        }
    }

    pub fn is_demo(&self) -> bool {
        self.demo
    }

    pub(super) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Option<String>> {
        if self.demo {
            if method != Method::GET {
                return Ok(None);
            }
            return match super::demo::get(path) {
                Some(value) => Ok(Some(value.to_string())),
                None => Err(ApiError {
                    status: 404,
                    message: "not in the demo catalog".into(),
                }
                .into()),
            };
        }

        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{BASE}{path}")
        };
        let mut attempt = 0;
        loop {
            attempt += 1;
            let token = self.auth.token().await?;
            let mut request = http().request(method.clone(), &url).bearer_auth(token);
            request = match body {
                Some(body) => request.json(body),
                // Spotify rejects bodyless PUT/POST without an explicit length.
                None if method != Method::GET => request.header("Content-Length", "0"),
                None => request,
            };

            let response = match request.send().await {
                Ok(response) => response,
                Err(err) if attempt < MAX_RETRIES && (err.is_timeout() || err.is_connect()) => {
                    tokio::time::sleep(Duration::from_millis(400 * attempt as u64)).await;
                    continue;
                }
                Err(err) => return Err(err).context("network error"),
            };

            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && attempt < MAX_RETRIES {
                self.auth.invalidate().await;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1);
                // A long penalty means the quota is spent; waiting would only freeze the UI.
                if wait <= 5 && attempt < MAX_RETRIES {
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                    continue;
                }
                return Err(ApiError {
                    status: 429,
                    message: format!("Spotify is limiting requests right now; try again in {wait} s."),
                }
                .into());
            }
            if status.is_server_error() && attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
                continue;
            }

            let text = response.text().await.unwrap_or_default();
            if !status.is_success() {
                let message = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| {
                        status
                            .canonical_reason()
                            .unwrap_or("request failed")
                            .to_owned()
                    });
                return Err(ApiError {
                    status: status.as_u16(),
                    message,
                }
                .into());
            }
            return Ok(if text.is_empty() { None } else { Some(text) });
        }
    }

    pub(super) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let text = self
            .send(Method::GET, path, None)
            .await?
            .ok_or_else(|| ApiError {
                status: 204,
                message: "empty response".into(),
            })?;
        serde_json::from_str(&text).with_context(|| format!("unexpected response from {path}"))
    }

    pub(super) async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<()> {
        self.send(method, path, body.as_ref()).await.map(|_| ())
    }

    // ---------------------------------------------------------------- profile

    pub async fn me(&self) -> Result<User> {
        self.get("/me").await
    }
}
