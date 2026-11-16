//! Audio output that reacts at once.
//!
//! librespot's own rodio sink keeps about half a second of decoded audio
//! queued, applies volume before queueing and drains the queue before
//! pausing, so every control was heard half a second late. Here:
//!
//! - volume is the stream's volume in the sound server (see
//!   `system_volume`), or applied as samples leave for the speaker,
//! - pause holds the queue where it is and resume continues from it,
//! - seek, skip and load drop what is queued ([`OutputControl::flush`]),
//!
//! so a quarter second of queue protects against stutter without making
//! anything wait for it.
//!
//! The sink is also the player's clock: it knows when audio actually starts
//! and stops coming out ([`Cue`]) and how much of it has been heard, so the
//! controls and the seek bar show what the speaker does, not what was asked.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use librespot_core::Error;
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::VolumeCtrl;
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::mappings::MappedCtrl as _;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::cpal::traits::HostTrait as _;
use rodio::{DeviceTrait as _, OutputStream, OutputStreamBuilder};
use tokio::sync::mpsc;

/// Audio queued ahead of the speaker. Counted in samples, not packets:
/// Vorbis packets range from ~3 to ~23 ms, so a packet count can leave only
/// a few milliseconds queued and the output runs dry (choppy playback).
const QUEUE_AHEAD: Duration = Duration::from_millis(250);
const QUEUE_SAMPLES: usize =
    (SAMPLE_RATE as usize * NUM_CHANNELS as usize * QUEUE_AHEAD.as_millis() as usize) / 1000;

/// Per-sample step towards a new gain: about 10 ms to settle, fast enough to
/// feel immediate and slow enough not to click.
const GAIN_SMOOTHING: f32 = 0.0025;

/// Samples (all channels) per millisecond of audio.
const SAMPLES_PER_MS: u64 = SAMPLE_RATE as u64 * NUM_CHANNELS as u64 / 1000;

/// What the speaker just did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    /// Audio started coming out: after a play, or after a seek, skip or
    /// load replaced what was queued.
    Started,
    /// The output went silent: paused or stopped.
    Stopped,
}

/// No generation is audible.
const SILENT: u64 = u64::MAX;

/// Shared between the sink (audio thread), the mixer (Spirc) and the backend.
#[derive(Default)]
pub struct OutputControl {
    /// Target gain as `f32` bits, applied at playback time.
    gain: AtomicU32,
    /// Gain actually reached, so the next packet continues the ramp.
    applied: AtomicU32,
    /// Bumped by [`flush`](Self::flush); queued audio of older generations is skipped.
    generation: AtomicU64,
    /// Samples appended but not yet played.
    queued: AtomicUsize,
    /// Samples played since the sink opened.
    played: AtomicU64,
    /// The generation whose audio is coming out, or [`SILENT`].
    audible: AtomicU64,
    /// Our sink is the one in use (it is not when no device could be opened).
    in_use: AtomicBool,
    /// librespot started the sink and has not stopped it. rodio applies a
    /// pause only every few milliseconds, so samples still trickle out
    /// after a stop; they must not count as audio starting again.
    running: AtomicBool,
    cues: OnceLock<mpsc::UnboundedSender<Cue>>,
}

impl OutputControl {
    pub fn new(gain: f32) -> Arc<Self> {
        let control = Self::default();
        control.gain.store(gain.to_bits(), Ordering::Relaxed);
        control.applied.store(gain.to_bits(), Ordering::Relaxed);
        control.audible.store(SILENT, Ordering::Relaxed);
        Arc::new(control)
    }

    /// Where [`Cue`]s go.
    pub fn cues(&self) -> mpsc::UnboundedReceiver<Cue> {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = self.cues.set(tx);
        rx
    }

    /// Whether audio goes through this control, so its clock and cues apply.
    pub fn in_use(&self) -> bool {
        self.in_use.load(Ordering::Relaxed)
    }

    /// Milliseconds of audio heard since the sink opened.
    pub fn played_ms(&self) -> u64 {
        self.played.load(Ordering::Relaxed) / SAMPLES_PER_MS
    }

    /// Milliseconds of audio decoded but not heard yet.
    pub fn queued_ms(&self) -> u32 {
        (self.queued.load(Ordering::Relaxed) as u64 / SAMPLES_PER_MS) as u32
    }

    fn cue(&self, cue: Cue) {
        if let Some(tx) = self.cues.get() {
            let _ = tx.send(cue);
        }
    }

    /// The speaker went quiet.
    fn silence(&self) {
        if self.audible.swap(SILENT, Ordering::AcqRel) != SILENT {
            self.cue(Cue::Stopped);
        }
    }

    /// Drop everything queued, for an explicit seek, skip or load: the user
    /// wants to hear the new position now, not the last quarter second of
    /// the old one.
    pub fn flush(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.queued.store(0, Ordering::Relaxed);
    }
}

/// librespot's mixer interface over [`OutputControl`]: Spirc and our own
/// controls set the volume here, the sink applies it.
pub struct OutputMixer {
    volume: AtomicU16,
    ctrl: VolumeCtrl,
    out: Arc<OutputControl>,
    /// When Lilac itself last changed the volume.
    local_at: std::sync::Mutex<Option<std::time::Instant>>,
}

/// Spirc applies volume commands only after its state upload for the
/// previous command, so for a while after a local change it can still set
/// older values (the 0 of a mute just undone). Those are ignored.
const LOCAL_WINS: Duration = Duration::from_secs(2);

impl OutputMixer {
    pub fn new(out: Arc<OutputControl>, volume: u16) -> Self {
        let mixer = Self {
            volume: AtomicU16::new(volume),
            // librespot's default 60 dB log curve leaves the lower half of
            // the slider nearly silent; a cubic curve is closer to the
            // official apps.
            ctrl: VolumeCtrl::Cubic(VolumeCtrl::DEFAULT_DB_RANGE),
            out,
            local_at: Default::default(),
        };
        mixer.apply(volume);
        // Start at the target rather than ramping to it from full volume.
        let gain = mixer.out.gain.load(Ordering::Relaxed);
        mixer.out.applied.store(gain, Ordering::Relaxed);
        mixer
    }
}

impl OutputMixer {
    /// A change made in Lilac (slider, keys, mute, desktop mixer).
    pub fn set_local(&self, volume: u16) {
        if let Ok(mut at) = self.local_at.lock() {
            *at = Some(std::time::Instant::now());
        }
        self.apply(volume);
    }

    fn apply(&self, volume: u16) {
        self.volume.store(volume, Ordering::Relaxed);
        // With a sound server the stream's own volume is the one volume, the
        // same one the desktop's mixer shows; the samples pass unchanged.
        #[cfg(target_os = "linux")]
        if crate::system_volume::available() {
            crate::system_volume::set(volume as f32 / u16::MAX as f32);
            self.out.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
            return;
        }
        let gain = self.ctrl.to_mapped(volume) as f32;
        self.out.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
}

impl Mixer for OutputMixer {
    fn open(_: MixerConfig) -> Result<Self, Error> {
        Ok(Self::new(OutputControl::new(1.0), u16::MAX))
    }

    fn volume(&self) -> u16 {
        self.volume.load(Ordering::Relaxed)
    }

    /// Called by Spirc: for remote changes (a phone's volume buttons) and,
    /// late, for the volume Lilac announced itself.
    fn set_volume(&self, volume: u16) {
        let recent_local = self
            .local_at
            .lock()
            .ok()
            .and_then(|at| *at)
            .is_some_and(|at| at.elapsed() < LOCAL_WINS);
        if !recent_local {
            self.apply(volume);
        }
    }
}

pub struct InstantSink {
    sink: rodio::Sink,
    out: Arc<OutputControl>,
    _stream: OutputStream,
}

impl InstantSink {
    pub fn open(out: Arc<OutputControl>) -> Result<Self, SinkError> {
        let stream = open_stream().map_err(|e| SinkError::ConnectionRefused(e.to_string()))?;
        let sink = rodio::Sink::connect_new(stream.mixer());
        sink.pause();
        out.in_use.store(true, Ordering::Relaxed);
        Ok(Self {
            sink,
            out,
            _stream: stream,
        })
    }
}

/// Stereo at librespot's 44.1 kHz when the device takes it, so the sound
/// server resamples rather than rodio's linear resampler.
fn open_stream() -> Result<OutputStream, rodio::StreamError> {
    let host = rodio::cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return OutputStreamBuilder::open_default_stream();
    };
    let config = device.default_output_config().ok().and_then(|default| {
        device
            .supported_output_configs()
            .ok()?
            .find(|c| c.channels() == NUM_CHANNELS as rodio::cpal::ChannelCount)
            .and_then(|c| {
                c.try_with_sample_rate(rodio::cpal::SampleRate(SAMPLE_RATE))
                    .or_else(|| c.try_with_sample_rate(default.sample_rate()))
            })
    });
    let mut stream = match config {
        Some(config) => OutputStreamBuilder::default()
            .with_device(device.clone())
            .with_config(&config.config())
            .with_sample_format(rodio::cpal::SampleFormat::F32)
            .open_stream()
            .or_else(|_| OutputStreamBuilder::from_device(device)?.open_stream_or_fallback())?,
        None => OutputStreamBuilder::from_device(device)?.open_stream_or_fallback()?,
    };
    stream.log_on_drop(false);
    Ok(stream)
}

impl Sink for InstantSink {
    fn start(&mut self) -> SinkResult<()> {
        self.out.running.store(true, Ordering::Release);
        self.sink.play();
        Ok(())
    }

    /// Silence now, keeping the queue: librespot calls this on pause, and a
    /// resume then continues exactly where the speaker stopped. Loads and
    /// seeks flush the queue separately.
    fn stop(&mut self) -> SinkResult<()> {
        self.out.running.store(false, Ordering::Release);
        self.sink.pause();
        self.out.silence();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples = converter.f64_to_f32(samples);
        self.out.queued.fetch_add(samples.len(), Ordering::Relaxed);
        self.sink.append(Counted {
            inner: rodio::buffer::SamplesBuffer::new(
                NUM_CHANNELS as rodio::ChannelCount,
                SAMPLE_RATE,
                samples,
            ),
            generation: self.out.generation.load(Ordering::Acquire),
            // Picked up when the packet starts playing, not now: packets wait
            // in the queue, and the ramp must continue from where the
            // previous one left it.
            gain: f32::NAN,
            out: self.out.clone(),
        });
        // Back-pressure for the decoder thread. librespot stops writing
        // while paused, so this cannot wait forever.
        while self.out.queued.load(Ordering::Relaxed) > QUEUE_SAMPLES && !self.sink.is_paused() {
            thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

/// One packet on its way to the speaker: applies the current volume, counts
/// itself down, and vanishes if a flush made it stale.
struct Counted {
    inner: rodio::buffer::SamplesBuffer,
    generation: u64,
    gain: f32,
    out: Arc<OutputControl>,
}

impl Iterator for Counted {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.out.generation.load(Ordering::Relaxed) != self.generation {
            return None;
        }
        let sample = self.inner.next()?;
        let _ = self
            .out
            .queued
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        self.out.played.fetch_add(1, Ordering::Relaxed);
        if self.out.running.load(Ordering::Acquire)
            && self.out.audible.load(Ordering::Relaxed) != self.generation
            && self.out.audible.swap(self.generation, Ordering::AcqRel) != self.generation
        {
            self.out.cue(Cue::Started);
        }
        if self.gain.is_nan() {
            self.gain = f32::from_bits(self.out.applied.load(Ordering::Relaxed));
        }
        let target = f32::from_bits(self.out.gain.load(Ordering::Relaxed));
        if self.gain != target {
            self.gain += (target - self.gain) * GAIN_SMOOTHING;
            if (target - self.gain).abs() < 1e-4 {
                self.gain = target;
            }
            self.out
                .applied
                .store(self.gain.to_bits(), Ordering::Relaxed);
        }
        Some(sample * self.gain)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl rodio::Source for Counted {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_sets_output_gain_on_a_cubic_curve() {
        let out = OutputControl::new(1.0);
        let mixer = OutputMixer::new(out.clone(), u16::MAX);
        assert_eq!(f32::from_bits(out.gain.load(Ordering::Relaxed)), 1.0);
        mixer.set_volume(0);
        assert_eq!(f32::from_bits(out.gain.load(Ordering::Relaxed)), 0.0);
        mixer.set_volume(u16::MAX / 2);
        let half = f32::from_bits(out.gain.load(Ordering::Relaxed));
        // Cubic: half the slider is well below half the amplitude, but audible.
        assert!(half > 0.05 && half < 0.4, "{half}");
        assert_eq!(mixer.volume(), u16::MAX / 2);
    }

    #[test]
    fn late_spirc_volume_does_not_undo_a_local_change() {
        let out = OutputControl::new(1.0);
        let mixer = OutputMixer::new(out.clone(), 0);
        mixer.set_local(u16::MAX);
        // Spirc catching up with an older command, e.g. the 0 of a mute.
        mixer.set_volume(0);
        assert_eq!(mixer.volume(), u16::MAX);
        assert_eq!(f32::from_bits(out.gain.load(Ordering::Relaxed)), 1.0);
    }

    #[test]
    fn flush_makes_queued_packets_vanish() {
        let out = OutputControl::new(1.0);
        out.queued.store(4, Ordering::Relaxed);
        let mut packet = Counted {
            inner: rodio::buffer::SamplesBuffer::new(2, SAMPLE_RATE, vec![0.5; 4]),
            generation: out.generation.load(Ordering::Relaxed),
            gain: 1.0,
            out: out.clone(),
        };
        assert_eq!(packet.next(), Some(0.5));
        out.flush();
        assert_eq!(packet.next(), None);
        assert_eq!(out.queued.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cues_follow_the_audio() {
        let out = OutputControl::new(1.0);
        let mut cues = out.cues();
        let packet = |out: &Arc<OutputControl>| Counted {
            inner: rodio::buffer::SamplesBuffer::new(2, SAMPLE_RATE, vec![0.5; 4]),
            generation: out.generation.load(Ordering::Relaxed),
            gain: 1.0,
            out: out.clone(),
        };
        out.running.store(true, Ordering::Relaxed);
        let mut first = packet(&out);
        assert!(cues.try_recv().is_err(), "nothing heard yet");
        first.next();
        first.next();
        assert_eq!(cues.try_recv(), Ok(Cue::Started));
        assert!(cues.try_recv().is_err(), "one cue per start");
        // A seek: the new position starting to play is a new start.
        out.flush();
        packet(&out).next();
        assert_eq!(cues.try_recv(), Ok(Cue::Started));
        // A stop, with samples still trickling out before rodio pauses.
        out.running.store(false, Ordering::Relaxed);
        out.silence();
        assert_eq!(cues.try_recv(), Ok(Cue::Stopped));
        packet(&out).next();
        assert!(
            cues.try_recv().is_err(),
            "trickle after a stop is not a start"
        );
        out.silence();
        assert!(cues.try_recv().is_err(), "already silent");
        assert_eq!(out.played.load(Ordering::Relaxed), 4);
    }
}
