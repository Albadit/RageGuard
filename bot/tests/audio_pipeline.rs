//! Buffering, resampling and WAV encoding.

mod common;

use std::time::{Duration, Instant};

use common::{parse_wav_header, segment, stereo_sine};
use rageguard::voice::{
    AudioError, AudioSegment, BufferEvent, BufferSettings, PcmFormat, Resampler, SegmentBuffer,
    TARGET_SAMPLE_RATE,
    resampler::{downmix_to_mono, to_i16, to_model_input},
    segment_to_wav,
    wav::{self, HEADER_LEN},
};

const TICK: Duration = Duration::from_millis(20);
/// One 20 ms Discord frame: 960 stereo frames.
const FRAME_SAMPLES: usize = 960 * 2;

fn settings() -> BufferSettings {
    BufferSettings {
        target: Duration::from_secs(3),
        min: Duration::from_secs(1),
        silence_flush: Duration::from_millis(800),
    }
}

fn buffer() -> SegmentBuffer {
    SegmentBuffer::new(PcmFormat::DISCORD, settings())
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn sine(rate: u32, freq: f32, seconds: f32) -> Vec<f32> {
    (0..(rate as f32 * seconds) as usize)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
        .collect()
}

/// Rising zero crossings per second ≈ frequency.
fn estimated_frequency(samples: &[f32], rate: u32) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    crossings as f32 / (samples.len() as f32 / rate as f32)
}

// --- buffer -----------------------------------------------------------------------------------

#[test]
fn discord_format_sizes() {
    assert_eq!(
        PcmFormat::DISCORD.samples_for(Duration::from_secs(3)),
        288_000
    );
    assert_eq!(
        PcmFormat::DISCORD.duration_of(288_000),
        Duration::from_secs(3)
    );
}

#[test]
fn continuous_speech_yields_three_second_segments() {
    let mut buffer = buffer();
    let frame = vec![1000i16; FRAME_SAMPLES];
    let now = Instant::now();
    for i in 1..150 {
        assert!(buffer.push_voice(&frame, now).is_none(), "frame {i}");
    }
    let Some(BufferEvent::Segment(segment)) = buffer.push_voice(&frame, now) else {
        panic!("150 frames = 3 s should emit a segment");
    };
    assert_eq!(segment.samples.len(), 288_000);
    assert_eq!(segment.duration(), Duration::from_secs(3));
    assert_eq!(buffer.buffered(), Duration::ZERO);
}

#[test]
fn overflow_carries_into_next_segment() {
    let mut buffer = buffer();
    let now = Instant::now();
    let big = vec![7i16; 200_000];
    assert!(buffer.push_voice(&big, now).is_none());
    let Some(BufferEvent::Segment(segment)) = buffer.push_voice(&big, now) else {
        panic!("expected a segment");
    };
    assert_eq!(segment.samples.len(), 288_000);
    assert_eq!(buffer.buffered(), PcmFormat::DISCORD.duration_of(112_000));
}

#[test]
fn silence_flushes_partial_segment() {
    let mut buffer = buffer();
    let frame = vec![500i16; FRAME_SAMPLES];
    let now = Instant::now();
    for _ in 0..75 {
        buffer.push_voice(&frame, now); // 1.5 s of speech
    }
    for tick in 1..40 {
        assert!(buffer.push_silence(TICK, now).is_none(), "tick {tick}");
    }
    let Some(BufferEvent::Segment(segment)) = buffer.push_silence(TICK, now) else {
        panic!("800 ms of silence should flush");
    };
    assert_eq!(segment.duration(), Duration::from_millis(1500));
}

#[test]
fn short_pause_does_not_flush() {
    let mut buffer = buffer();
    let frame = vec![500i16; FRAME_SAMPLES];
    let now = Instant::now();
    for _ in 0..50 {
        buffer.push_voice(&frame, now);
    }
    for _ in 0..20 {
        assert!(buffer.push_silence(TICK, now).is_none()); // 400 ms pause
    }
    for _ in 0..50 {
        buffer.push_voice(&frame, now);
    }
    assert_eq!(buffer.buffered(), Duration::from_secs(2));
}

#[test]
fn too_short_utterance_is_discarded() {
    let mut buffer = buffer();
    let frame = vec![500i16; FRAME_SAMPLES];
    let now = Instant::now();
    for _ in 0..25 {
        buffer.push_voice(&frame, now); // 0.5 s
    }
    let mut event = None;
    for _ in 0..40 {
        event = event.or(buffer.push_silence(TICK, now));
    }
    assert_eq!(
        event,
        Some(BufferEvent::DiscardedShort(Duration::from_millis(500)))
    );
    assert_eq!(buffer.buffered(), Duration::ZERO);
}

#[test]
fn silence_on_empty_buffer_is_ignored() {
    let mut buffer = buffer();
    for _ in 0..100 {
        assert!(buffer.push_silence(TICK, Instant::now()).is_none());
    }
}

#[test]
fn corrupted_partial_frames_are_trimmed() {
    let mut buffer = buffer();
    buffer.push_voice(&[1, 2, 3], Instant::now()); // odd length in stereo
    assert_eq!(buffer.buffered(), PcmFormat::DISCORD.duration_of(2));
}

#[test]
fn clear_drops_buffered_audio() {
    let mut buffer = buffer();
    buffer.push_voice(&vec![1i16; FRAME_SAMPLES * 10], Instant::now());
    buffer.clear();
    assert_eq!(buffer.buffered(), Duration::ZERO);
}

// --- resampler --------------------------------------------------------------------------------

#[test]
fn downmix_averages_channels() {
    let mono = downmix_to_mono(&[1000, -1000, 16384, 16384], 2).unwrap();
    assert_eq!(mono.len(), 2);
    assert!(mono[0].abs() < 1e-6);
    assert!((mono[1] - 0.5).abs() < 1e-6);
    assert_eq!(
        downmix_to_mono(&[1, 2], 0),
        Err(AudioError::UnsupportedChannels(0))
    );
}

#[test]
fn float_to_pcm_clips() {
    assert_eq!(
        to_i16(&[0.0, 0.5, -1.0, 2.0, -2.0]),
        vec![0, 16384, -32768, 32767, -32768]
    );
}

#[test]
fn resample_48k_to_16k_has_expected_length() {
    let resampler = Resampler::new(48_000, 16_000).unwrap();
    assert_eq!(resampler.process(&vec![0.0; 144_000]).len(), 48_000);
}

#[test]
fn resample_preserves_speech_band_tone() {
    let input = sine(48_000, 1_000.0, 1.0);
    let output = Resampler::new(48_000, 16_000).unwrap().process(&input);
    // Ignore filter edges.
    let body = &output[200..output.len() - 200];
    assert!(
        (rms(body) - rms(&input)).abs() / rms(&input) < 0.02,
        "rms {}",
        rms(body)
    );
    let freq = estimated_frequency(body, 16_000);
    assert!((freq - 1_000.0).abs() < 10.0, "frequency {freq}");
}

#[test]
fn resample_removes_content_above_new_nyquist() {
    // 12 kHz would alias to 4 kHz without an anti-aliasing filter.
    let input = sine(48_000, 12_000.0, 1.0);
    let output = Resampler::new(48_000, 16_000).unwrap().process(&input);
    let body = &output[200..output.len() - 200];
    assert!(rms(body) < 0.01 * rms(&input), "leaked rms {}", rms(body));
}

#[test]
fn resample_preserves_dc() {
    let output = Resampler::new(48_000, 16_000)
        .unwrap()
        .process(&vec![0.25; 4_800]);
    for s in &output[100..output.len() - 100] {
        assert!((s - 0.25).abs() < 1e-3);
    }
}

#[test]
fn non_integer_ratios_are_supported() {
    let output = Resampler::new(44_100, 16_000)
        .unwrap()
        .process(&sine(44_100, 440.0, 1.0));
    assert_eq!(output.len(), 16_000);
    let freq = estimated_frequency(&output[100..output.len() - 100], 16_000);
    assert!((freq - 440.0).abs() < 10.0, "frequency {freq}");
}

#[test]
fn invalid_rates_are_rejected() {
    assert!(Resampler::new(0, 16_000).is_err());
    assert!(Resampler::new(48_000, 0).is_err());
}

#[test]
fn model_input_is_16k_mono() {
    let pcm = stereo_sine(PcmFormat::DISCORD, 440.0, 3.0, 0.5);
    let mono = to_model_input(&pcm, PcmFormat::DISCORD).unwrap();
    assert_eq!(mono.len(), 3 * TARGET_SAMPLE_RATE as usize);
    assert!(matches!(
        to_model_input(&[], PcmFormat::DISCORD),
        Err(AudioError::Empty)
    ));
}

// --- WAV --------------------------------------------------------------------------------------

#[test]
fn wav_header_is_correct() {
    let bytes = wav::encode_pcm16_mono(&[1, -1, 32767, -32768], 16_000);
    assert_eq!(bytes.len(), HEADER_LEN + 8);
    let header = parse_wav_header(&bytes);
    assert_eq!(header.channels, 1);
    assert_eq!(header.sample_rate, 16_000);
    assert_eq!(header.bits_per_sample, 16);
    assert_eq!(header.data_len, 8);
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
    assert_eq!(
        u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
        32_000
    );
    assert_eq!(&bytes[44..46], &1i16.to_le_bytes());
}

#[test]
fn captured_segment_becomes_16k_mono_wav() {
    let wav = segment_to_wav(&segment(3.0, Instant::now())).unwrap();
    let header = parse_wav_header(&wav);
    assert_eq!(header.channels, 1);
    assert_eq!(header.sample_rate, 16_000);
    assert_eq!(header.data_len, 48_000 * 2);
    assert_eq!(wav.len(), HEADER_LEN + 96_000);
}

#[test]
fn empty_segment_is_rejected() {
    let empty = AudioSegment {
        samples: vec![],
        format: PcmFormat::DISCORD,
        captured_at: Instant::now(),
    };
    assert_eq!(segment_to_wav(&empty), Err(AudioError::Empty));
}
