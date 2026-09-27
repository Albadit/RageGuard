//! Collects 20 ms voice frames into multi-second segments suitable for emotion inference.

use std::time::{Duration, Instant};

use crate::config::{AudioConfig, DetectionConfig};

/// Layout of interleaved 16-bit PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl PcmFormat {
    /// What songbird produces by default: Discord's native 48 kHz stereo.
    pub const DISCORD: Self = Self {
        sample_rate: 48_000,
        channels: 2,
    };

    /// Interleaved samples per second of audio.
    pub fn samples_per_second(self) -> usize {
        self.sample_rate as usize * self.channels as usize
    }

    pub fn samples_for(self, duration: Duration) -> usize {
        let frames = (duration.as_secs_f64() * f64::from(self.sample_rate)).round() as usize;
        frames * self.channels as usize
    }

    pub fn duration_of(self, samples: usize) -> Duration {
        Duration::from_secs_f64(samples as f64 / self.samples_per_second() as f64)
    }
}

/// A finished chunk of one user's speech, still in the capture format.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSegment {
    /// Interleaved PCM.
    pub samples: Vec<i16>,
    pub format: PcmFormat,
    /// When the last sample of the segment was captured.
    pub captured_at: Instant,
}

impl AudioSegment {
    pub fn duration(&self) -> Duration {
        self.format.duration_of(self.samples.len())
    }
}

/// Segmentation parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferSettings {
    /// Emit a segment as soon as this much speech is buffered.
    pub target: Duration,
    /// Partial segments shorter than this are dropped when the speaker goes quiet.
    pub min: Duration,
    /// Flush a partial segment after this much continuous silence.
    pub silence_flush: Duration,
}

impl BufferSettings {
    pub fn from_config(detection: &DetectionConfig, audio: &AudioConfig) -> Self {
        Self {
            target: detection.segment(),
            min: Duration::from_secs_f32(audio.min_segment_seconds),
            silence_flush: Duration::from_millis(audio.silence_flush_ms),
        }
    }
}

/// Outcome of feeding the buffer.
#[derive(Debug, Clone, PartialEq)]
pub enum BufferEvent {
    /// A segment is ready for analysis.
    Segment(AudioSegment),
    /// A too-short utterance was discarded after the speaker went quiet.
    DiscardedShort(Duration),
}

/// Accumulates one speaker's audio. Speech is appended; short pauses are skipped rather than
/// recorded, so segments are dense with speech.
#[derive(Debug)]
pub struct SegmentBuffer {
    format: PcmFormat,
    settings: BufferSettings,
    target_samples: usize,
    min_samples: usize,
    samples: Vec<i16>,
    silence: Duration,
}

impl SegmentBuffer {
    pub fn new(format: PcmFormat, settings: BufferSettings) -> Self {
        let target_samples = format
            .samples_for(settings.target)
            .max(format.channels as usize);
        let min_samples = format.samples_for(settings.min).min(target_samples);
        Self {
            format,
            settings,
            target_samples,
            min_samples,
            samples: Vec::with_capacity(target_samples),
            silence: Duration::ZERO,
        }
    }

    pub fn format(&self) -> PcmFormat {
        self.format
    }

    /// Appends one decoded voice frame. Returns a segment once enough speech is buffered.
    ///
    /// A trailing partial frame (length not a multiple of the channel count) indicates a
    /// corrupted packet and is dropped.
    pub fn push_voice(&mut self, pcm: &[i16], now: Instant) -> Option<BufferEvent> {
        let channels = self.format.channels as usize;
        let usable = pcm.len() - pcm.len() % channels;
        self.silence = Duration::ZERO;
        self.samples.extend_from_slice(&pcm[..usable]);

        if self.samples.len() < self.target_samples {
            return None;
        }
        // Emit exactly one target-length segment; any overflow starts the next one.
        let rest = self.samples.split_off(self.target_samples);
        let samples = std::mem::replace(&mut self.samples, rest);
        Some(BufferEvent::Segment(AudioSegment {
            samples,
            format: self.format,
            captured_at: now,
        }))
    }

    /// Records a tick in which the speaker was silent (or their packet was missing).
    pub fn push_silence(&mut self, elapsed: Duration, now: Instant) -> Option<BufferEvent> {
        if self.samples.is_empty() {
            return None;
        }
        self.silence += elapsed;
        if self.silence < self.settings.silence_flush {
            return None;
        }
        self.silence = Duration::ZERO;
        let samples = std::mem::take(&mut self.samples);
        if samples.len() >= self.min_samples {
            Some(BufferEvent::Segment(AudioSegment {
                samples,
                format: self.format,
                captured_at: now,
            }))
        } else {
            Some(BufferEvent::DiscardedShort(
                self.format.duration_of(samples.len()),
            ))
        }
    }

    pub fn buffered(&self) -> Duration {
        self.format.duration_of(self.samples.len())
    }

    /// Drops any buffered audio (e.g. after a reconnect).
    pub fn clear(&mut self) {
        self.samples.clear();
        self.silence = Duration::ZERO;
    }
}
