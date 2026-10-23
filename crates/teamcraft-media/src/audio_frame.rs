//! The unit of audio between encoder and call engine.

use crate::rtp::AudioLevel;

/// Call audio always runs at 48 kHz (Opus' native rate); devices resample if needed.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES_PER_CHANNEL: usize = 960;

/// Upper bound for the loss Opus plans in-band FEC for: beyond it, FEC takes more bitrate from the
/// speech than it gives back.
pub const MAX_PLANNED_LOSS_PERCENT: u8 = 30;

/// The loss an Opus encoder plans its in-band FEC for: the measured uplink loss, never below the
/// configured floor (loss comes in bursts the average hides) nor above
/// [`MAX_PLANNED_LOSS_PERCENT`].
pub fn planned_loss_percent(floor_percent: u8, measured_percent: u8) -> u8 {
  measured_percent.max(floor_percent).min(MAX_PLANNED_LOSS_PERCENT)
}

/// One encoded 20 ms audio frame with its level, ready for DAVE encryption and RTP. Produced by the
/// native capture pipeline or by WebCodecs in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedAudioFrame {
  pub opus: Vec<u8>,
  pub level: AudioLevel,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn planned_loss_follows_the_measurement_within_bounds() {
    assert_eq!(planned_loss_percent(5, 0), 5);
    assert_eq!(planned_loss_percent(5, 12), 12);
    assert_eq!(planned_loss_percent(5, 80), MAX_PLANNED_LOSS_PERCENT);
  }
}
