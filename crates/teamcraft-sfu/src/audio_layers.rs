//! Audio simulcast (#1893 step 3): which voice each receiver gets of each sender.
//!
//! With DAVE the SFU can't re-encode a voice for a receiver on a thin downlink, so a sender whose
//! uplink has room sends its voice twice: layer 0 as its voice step says, and layer
//! [`THIN_VOICE_LAYER`] at the thinnest step (12 kbit/s in 60 ms packets, about 25 kbit/s on the
//! wire instead of about 70). Each receiver gets one of them: the thin one while its downlink is
//! thin or its voice is being lost ([`VoiceLayerChoice`]), the sender's own otherwise.
//!
//! A receiver's jitter buffer orders packets by RTP sequence number alone, and the two layers count
//! on their own. So the SFU renumbers each sender's voice per receiver ([`VoiceSequence`]): a switch
//! continues from the last number the receiver got, and gaps within a layer stay gaps. DAVE covers
//! only the payload; the RTP header is the SFU's to rewrite.
//!
//! Pure: time is passed in.

use std::time::{Duration, Instant};

/// The datagram layer of a voice's thin copy.
pub const THIN_VOICE_LAYER: u8 = 1;

/// When a receiver takes the thin voices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLayerConfig {
  /// Below this downlink estimate a receiver takes the thin voices: three full voices take about
  /// 210 kbit/s on the wire, three thin ones about 75.
  pub thin_below_bps: u32,
  /// Back to the full voices once the estimate stayed above this for [`Self::full_after`].
  pub full_above_bps: u32,
  pub full_after: Duration,
  /// A sender's thin copy counts as stopped when none came for this long: its receivers get its
  /// own voice then.
  pub thin_stale_after: Duration,
  /// A sender whose voice comes back after a pause (a new turn) and that sent thin copies within
  /// [`Self::thin_known_for`] starts its thin copy too, a packet or two after the voice: for this
  /// long a thin receiver waits for it instead of taking the full voice it can't carry.
  pub thin_resume_grace: Duration,
  pub thin_known_for: Duration,
}

/// Whether a receiver takes the thin voices now.
#[derive(Debug, Clone, Default)]
pub struct VoiceLayerChoice {
  is_thin: bool,
  maybe_above_since: Option<Instant>,
}

/// One sender's voice as one receiver sees it: numbered on across switches of layer.
#[derive(Debug, Clone, Default)]
pub struct VoiceSequence {
  maybe_layer: Option<u8>,
  offset: u16,
  maybe_last: Option<u16>,
}

impl Default for AudioLayerConfig {
  fn default() -> Self {
    Self {
      thin_below_bps: 200_000,
      full_above_bps: 300_000,
      full_after: Duration::from_secs(5),
      thin_stale_after: Duration::from_millis(500),
      thin_resume_grace: Duration::from_millis(300),
      thin_known_for: Duration::from_secs(30),
    }
  }
}

impl VoiceLayerChoice {
  /// Follows the receiver's downlink estimate and whether its voice is being lost: thin at once,
  /// full again only after the estimate stayed high. Returns whether it takes the thin voices.
  pub fn update(
    &mut self,
    config: &AudioLayerConfig,
    estimate_bps: u32,
    is_voice_hurt: bool,
    now: Instant,
  ) -> bool {
    if is_voice_hurt || estimate_bps < config.thin_below_bps {
      self.is_thin = true;
      self.maybe_above_since = None;
    } else if self.is_thin {
      if estimate_bps >= config.full_above_bps {
        let since = *self.maybe_above_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= config.full_after {
          self.is_thin = false;
          self.maybe_above_since = None;
        }
      } else {
        self.maybe_above_since = None;
      }
    }
    self.is_thin
  }

  pub fn is_thin(&self) -> bool {
    self.is_thin
  }
}

impl VoiceSequence {
  /// The number the receiver gets for packet `sequence_number` of `layer`.
  pub fn map(&mut self, layer: u8, sequence_number: u16) -> u16 {
    if self.maybe_layer != Some(layer) {
      self.offset = self
        .maybe_last
        .map_or(0, |last| last.wrapping_add(1).wrapping_sub(sequence_number));
      self.maybe_layer = Some(layer);
    }
    let mapped = sequence_number.wrapping_add(self.offset);
    // A late packet of the same layer keeps its place behind the newest.
    let is_newest = self
      .maybe_last
      .is_none_or(|last| (mapped.wrapping_sub(last) as i16) > 0);
    if is_newest {
      self.maybe_last = Some(mapped);
    }
    mapped
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod choice {
    use super::*;

    #[test]
    fn a_thin_downlink_takes_the_thin_voices_at_once() {
      let config = AudioLayerConfig::default();
      let mut choice = VoiceLayerChoice::default();
      let now = Instant::now();
      assert!(!choice.update(&config, 2_000_000, false, now));
      assert!(choice.update(&config, 64_000, false, now));
    }

    #[test]
    fn voice_lost_toward_the_receiver_takes_the_thin_voices_too() {
      let config = AudioLayerConfig::default();
      let mut choice = VoiceLayerChoice::default();
      assert!(choice.update(&config, 2_000_000, true, Instant::now()));
    }

    #[test]
    fn the_full_voices_come_back_only_after_the_link_stayed_good() {
      let config = AudioLayerConfig::default();
      let mut choice = VoiceLayerChoice::default();
      let start = Instant::now();
      choice.update(&config, 64_000, false, start);
      // Between the bounds: stays thin.
      assert!(choice.update(&config, 250_000, false, start + Duration::from_secs(10)));
      let good = start + Duration::from_secs(11);
      assert!(choice.update(&config, 1_000_000, false, good));
      // A dip in between starts the wait over.
      assert!(choice.update(&config, 250_000, false, good + Duration::from_secs(3)));
      let good = good + Duration::from_secs(4);
      assert!(choice.update(&config, 1_000_000, false, good));
      assert!(choice.update(
        &config,
        1_000_000,
        false,
        good + config.full_after - Duration::from_millis(1)
      ));
      assert!(!choice.update(&config, 1_000_000, false, good + config.full_after));
    }
  }

  /// The layer is the one senders put their thin copy on.
  #[test]
  fn the_thin_layer_is_the_senders() {
    assert_eq!(
      THIN_VOICE_LAYER,
      teamcraft_media::datagram::SimulcastLayer::THIN_VOICE.value()
    );
  }

  mod sequence {
    use super::*;

    #[test]
    fn one_layer_keeps_its_numbers_and_gaps() {
      let mut sequence = VoiceSequence::default();
      assert_eq!(sequence.map(0, 100), 100);
      assert_eq!(sequence.map(0, 101), 101);
      assert_eq!(sequence.map(0, 104), 104, "a gap stays a gap");
    }

    #[test]
    fn a_switch_numbers_on_from_the_last_packet() {
      let mut sequence = VoiceSequence::default();
      sequence.map(0, 500);
      sequence.map(0, 501);
      assert_eq!(sequence.map(THIN_VOICE_LAYER, 9), 502);
      assert_eq!(sequence.map(THIN_VOICE_LAYER, 10), 503);
      assert_eq!(sequence.map(THIN_VOICE_LAYER, 12), 505, "a gap of the thin layer");
      assert_eq!(sequence.map(0, 540), 506, "and back");
      assert_eq!(sequence.map(0, 541), 507);
    }

    #[test]
    fn numbers_wrap() {
      let mut sequence = VoiceSequence::default();
      sequence.map(0, u16::MAX);
      assert_eq!(sequence.map(THIN_VOICE_LAYER, 7), 0);
      assert_eq!(sequence.map(THIN_VOICE_LAYER, 8), 1);
    }

    #[test]
    fn a_late_packet_keeps_its_place() {
      let mut sequence = VoiceSequence::default();
      sequence.map(0, 10);
      sequence.map(0, 12);
      assert_eq!(sequence.map(0, 11), 11);
      assert_eq!(
        sequence.map(THIN_VOICE_LAYER, 3),
        13,
        "after the newest, not the late one"
      );
    }
  }
}
