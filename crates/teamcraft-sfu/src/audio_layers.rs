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
//! A receiver on a thin downlink gets no padding probes: their bursts queued in front of its voice.
//! It learns that its link recovered from the voices themselves ([`ThinRecovery`]).
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
  /// A receiver on a thin downlink whose voices arrived without loss or delay for this long tries
  /// the full voices; once those arrived as cleanly for as long again, its downlink counts as
  /// recovered ([`ThinRecovery`]).
  pub recovery_hold: Duration,
  /// Each trial that failed, and each relapse within [`Self::relapse_within`] of a recovery,
  /// doubles the hold, up to this.
  pub recovery_max_hold: Duration,
  pub relapse_within: Duration,
}

/// Whether a receiver takes the thin voices now.
#[derive(Debug, Clone, Default)]
pub struct VoiceLayerChoice {
  is_thin: bool,
  maybe_above_since: Option<Instant>,
}

/// How a receiver on a thin downlink comes back without padding probes (#1893 step 3): its voices
/// are the probe. Voice that arrives without loss or delay for a hold shows room for the full
/// voices; the full voices arriving as cleanly for another hold show room beyond, and the receiver
/// leaves the thin downlink (video and the usual probing come back). Lost or late voice at any point
/// starts over at the thin voices; a failed trial doubles the hold, so a link that stays thin is
/// tried rarely.
#[derive(Debug, Clone, Default)]
pub struct ThinRecovery {
  stage: RecoveryStage,
  /// How long its voices arrived cleanly in this stage: only time while a voice went to it counts.
  clean_for: Duration,
  maybe_updated_at: Option<Instant>,
  failures: u32,
  maybe_recovered_at: Option<Instant>,
}

/// Where a receiver on a thin downlink is on its way back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecoveryStage {
  /// The thin voices.
  #[default]
  Thin,
  /// The full voices, still without video.
  FullVoices,
  /// The downlink carried the full voices: it is thin no more.
  Recovered,
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
      thin_resume_grace: Duration::from_millis(500),
      thin_known_for: Duration::from_secs(30),
      recovery_hold: Duration::from_secs(10),
      recovery_max_hold: Duration::from_secs(80),
      relapse_within: Duration::from_secs(60),
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

  /// Back to the full voices at once: its thin downlink recovered ([`ThinRecovery`]).
  pub fn set_full(&mut self) {
    self.is_thin = false;
    self.maybe_above_since = None;
  }
}

impl ThinRecovery {
  /// The receiver's downlink became thin: it starts at the thin voices. Thin again soon after it
  /// recovered counts as a failed trial.
  pub fn enter(&mut self, config: &AudioLayerConfig, now: Instant) {
    let is_relapse = self
      .maybe_recovered_at
      .is_some_and(|at| now.saturating_duration_since(at) < config.relapse_within);
    if is_relapse {
      self.failures = self.failures.saturating_add(1);
    } else if self.maybe_recovered_at.is_some() {
      self.failures = 0;
    }
    self.stage = RecoveryStage::Thin;
    self.clean_for = Duration::ZERO;
    self.maybe_updated_at = Some(now);
    self.maybe_recovered_at = None;
  }

  /// Follows its voices: `is_voice_flowing` while a voice goes to it, `is_voice_hurt` while
  /// what went to it was lost or late. Returns the stage it is in now.
  pub fn update(
    &mut self,
    config: &AudioLayerConfig,
    is_voice_flowing: bool,
    is_voice_hurt: bool,
    now: Instant,
  ) -> RecoveryStage {
    let elapsed = self
      .maybe_updated_at
      .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
    self.maybe_updated_at = Some(now);
    if self.stage == RecoveryStage::Recovered {
      return self.stage;
    }
    if is_voice_hurt {
      if self.stage == RecoveryStage::FullVoices {
        self.failures = self.failures.saturating_add(1);
        self.stage = RecoveryStage::Thin;
      }
      self.clean_for = Duration::ZERO;
      return self.stage;
    }
    if is_voice_flowing {
      self.clean_for += elapsed;
    }
    if self.clean_for >= self.hold(config) {
      self.clean_for = Duration::ZERO;
      self.stage = match self.stage {
        RecoveryStage::Thin => RecoveryStage::FullVoices,
        RecoveryStage::FullVoices | RecoveryStage::Recovered => {
          self.maybe_recovered_at = Some(now);
          RecoveryStage::Recovered
        },
      };
    }
    self.stage
  }

  pub fn stage(&self) -> RecoveryStage {
    self.stage
  }

  /// The hold of a stage: doubled for each failure, within the bound.
  fn hold(&self, config: &AudioLayerConfig) -> Duration {
    let factor = 1u32.checked_shl(self.failures).unwrap_or(u32::MAX);
    config
      .recovery_hold
      .saturating_mul(factor)
      .min(config.recovery_max_hold)
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

  mod recovery {
    use super::*;

    const STEP: Duration = Duration::from_millis(100);

    /// Clean voices for a hold try the full voices, clean full voices for another leave the thin
    /// downlink: 20 s on a link that recovered.
    #[test]
    fn clean_voices_recover_in_two_holds() {
      let config = AudioLayerConfig::default();
      let mut recovery = ThinRecovery::default();
      let now = Instant::now();
      recovery.enter(&config, now);
      let full_at = run(
        &mut recovery,
        &config,
        now,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      assert_eq!(full_at.saturating_duration_since(now), config.recovery_hold);
      let recovered_at = run(
        &mut recovery,
        &config,
        full_at,
        true,
        false,
        RecoveryStage::Recovered,
      );
      assert_eq!(
        recovered_at.saturating_duration_since(now),
        2 * config.recovery_hold
      );
    }

    /// Silence proves nothing: the hold counts only time a voice went to the receiver.
    #[test]
    fn silence_doesnt_count() {
      let config = AudioLayerConfig::default();
      let mut recovery = ThinRecovery::default();
      let now = Instant::now();
      recovery.enter(&config, now);
      for step in 1..=600u32 {
        assert_eq!(
          recovery.update(&config, false, false, now + STEP * step),
          RecoveryStage::Thin
        );
      }
    }

    /// Full voices that are lost go back to the thin ones, and the next trial waits twice as long.
    #[test]
    fn a_failed_trial_doubles_the_hold() {
      let config = AudioLayerConfig::default();
      let mut recovery = ThinRecovery::default();
      let now = Instant::now();
      recovery.enter(&config, now);
      let full_at = run(
        &mut recovery,
        &config,
        now,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      let hurt_at = full_at + Duration::from_secs(2);
      assert_eq!(recovery.update(&config, true, true, hurt_at), RecoveryStage::Thin);
      let again_at = run(
        &mut recovery,
        &config,
        hurt_at,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      assert_eq!(
        again_at.saturating_duration_since(hurt_at),
        2 * config.recovery_hold
      );
    }

    /// Lost voice at the thin voices starts the hold over.
    #[test]
    fn lost_thin_voices_start_over() {
      let config = AudioLayerConfig::default();
      let mut recovery = ThinRecovery::default();
      let now = Instant::now();
      recovery.enter(&config, now);
      let hurt_at = now + Duration::from_secs(8);
      recovery.update(&config, true, false, hurt_at);
      assert_eq!(
        recovery.update(&config, true, true, hurt_at + STEP),
        RecoveryStage::Thin
      );
      let full_at = run(
        &mut recovery,
        &config,
        hurt_at + STEP,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      assert_eq!(
        full_at.saturating_duration_since(hurt_at + STEP),
        config.recovery_hold
      );
    }

    /// Thin again within a minute of a recovery: the next recovery waits twice as long; later, the
    /// usual hold again.
    #[test]
    fn a_relapse_doubles_the_hold_and_a_lasting_recovery_resets_it() {
      let config = AudioLayerConfig::default();
      let mut recovery = ThinRecovery::default();
      let now = Instant::now();
      recovery.enter(&config, now);
      let full_at = run(
        &mut recovery,
        &config,
        now,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      let recovered_at = run(
        &mut recovery,
        &config,
        full_at,
        true,
        false,
        RecoveryStage::Recovered,
      );
      let relapse_at = recovered_at + Duration::from_secs(5);
      recovery.enter(&config, relapse_at);
      let again_at = run(
        &mut recovery,
        &config,
        relapse_at,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      assert_eq!(
        again_at.saturating_duration_since(relapse_at),
        2 * config.recovery_hold
      );
      let recovered_at = run(
        &mut recovery,
        &config,
        again_at,
        true,
        false,
        RecoveryStage::Recovered,
      );
      let much_later = recovered_at + config.relapse_within;
      recovery.enter(&config, much_later);
      let full_at = run(
        &mut recovery,
        &config,
        much_later,
        true,
        false,
        RecoveryStage::FullVoices,
      );
      assert_eq!(
        full_at.saturating_duration_since(much_later),
        config.recovery_hold
      );
    }

    /// Steps of 100 ms from `from` until `recovery` reaches `stage`; when it did.
    fn run(
      recovery: &mut ThinRecovery,
      config: &AudioLayerConfig,
      from: Instant,
      is_voice_flowing: bool,
      is_voice_hurt: bool,
      stage: RecoveryStage,
    ) -> Instant {
      for step in 1..=2_000u32 {
        let at = from + STEP * step;
        if recovery.update(config, is_voice_flowing, is_voice_hurt, at) == stage {
          return at;
        }
      }
      panic!("never reached {stage:?}");
    }
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
