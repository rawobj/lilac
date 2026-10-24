//! Playback through librespot, announced as a Spotify Connect device.
//!
//! librespot owns the streaming session, decryption, decoding and output.
//! Its Connect component (Spirc) owns what plays next, so lilac shows up in
//! every Spotify app's device list and stays in step with them. This module
//! builds both from our tokens and settings, forwards commands, and turns
//! Spotify's pushed "cluster" updates into a small snapshot for the UI.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow};
use futures_util::StreamExt as _;
use librespot_connect::{
    ConnectConfig, LoadContextOptions, LoadRequest, LoadRequestOptions, Options as ContextOptions,
    PlayingTrack, Spirc,
};
use librespot_core::authentication::Credentials;
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::dealer::protocol::Message;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, Bitrate, PlayerConfig};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{Player as Engine, PlayerEventChannel};
use librespot_protocol::connect::ClusterUpdate;
use tokio::sync::mpsc;

pub use librespot_playback::player::PlayerEvent as EngineEvent;

use crate::settings::{Quality, Settings};
use crate::spotify::api::Auth;
use crate::spotify::oauth;

/// Settings that are fixed for the lifetime of one librespot player.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub quality: Quality,
    pub normalize: bool,
    pub gapless: bool,
}

impl Options {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            quality: settings.quality,
            normalize: settings.normalize,
            gapless: settings.gapless,
        }
    }
}

/// A device in the user's Spotify Connect list.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub kind: DeviceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Computer,
    Phone,
    Speaker,
    Tv,
    Other,
}

/// Playback on whichever device is active, as Spotify reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Playback {
    pub track_uri: String,
    pub context_uri: String,
    pub duration_ms: u32,
    pub position_ms: u32,
    pub playing: bool,
    pub shuffle: bool,
    pub repeat_context: bool,
    pub repeat_track: bool,
    /// Upcoming track URIs, user queue first.
    pub next: Vec<String>,
}

/// One pushed update of the Connect cluster.
#[derive(Clone, Debug, PartialEq)]
pub struct Cluster {
    pub active_device: Option<String>,
    pub devices: Vec<Device>,
    pub playback: Option<Playback>,
}

pub struct Backend {
    session: Session,
    engine: Arc<Engine>,
    spirc: Spirc,
    pub device_id: String,
    pub options: Options,
}

pub struct Channels {
    pub engine: PlayerEventChannel,
    pub cluster: mpsc::UnboundedReceiver<Cluster>,
}

impl Backend {
    /// Log in to an access point, register as a Connect device and set up
    /// the player. Runs on the I/O runtime.
    pub async fn start(
        auth: Arc<Auth>,
        options: Options,
        volume: f32,
    ) -> Result<(Backend, Channels)> {
        let token = auth.token().await?;
        let config = SessionConfig {
            client_id: oauth::DESKTOP_CLIENT_ID.to_string(),
            device_id: crate::paths::device_id(),
            ..SessionConfig::default()
        };
        // No librespot cache: credentials stay in our own store and audio is never written to disk.
        let session = Session::new(config, None);

        let mixer = Arc::new(
            SoftMixer::open(MixerConfig::default())
                .map_err(|e| anyhow!("could not open the volume mixer: {e}"))?,
        );
        mixer.set_volume(to_mixer(volume));

        let sink = audio_backend::find(None).context("no audio backend compiled in")?;
        let player_config = PlayerConfig {
            bitrate: match options.quality {
                Quality::Low => Bitrate::Bitrate96,
                Quality::Normal => Bitrate::Bitrate160,
                Quality::High => Bitrate::Bitrate320,
            },
            gapless: options.gapless,
            normalisation: options.normalize,
            ..PlayerConfig::default()
        };
        let engine = Engine::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || sink(None, AudioFormat::default()),
        );
        let engine_events = engine.get_player_event_channel();

        // Subscribe before Spirc connects the session so the first cluster is not missed.
        let mut updates = session
            .dealer()
            .listen_for(
                "hm://connect-state/v1/cluster",
                Message::from_raw::<ClusterUpdate>,
            )
            .map_err(|e| anyhow!("could not listen for Connect updates: {e}"))?;

        let connect = ConnectConfig {
            name: "lilac".into(),
            device_type: DeviceType::Computer,
            initial_volume: to_mixer(volume),
            ..ConnectConfig::default()
        };
        let (spirc, task) = Spirc::new(
            connect,
            session.clone(),
            Credentials::with_access_token(token),
            engine.clone(),
            mixer,
        )
        .await
        .map_err(|e| anyhow!("could not start a streaming session: {e}"))?;
        tokio::spawn(task);

        let (cluster_tx, cluster) = mpsc::unbounded_channel();
        let time = session.clone();
        tokio::spawn(async move {
            while let Some(update) = updates.next().await {
                let Ok(update) = update else { continue };
                if cluster_tx
                    .send(simplify(&update, time.time_delta()))
                    .is_err()
                {
                    break;
                }
            }
        });

        let device_id = session.device_id().to_string();
        Ok((
            Backend {
                session,
                engine,
                spirc,
                device_id,
                options,
            },
            Channels {
                engine: engine_events,
                cluster,
            },
        ))
    }

    /// False once the access point connection dropped; a new backend is needed then.
    pub fn is_usable(&self) -> bool {
        !self.session.is_invalid() && !self.engine.is_invalid()
    }

    /// Make this device the active one and play `context` (a `spotify:` URI)
    /// or, without one, the given track list, starting at `start`.
    pub fn load(
        &self,
        context: Option<&str>,
        tracks: &[String],
        start: Start,
        shuffle: bool,
    ) -> Result<()> {
        let options = LoadRequestOptions {
            start_playing: true,
            seek_to: 0,
            context_options: Some(LoadContextOptions::Options(ContextOptions {
                shuffle,
                ..ContextOptions::default()
            })),
            playing_track: Some(match start {
                Start::Uri(uri) => PlayingTrack::Uri(uri),
                Start::Index(ix) => PlayingTrack::Index(ix),
            }),
        };
        let request = match context {
            Some(uri) => LoadRequest::from_context_uri(uri.to_string(), options),
            None => LoadRequest::from_tracks(tracks.to_vec(), options),
        };
        // Load only works on the active device; activating an active device is a no-op.
        self.spirc.activate().map_err(spirc_error)?;
        self.spirc.load(request).map_err(spirc_error)
    }

    pub fn play(&self) {
        let _ = self.spirc.play();
    }

    pub fn pause(&self) {
        let _ = self.spirc.pause();
    }

    pub fn next(&self) {
        let _ = self.spirc.next();
    }

    pub fn previous(&self) {
        let _ = self.spirc.prev();
    }

    pub fn seek(&self, position_ms: u32) {
        let _ = self.spirc.set_position_ms(position_ms);
    }

    pub fn set_shuffle(&self, on: bool) {
        let _ = self.spirc.shuffle(on);
    }

    pub fn set_repeat(&self, context: bool, track: bool) {
        let _ = self.spirc.repeat(context);
        let _ = self.spirc.repeat_track(track);
    }

    pub fn set_volume(&self, volume: f32) {
        // Through Spirc, so other apps see the new volume too.
        let _ = self.spirc.set_volume(to_mixer(volume));
    }

    pub fn shutdown(&self) {
        let _ = self.spirc.shutdown();
        self.session.shutdown();
    }
}

pub enum Start {
    Uri(String),
    Index(u32),
}

fn spirc_error(err: librespot_core::Error) -> anyhow::Error {
    anyhow!("Spotify Connect: {err}")
}

/// The slider's 0..1 onto librespot's 16-bit volume. The mixer applies its
/// own logarithmic curve, so the mapping here stays linear.
fn to_mixer(volume: f32) -> u16 {
    (volume.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16
}

pub fn from_mixer(volume: u16) -> f32 {
    volume as f32 / u16::MAX as f32
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn simplify(update: &ClusterUpdate, time_delta_s: i64) -> Cluster {
    let cluster = &update.cluster;
    let mut devices: Vec<Device> = cluster
        .device
        .iter()
        .filter(|(_, d)| d.can_play && !d.is_offline)
        .map(|(id, d)| Device {
            id: if d.device_id.is_empty() {
                id.clone()
            } else {
                d.device_id.clone()
            },
            name: d.name.clone(),
            kind: match d.device_type.value() {
                1 | 14 => DeviceKind::Computer,
                2 | 3 | 13 => DeviceKind::Phone,
                4 | 6 | 8 | 10 | 12 => DeviceKind::Speaker,
                5 | 7 | 9 | 11 => DeviceKind::Tv,
                _ => DeviceKind::Other,
            },
        })
        .collect();
    devices.sort_by(|a, b| a.name.cmp(&b.name));
    let active_device = Some(cluster.active_device_id.clone()).filter(|id| !id.is_empty());

    let state = &cluster.player_state;
    let playback = (!state.track.uri.is_empty()).then(|| {
        // The position is reported as of a server timestamp; carry it to now.
        let server_now = now_ms() + time_delta_s * 1000;
        let playing = state.is_playing && !state.is_paused;
        let elapsed = if playing {
            (server_now - state.timestamp).max(0)
        } else {
            0
        };
        let position = (state.position_as_of_timestamp + elapsed).clamp(0, state.duration.max(0));
        Playback {
            track_uri: state.track.uri.clone(),
            context_uri: state.context_uri.clone(),
            duration_ms: state.duration.max(0) as u32,
            position_ms: position as u32,
            playing,
            shuffle: state.options.shuffling_context,
            repeat_context: state.options.repeating_context,
            repeat_track: state.options.repeating_track,
            next: state
                .next_tracks
                .iter()
                .filter(|t| {
                    t.uri.starts_with("spotify:track:") || t.uri.starts_with("spotify:episode:")
                })
                .map(|t| t.uri.clone())
                .collect(),
        }
    });
    Cluster {
        active_device,
        devices,
        playback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mapping() {
        assert_eq!(to_mixer(0.0), 0);
        assert_eq!(to_mixer(1.0), u16::MAX);
        assert_eq!(to_mixer(2.0), u16::MAX);
        assert_eq!(to_mixer(0.5), 32768);
        assert!((from_mixer(32768) - 0.5).abs() < 0.001);
    }

    #[test]
    fn cluster_snapshot() {
        let mut update = ClusterUpdate::new();
        let cluster = update.cluster.mut_or_insert_default();
        cluster.active_device_id = "phone".into();
        let mut phone = librespot_protocol::connect::DeviceInfo::new();
        phone.can_play = true;
        phone.name = "Pixel".into();
        phone.device_type = librespot_protocol::devices::DeviceType::SMARTPHONE.into();
        cluster.device.insert("phone".into(), phone);
        let state = cluster.player_state.mut_or_insert_default();
        state.track.mut_or_insert_default().uri = "spotify:track:abc".into();
        state.duration = 200_000;
        state.position_as_of_timestamp = 30_000;
        state.is_paused = true;

        let snapshot = simplify(&update, 0);
        assert_eq!(snapshot.active_device.as_deref(), Some("phone"));
        assert_eq!(snapshot.devices.len(), 1);
        assert_eq!(snapshot.devices[0].kind, DeviceKind::Phone);
        let playback = snapshot.playback.unwrap();
        assert_eq!(playback.position_ms, 30_000);
        assert!(!playback.playing);
    }
}
