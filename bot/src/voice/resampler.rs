//! Converts captured Discord audio (48 kHz stereo) into what the model expects (16 kHz mono).
//!
//! The resampler is a polyphase windowed-sinc filter: it low-passes below the output Nyquist
//! frequency before decimating, so high-frequency content does not alias into the speech band.
//! All of this is CPU-bound; call it from `tokio::task::spawn_blocking`.

use std::f64::consts::PI;

use super::{
    buffer::{AudioSegment, PcmFormat},
    wav,
};

/// Sample rate expected by wav2vec2 models.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Zero crossings of the sinc kernel on each side. Higher is sharper but slower.
const ZERO_CROSSINGS: f64 = 16.0;
/// Passband edge as a fraction of the output Nyquist frequency.
const ROLLOFF: f64 = 0.94;
/// Largest number of filter phases we are willing to precompute.
const MAX_PHASES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("unsupported sample rate conversion {from} Hz -> {to} Hz")]
    UnsupportedRate { from: u32, to: u32 },
    #[error("unsupported channel count {0}")]
    UnsupportedChannels(u16),
    #[error("segment contains no audio")]
    Empty,
}

/// Averages interleaved channels into mono and scales to `[-1.0, 1.0]`.
pub fn downmix_to_mono(interleaved: &[i16], channels: u16) -> Result<Vec<f32>, AudioError> {
    if channels == 0 || channels > 8 {
        return Err(AudioError::UnsupportedChannels(channels));
    }
    let channels = channels as usize;
    let scale = 1.0 / (f32::from(i16::MAX) + 1.0) / channels as f32;
    Ok(interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| f32::from(s)).sum::<f32>() * scale)
        .collect())
}

/// Converts float samples back to 16-bit PCM, clipping out-of-range values.
pub fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// Band-limited rational resampler with a precomputed polyphase filter bank.
#[derive(Debug, Clone)]
pub struct Resampler {
    /// Upsampling factor (output rate / gcd).
    up: usize,
    /// Downsampling factor (input rate / gcd).
    down: usize,
    /// Kernel half-width in input samples.
    half_width: usize,
    /// `up` phases, each with `2 * half_width` taps.
    phases: Vec<Vec<f32>>,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Result<Self, AudioError> {
        let unsupported = AudioError::UnsupportedRate {
            from: from_rate,
            to: to_rate,
        };
        if from_rate == 0 || to_rate == 0 {
            return Err(unsupported);
        }
        let g = gcd(from_rate as usize, to_rate as usize);
        let (up, down) = (to_rate as usize / g, from_rate as usize / g);
        if up > MAX_PHASES {
            return Err(unsupported);
        }

        // Cutoff relative to the input Nyquist frequency.
        let cutoff = (up as f64 / down as f64).min(1.0) * ROLLOFF;
        let half_width = (ZERO_CROSSINGS / cutoff).ceil() as usize;

        let phases = (0..up)
            .map(|phase| {
                let frac = phase as f64 / up as f64;
                let mut taps: Vec<f64> = (0..2 * half_width)
                    .map(|o| {
                        // Distance between the output position and input tap `o`.
                        let x = frac + half_width as f64 - 1.0 - o as f64;
                        cutoff * sinc(cutoff * x) * blackman(x / half_width as f64)
                    })
                    .collect();
                // Normalise each phase to unity DC gain.
                let sum: f64 = taps.iter().sum();
                if sum.abs() > f64::EPSILON {
                    taps.iter_mut().for_each(|t| *t /= sum);
                }
                taps.into_iter().map(|t| t as f32).collect()
            })
            .collect();

        Ok(Self {
            up,
            down,
            half_width,
            phases,
        })
    }

    /// Resamples a complete signal.
    pub fn process(&self, input: &[f32]) -> Vec<f32> {
        if self.up == self.down {
            return input.to_vec();
        }
        let out_len = input.len() * self.up / self.down;
        let len = input.len() as isize;
        let mut output = Vec::with_capacity(out_len);
        for n in 0..out_len {
            let pos = n * self.down;
            let (center, phase) = (pos / self.up, pos % self.up);
            let taps = &self.phases[phase];
            let start = center as isize - self.half_width as isize + 1;
            let mut acc = 0.0_f32;
            for (o, &tap) in taps.iter().enumerate() {
                let idx = start + o as isize;
                if (0..len).contains(&idx) {
                    acc += tap * input[idx as usize];
                }
            }
            output.push(acc);
        }
        output
    }
}

/// Full conversion: interleaved capture-format PCM → 16 kHz mono float samples.
pub fn to_model_input(samples: &[i16], format: PcmFormat) -> Result<Vec<f32>, AudioError> {
    if samples.is_empty() {
        return Err(AudioError::Empty);
    }
    let mono = downmix_to_mono(samples, format.channels)?;
    if format.sample_rate == TARGET_SAMPLE_RATE {
        return Ok(mono);
    }
    Ok(Resampler::new(format.sample_rate, TARGET_SAMPLE_RATE)?.process(&mono))
}

/// Converts a captured segment into an in-memory 16 kHz mono 16-bit WAV file.
/// Nothing touches the disk.
pub fn segment_to_wav(segment: &AudioSegment) -> Result<Vec<u8>, AudioError> {
    let mono = to_model_input(&segment.samples, segment.format)?;
    Ok(wav::encode_pcm16_mono(&to_i16(&mono), TARGET_SAMPLE_RATE))
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

/// Blackman window over `u ∈ [-1, 1]`.
fn blackman(u: f64) -> f64 {
    if u.abs() > 1.0 {
        0.0
    } else {
        0.42 + 0.5 * (PI * u).cos() + 0.08 * (2.0 * PI * u).cos()
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}
