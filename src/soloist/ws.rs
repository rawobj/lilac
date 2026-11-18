//! Soloist's local WebSocket API: commands out, events in.

use anyhow::{Context as _, Result};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// What Lilac uses of the events Soloist pushes.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Auth {
        logged_in: bool,
        is_active: bool,
    },
    /// Paused, playing, buffering or idle.
    Status(String),
    /// Everything about what plays, sent on every change.
    State(Box<State>),
    /// Where playback is, sent on starts, seeks and pauses.
    Position(Position),
    /// What plays next (queued, then the rest of the context), as Web API
    /// tracks. Broadcasts carry 10; `get_queue` answers with as many as asked.
    Queue(Vec<Value>),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub status: String,
    /// The track in the Web API's JSON shape, ready to deserialize.
    pub track: Option<Value>,
    pub context_uri: Option<String>,
    pub position: Position,
    pub shuffle: bool,
    pub repeat: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Position {
    pub position_ms: u64,
    /// Unix milliseconds the position was taken at.
    pub timestamp_ms: u64,
    /// 1 while playing, 0 while paused or buffering.
    pub speed: f64,
}

impl Position {
    /// The position now, given the wall clock in Unix milliseconds.
    pub fn at(&self, now_ms: u64) -> u64 {
        let elapsed = now_ms.saturating_sub(self.timestamp_ms) as f64 * self.speed;
        self.position_ms + elapsed.max(0.0) as u64
    }
}

fn position(value: &Value) -> Position {
    Position {
        position_ms: value
            .get("position_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        timestamp_ms: value
            .get("timestamp_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        speed: value.get("speed").and_then(Value::as_f64).unwrap_or(0.0),
    }
}

/// Soloist's item (with its "decorations") as a Web API track.
fn track_json(item: &Value) -> Option<Value> {
    let uri = item.get("uri")?.as_str()?;
    let id = uri.strip_prefix("spotify:track:")?;
    let deco = item.get("decorations")?;
    let name = deco
        .pointer("/identity/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let images: Vec<Value> = deco
        .pointer("/visual_identity/cover")
        .and_then(Value::as_array)
        .map(|covers| {
            covers
                .iter()
                .filter_map(|c| {
                    let width = match c.get("size")?.as_str()? {
                        "small" => 64,
                        "default" => 300,
                        "large" => 640,
                        _ => return None,
                    };
                    Some(json!({ "url": c.get("url")?, "width": width }))
                })
                .collect()
        })
        .unwrap_or_default();
    let entity = |v: &Value| -> Option<(String, String)> {
        let uri = v.pointer("/entity/uri")?.as_str()?;
        let name = v.pointer("/entity/decorations/identity/name")?.as_str()?;
        Some((uri.rsplit(':').next()?.to_string(), name.to_string()))
    };
    let artists: Vec<Value> = deco
        .get("creators")
        .and_then(Value::as_array)
        .map(|c| {
            c.iter()
                .filter_map(entity)
                .map(|(id, name)| json!({ "id": id, "name": name }))
                .collect()
        })
        .unwrap_or_default();
    let album = deco.get("parent").and_then(entity);
    Some(json!({
        "id": id,
        "uri": uri,
        "name": name,
        "type": "track",
        "is_playable": true,
        "duration_ms": deco.pointer("/playback/duration_ms").and_then(Value::as_u64).unwrap_or(0),
        "artists": artists,
        "album": album.map(|(id, name)| json!({ "id": id, "name": name, "images": images })),
    }))
}

#[derive(Deserialize)]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    logged_in: bool,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

pub fn parse(text: &str) -> Option<Event> {
    let raw: Raw = serde_json::from_str(text).ok()?;
    match raw.kind.as_str() {
        "playback_state" => {
            let value: Value = serde_json::from_str(text).ok()?;
            Some(Event::State(Box::new(State {
                status: raw.status.unwrap_or_default(),
                track: value.get("item").and_then(track_json),
                context_uri: value
                    .pointer("/context/uri")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                position: value.get("position").map(position).unwrap_or_default(),
                shuffle: value
                    .pointer("/options/shuffle")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                repeat: value
                    .pointer("/options/repeat")
                    .and_then(Value::as_str)
                    .unwrap_or("off")
                    .to_string(),
                is_active: raw.is_active,
            })))
        }
        "position_sync" => {
            let value: Value = serde_json::from_str(text).ok()?;
            Some(Event::Position(position(value.get("position")?)))
        }
        "auth_state" => Some(Event::Auth {
            logged_in: raw.logged_in,
            is_active: raw.is_active,
        }),
        "playback_changed" => raw.status.map(Event::Status),
        "queue_changed" => {
            let value: Value = serde_json::from_str(text).ok()?;
            let upcoming = value
                .get("upcoming")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|entry| track_json(entry.get("item")?))
                        .collect()
                })
                .unwrap_or_default();
            Some(Event::Queue(upcoming))
        }
        "error" => Some(Event::Error(raw.message.unwrap_or_default())),
        _ => None,
    }
}

/// A command for Soloist, e.g. `command("seek", json!({"position_ms": 3000}))`.
pub fn command(name: &str, fields: Value) -> String {
    let mut message = json!({ "type": "command", "command": name });
    if let (Some(message), Value::Object(fields)) = (message.as_object_mut(), fields) {
        message.extend(fields);
    }
    message.to_string()
}

/// A connection: `send` queues a command, events arrive on the receiver,
/// which closes when the connection does.
pub struct Connection {
    pub send: mpsc::UnboundedSender<String>,
}

pub async fn connect(addr: &str) -> Result<(Connection, mpsc::UnboundedReceiver<Event>)> {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
        .await
        .context("could not reach Soloist")?;
    let (mut sink, mut stream) = socket.split();
    let (send, mut outgoing) = mpsc::unbounded_channel::<String>();
    let (events, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(text) = outgoing.recv().await {
            if sink.send(Message::text(text)).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            if let Message::Text(text) = message
                && let Some(event) = parse(&text)
                && events.send(event).is_err()
            {
                break;
            }
        }
    });
    Ok((Connection { send }, received))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_events_lilac_uses() {
        assert_eq!(
            parse(r#"{"type":"auth_state","logged_in":true,"is_active":false,"device_name":"x"}"#),
            Some(Event::Auth {
                logged_in: true,
                is_active: false
            })
        );
        assert_eq!(
            parse(r#"{"type":"playback_changed","status":"paused"}"#),
            Some(Event::Status("paused".into()))
        );
        assert_eq!(
            parse(r#"{"type":"error","message":"command requires authentication"}"#),
            Some(Event::Error("command requires authentication".into()))
        );
        assert_eq!(parse(r#"{"type":"volume_changed","volume":4}"#), None);
        let Some(Event::Queue(upcoming)) = parse(
            r#"{"type":"queue_changed","previous":[],"upcoming":[{"uid":"a","source":"queue","item":{"uri":"spotify:track:05nN","decorations":{"identity":{"name":"Out of Time"}}}},{"uid":"b","source":"context","item":{"uri":"spotify:episode:9"}}]}"#,
        ) else {
            panic!("queue_changed not parsed");
        };
        assert_eq!(upcoming.len(), 1);
        assert_eq!(upcoming[0]["name"], "Out of Time");
        let state = parse(
            r#"{"type":"playback_state","status":"playing","item":{"uri":"spotify:track:05nN","decorations":{"identity":{"name":"Out of Time"},"visual_identity":{"cover":[{"url":"u","size":"large"}]},"parent":{"entity":{"uri":"spotify:album:5V7W","decorations":{"identity":{"name":"Telos"}}}},"creators":[{"entity":{"uri":"spotify:artist:2qx","decorations":{"identity":{"name":"Zedd"}}}}],"playback":{"duration_ms":222908}}},"context":{"uri":"spotify:album:5V7W"},"position":{"position_ms":30002,"timestamp_ms":1000,"speed":1},"is_active":true,"options":{"shuffle":true,"repeat":"context"}}"#,
        );
        let Some(Event::State(state)) = state else {
            panic!("not a state");
        };
        let track = state.track.unwrap();
        assert_eq!(track["id"], "05nN");
        assert_eq!(track["artists"][0]["name"], "Zedd");
        assert_eq!(track["album"]["images"][0]["width"], 640);
        assert_eq!(track["duration_ms"], 222908);
        assert_eq!(state.position.at(1500), 30502);
        assert!(state.shuffle);
        assert_eq!(state.repeat, "context");
        assert_eq!(parse("not json"), None);
    }

    #[test]
    fn builds_commands() {
        let text = command("seek", json!({ "position_ms": 3000 }));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "command");
        assert_eq!(value["command"], "seek");
        assert_eq!(value["position_ms"], 3000);
        let value: Value = serde_json::from_str(&command("pause", Value::Null)).unwrap();
        assert_eq!(value["command"], "pause");
    }
}
