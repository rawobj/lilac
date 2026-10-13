//! Audio playback through librespot.
//!
//! librespot owns the streaming session, decryption, decoding and output.
//! This module only builds it from our OAuth token and settings and exposes
//! the handful of calls the [`Player`](super::Player) entity needs.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_core::spotify_uri::SpotifyUri;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, Bitrate, PlayerConfig};
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{Player as Engine, PlayerEventChannel};

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

pub struct Backend {
    session: Session,
    engine: Arc<Engine>,
    mixer: Arc<SoftMixer>,
    pub options: Options,
}

impl Backend {
    /// Log in to an access point and set up the player. Runs on the I/O runtime.
    pub async fn start(
        auth: Arc<Auth>,
        options: Options,
        volume: f32,
    ) -> Result<(Backend, PlayerEventChannel)> {
        let token = auth.token().await?;
        let config = SessionConfig {
            client_id: oauth::DESKTOP_CLIENT_ID.to_string(),
            ..SessionConfig::default()
        };
        // No librespot cache: credentials stay in our own store and audio is never written to disk.
        let session = Session::new(config, None);
        session
            .connect(Credentials::with_access_token(token), false)
            .await
            .map_err(|e| anyhow!("could not start a streaming session: {e}"))?;

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
        let events = engine.get_player_event_channel();
        Ok((
            Backend {
                session,
                engine,
                mixer,
                options,
            },
            events,
        ))
    }

    /// False once the access point connection dropped; a new backend is needed then.
    pub fn is_usable(&self) -> bool {
        !self.session.is_invalid() && !self.engine.is_invalid()
    }

    pub fn load(&self, uri: &str, play: bool, position_ms: u32) -> Result<()> {
        self.engine.load(parse(uri)?, play, position_ms);
        Ok(())
    }

    pub fn preload(&self, uri: &str) -> Result<()> {
        self.engine.preload(parse(uri)?);
        Ok(())
    }

    pub fn play(&self) {
        self.engine.play();
    }

    pub fn pause(&self) {
        self.engine.pause();
    }

    pub fn stop(&self) {
        self.engine.stop();
    }

    pub fn seek(&self, position_ms: u32) {
        self.engine.seek(position_ms);
    }

    pub fn set_volume(&self, volume: f32) {
        self.mixer.set_volume(to_mixer(volume));
    }

    pub fn shutdown(&self) {
        self.engine.stop();
        self.session.shutdown();
    }
}

fn parse(uri: &str) -> Result<SpotifyUri> {
    SpotifyUri::from_uri(uri).map_err(|e| anyhow!("not a playable item {uri:?}: {e}"))
}

/// The slider's 0..1 onto librespot's 16-bit volume. The mixer applies its
/// own logarithmic curve, so the mapping here stays linear.
fn to_mixer(volume: f32) -> u16 {
    (volume.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16
}

/// The URI librespot reports for an event, as the `spotify:…` string we key tracks by.
pub fn event_uri(uri: &SpotifyUri) -> Option<String> {
    uri.to_uri().ok()
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
    }

    #[test]
    fn uris_roundtrip() {
        let uri = "spotify:track:4cOdK2wGLETKBW3PvgPWqT";
        assert_eq!(event_uri(&parse(uri).unwrap()).as_deref(), Some(uri));
        assert!(parse("spotify:nonsense").is_err());
    }
}
