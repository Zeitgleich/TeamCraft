//! The unit of audio between encoder and call engine.

use crate::rtp::AudioLevel;

/// Call audio always runs at 48 kHz (Opus' native rate); devices resample if needed.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// Samples per channel in one 20 ms frame.
pub const FRAME_SAMPLES_PER_CHANNEL: usize = 960;
/// Samples per channel in the longest Opus packet (120 ms, RFC 6716 §3.2.5).
pub const MAX_PACKET_SAMPLES_PER_CHANNEL: usize = 6 * FRAME_SAMPLES_PER_CHANNEL;

/// Upper bound for the loss Opus plans in-band FEC for: beyond it, FEC takes more bitrate from the
/// speech than it gives back.
pub const MAX_PLANNED_LOSS_PERCENT: u8 = 30;

/// The loss an Opus encoder plans its in-band FEC for: the measured uplink loss, never below the
/// configured floor (loss comes in bursts the average hides) nor above
/// [`MAX_PLANNED_LOSS_PERCENT`].
pub fn planned_loss_percent(floor_percent: u8, measured_percent: u8) -> u8 {
  measured_percent.max(floor_percent).min(MAX_PLANNED_LOSS_PERCENT)
}

/// The duration of an Opus packet in 48 kHz samples per channel, read from its TOC byte (RFC 6716
/// §3.1): 20 ms packets are the call's default, 40 and 60 ms ones a voice on a thin uplink sends
/// (#1893). `None` for a packet that isn't valid Opus framing.
pub fn opus_packet_samples(packet: &[u8]) -> Option<usize> {
  let toc = *packet.first()?;
  let frames = match toc & 0b11 {
    0 => 1,
    1 | 2 => 2,
    _ => usize::from(*packet.get(1)? & 0b11_1111),
  };
  let samples = frames * opus_frame_samples(toc);
  (frames > 0 && samples <= MAX_PACKET_SAMPLES_PER_CHANNEL).then_some(samples)
}

/// A packet that stands for a lost one shaped like `last` (the packet before it): the same
/// configuration and frame count, every frame empty. A decoder conceals an empty frame (RFC 6716
/// §3.2.1), for as long as the lost packet would have played: where there is no concealment call
/// (WebCodecs), this keeps a lost 60 ms packet from becoming 20 ms of silence and the playout from
/// running ahead. `None` when `last` isn't valid Opus framing.
pub fn opus_lost_packet_like(last: &[u8]) -> Option<Vec<u8>> {
  let samples = opus_packet_samples(last)?;
  let toc = last[0];
  let frames = samples / opus_frame_samples(toc);
  // Code 3, constant bitrate, no padding: `frames` frames of (2 - 2) / frames = 0 bytes.
  u8::try_from(frames)
    .ok()
    .map(|count| vec![(toc & !0b11) | 0b11, count])
}

/// Samples per channel of one frame of the TOC byte's configuration (RFC 6716 Table 2).
fn opus_frame_samples(toc: u8) -> usize {
  let config = toc >> 3;
  let index = usize::from(config % 4);
  match config {
    // SILK: 10, 20, 40 or 60 ms.
    0..=11 => [480, 960, 1920, 2880][index],
    // Hybrid: 10 or 20 ms.
    12..=15 => [480, 960][index % 2],
    // CELT: 2.5, 5, 10 or 20 ms.
    _ => [120, 240, 480, 960][index],
  }
}

/// One encoded audio packet (20 ms, or 40 or 60 ms on a thin uplink) with its level, ready for DAVE
/// encryption and RTP. Produced by the native capture pipeline or by WebCodecs in the browser.
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

  mod opus_framing {
    use super::*;

    /// Configuration `config`, code `code`, as the TOC byte has them (mono).
    fn toc(config: u8, code: u8) -> u8 {
      (config << 3) | code
    }

    #[test]
    fn single_frames_last_as_their_configuration_says() {
      // SILK wideband 20, 40 and 60 ms (configs 9, 10, 11); CELT fullband 20 ms (31); hybrid 10 ms.
      assert_eq!(opus_packet_samples(&[toc(9, 0), 1, 2]), Some(960));
      assert_eq!(opus_packet_samples(&[toc(10, 0), 1, 2]), Some(1920));
      assert_eq!(opus_packet_samples(&[toc(11, 0), 1, 2]), Some(2880));
      assert_eq!(opus_packet_samples(&[toc(31, 0), 1]), Some(960));
      assert_eq!(opus_packet_samples(&[toc(12, 0)]), Some(480));
      assert_eq!(opus_packet_samples(&[toc(16, 0)]), Some(120));
    }

    #[test]
    fn packets_of_several_frames_add_them_up() {
      // CELT has no frame longer than 20 ms: 40 and 60 ms are two and three frames.
      assert_eq!(opus_packet_samples(&[toc(31, 1), 1, 2]), Some(1920));
      assert_eq!(opus_packet_samples(&[toc(31, 2), 1, 1, 2]), Some(1920));
      assert_eq!(opus_packet_samples(&[toc(31, 3), 3, 0, 0]), Some(2880));
      // The count byte's VBR and padding flags don't count.
      assert_eq!(opus_packet_samples(&[toc(31, 3), 0b1100_0011, 0]), Some(2880));
    }

    #[test]
    fn malformed_framing_is_none() {
      assert_eq!(opus_packet_samples(&[]), None);
      // Code 3 without its count byte, with no frames, or with more than 120 ms.
      assert_eq!(opus_packet_samples(&[toc(31, 3)]), None);
      assert_eq!(opus_packet_samples(&[toc(31, 3), 0]), None);
      assert_eq!(opus_packet_samples(&[toc(31, 3), 7]), None);
      assert_eq!(opus_packet_samples(&[toc(11, 3), 3]), None);
      assert_eq!(opus_packet_samples(&[toc(11, 3), 2]), Some(5760));
    }

    #[test]
    fn a_lost_packet_is_stood_for_by_empty_frames_of_its_length() {
      for last in [
        vec![toc(11, 0), 9, 9],
        vec![toc(31, 3), 3, 9],
        vec![toc(9, 1), 9, 9],
      ] {
        let lost = opus_lost_packet_like(&last).unwrap();
        assert_eq!(lost.len(), 2);
        assert_eq!(opus_packet_samples(&lost), opus_packet_samples(&last));
        assert_eq!(lost[0] >> 3, last[0] >> 3, "same configuration");
      }
      assert_eq!(opus_lost_packet_like(&[]), None);
    }
  }
}
