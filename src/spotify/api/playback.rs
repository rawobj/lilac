//! Spotify Connect: control playback on other devices.
//!
//! What plays where is pushed to us through the Connect cluster (see
//! `player::backend`); these calls only send commands.

use anyhow::Result;
use reqwest::Method;
use serde_json::{Value, json};

use super::super::enc;
use super::Api;
use crate::model::*;

impl Api {
    pub async fn remote_queue(&self) -> Result<RemoteQueue> {
        self.get("/me/player/queue").await
    }

    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        let body = json!({ "device_ids": [device_id], "play": play });
        self.call(Method::PUT, "/me/player", Some(body)).await
    }

    /// Start a context (album, playlist, artist) or an explicit list of track URIs.
    pub async fn remote_play(
        &self,
        device_id: Option<&str>,
        context_uri: Option<&str>,
        uris: Option<&[String]>,
        offset_uri: Option<&str>,
        position_ms: u32,
    ) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(context) = context_uri {
            body.insert("context_uri".into(), json!(context));
            if let Some(offset) = offset_uri {
                body.insert("offset".into(), json!({ "uri": offset }));
            }
        } else if let Some(uris) = uris {
            body.insert("uris".into(), json!(uris));
            if let Some(offset) = offset_uri {
                body.insert("offset".into(), json!({ "uri": offset }));
            }
        }
        if position_ms > 0 {
            body.insert("position_ms".into(), json!(position_ms));
        }
        let body = if body.is_empty() {
            None
        } else {
            Some(Value::Object(body))
        };
        self.call(
            Method::PUT,
            &with_device("/me/player/play", device_id),
            body,
        )
        .await
    }

    pub async fn remote_pause(&self) -> Result<()> {
        self.call(Method::PUT, "/me/player/pause", None).await
    }

    pub async fn remote_next(&self) -> Result<()> {
        self.call(Method::POST, "/me/player/next", None).await
    }

    pub async fn remote_previous(&self) -> Result<()> {
        self.call(Method::POST, "/me/player/previous", None).await
    }

    pub async fn remote_seek(&self, position_ms: u32) -> Result<()> {
        self.call(
            Method::PUT,
            &format!("/me/player/seek?position_ms={position_ms}"),
            None,
        )
        .await
    }

    pub async fn remote_volume(&self, percent: u32) -> Result<()> {
        let percent = percent.min(100);
        self.call(
            Method::PUT,
            &format!("/me/player/volume?volume_percent={percent}"),
            None,
        )
        .await
    }

    pub async fn remote_shuffle(&self, on: bool) -> Result<()> {
        self.call(Method::PUT, &format!("/me/player/shuffle?state={on}"), None)
            .await
    }

    /// `state` is one of `off`, `context`, `track`.
    pub async fn remote_repeat(&self, state: &str) -> Result<()> {
        self.call(
            Method::PUT,
            &format!("/me/player/repeat?state={state}"),
            None,
        )
        .await
    }

    pub async fn remote_enqueue(&self, uri: &str) -> Result<()> {
        self.call(
            Method::POST,
            &format!("/me/player/queue?uri={}", enc(uri)),
            None,
        )
        .await
    }
}

fn with_device(path: &str, device_id: Option<&str>) -> String {
    match device_id {
        Some(id) => format!("{path}?device_id={}", enc(id)),
        None => path.to_string(),
    }
}
