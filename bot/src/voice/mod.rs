//! Voice capture pipeline:
//!
//! ```text
//! Discord voice packets → receiver (monitored user only) → buffer (3 s segments)
//!   → resampler (16 kHz mono) → WAV → AI service
//! ```

pub mod access;
pub mod buffer;
pub mod connection;
pub mod receiver;
pub mod resampler;
pub mod wav;

pub use access::{VoiceAccessProblem, check_voice_access};
pub use buffer::{AudioSegment, BufferEvent, BufferSettings, PcmFormat, SegmentBuffer};
pub use connection::{CAPTURE_FORMAT, VoiceLink, VoiceLocks, songbird_config};
pub use receiver::{Target, Targets, VoiceReceiver};
pub use resampler::{AudioError, Resampler, TARGET_SAMPLE_RATE, segment_to_wav};
