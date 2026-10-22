//! The unit of audio between encoder and call engine.

use crate::rtp::AudioLevel;

/// One encoded 20 ms audio frame with its level, ready for DAVE encryption and RTP. Produced by the
/// native capture pipeline or by WebCodecs in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAudioFrame {
  pub opus: Vec<u8>,
  pub level: AudioLevel,
}
