//! Preview clips for free accounts: Spotify's 30-second MP3 of a track,
//! played from memory through rodio.

use std::io::Cursor;
use std::time::Duration;

use anyhow::{Context as _, Result};
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source as _};

/// Length of a clip when the decoder cannot tell.
pub const CLIP_MS: u32 = 30_000;

/// The sound output, opened on the first clip and kept.
pub struct Output {
    stream: OutputStream,
}

impl Output {
    pub fn open() -> Result<Self> {
        let mut stream =
            OutputStreamBuilder::open_default_stream().context("no sound output found")?;
        stream.log_on_drop(false);
        Ok(Self { stream })
    }

    /// Start playing `bytes` from `from_ms`. Its length, and what controls it.
    pub fn play(&self, bytes: Vec<u8>, from_ms: u32, volume: f32) -> Result<(Clip, u32)> {
        let len = bytes.len() as u64;
        let source = Decoder::builder()
            .with_data(Cursor::new(bytes))
            .with_byte_len(len)
            .with_seekable(true)
            .with_hint("mp3")
            .build()
            .context("the preview could not be decoded")?;
        let length = source
            .total_duration()
            .map(|d| d.as_millis() as u32)
            .filter(|ms| *ms > 0)
            .unwrap_or(CLIP_MS);
        let sink = Sink::connect_new(self.stream.mixer());
        sink.set_volume(volume);
        sink.append(source);
        let clip = Clip { sink };
        if from_ms > 0 {
            clip.seek(from_ms);
        }
        Ok((clip, length))
    }
}

/// One clip playing. Dropping it stops it.
pub struct Clip {
    sink: Sink,
}

impl Clip {
    pub fn seek(&self, position_ms: u32) {
        let _ = self
            .sink
            .try_seek(Duration::from_millis(position_ms.into()));
    }

    pub fn set_volume(&self, volume: f32) {
        self.sink.set_volume(volume);
    }
}
