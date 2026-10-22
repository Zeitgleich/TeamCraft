//! The unit of audio between encoder and call engine.

use crate::rtp::AudioLevel;

/// Call audio always runs at 48 kHz (Opus' native rate); devices resample if needed.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES_PER_CHANNEL: usize = 960;

/// One encoded 20 ms audio frame with its level, ready for DAVE encryption and RTP. Produced by the
/// native capture pipeline or by WebCodecs in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAudioFrame {
  pub opus: Vec<u8>,
  pub level: AudioLevel,
}
