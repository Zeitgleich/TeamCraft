//! Selection of the loudest speakers from the unencrypted audio level extension.
//!
//! Each packet's loudness feeds an exponential moving average per participant, so a single loud
//! click doesn't steal a slot. The selection is recomputed at most every `reselect_interval`
//! (cheap per-packet checks), except that a free slot is filled immediately so the first words in
//! a quiet room aren't cut. A selected speaker is only replaced by someone louder by at least
//! `switch_margin_db`, which stops two similar voices from flapping. Participants that stopped
//! sending for `timeout`, or whose average is silence, are never selected. Time is passed in, so
//! the logic is deterministic.

use std::{
  collections::HashMap,
  time::{Duration, Instant},
};

use teamcraft_media::rtp::AudioLevel;
use teamcraft_proto::DeviceId;

/// Weight of a new sample in the moving average (out of 1.0).
const SMOOTHING_FACTOR: f32 = 0.2;
/// Averages at or below this loudness count as silence.
const SILENCE_LOUDNESS: f32 = 0.5;

/// Tracks recent loudness per participant and keeps the top `max_speakers` selected.
#[derive(Debug)]
pub struct SpeakerSelector {
  config: SpeakerSelectionConfig,
  speakers: HashMap<DeviceId, SpeakerState>,
  /// Current selection, loudest first.
  selected: Vec<DeviceId>,
  maybe_last_selection_at: Option<Instant>,
}

/// Settings of the speaker selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpeakerSelectionConfig {
  /// Audio of at most this many speakers is forwarded (ARCHITECTURE.md §6.3: 3–5).
  pub max_speakers: usize,
  /// A participant whose audio stopped this long ago no longer counts as a speaker.
  pub timeout: Duration,
  /// How often the selection is recomputed.
  pub reselect_interval: Duration,
  /// How much louder (in dB) someone must be to replace a selected speaker.
  pub switch_margin_db: f32,
}

#[derive(Debug, Clone, Copy)]
struct SpeakerState {
  average_loudness: f32,
  last_packet_at: Instant,
}

impl Default for SpeakerSelectionConfig {
  fn default() -> Self {
    Self {
      max_speakers: 3,
      timeout: Duration::from_millis(1000),
      reselect_interval: Duration::from_millis(100),
      switch_margin_db: 6.0,
    }
  }
}

impl SpeakerSelector {
  pub fn new(config: SpeakerSelectionConfig) -> Self {
    Self {
      config,
      speakers: HashMap::new(),
      selected: Vec::new(),
      maybe_last_selection_at: None,
    }
  }

  /// Records one audio packet's level. Returns the new selection when it changed.
  pub fn record(
    &mut self,
    participant_id: DeviceId,
    level: AudioLevel,
    now: Instant,
  ) -> Option<Vec<DeviceId>> {
    let sample = f32::from(level.loudness());
    let state = self
      .speakers
      .entry(participant_id)
      .and_modify(|state| {
        state.average_loudness += SMOOTHING_FACTOR * (sample - state.average_loudness);
        state.last_packet_at = now;
      })
      .or_insert(SpeakerState {
        average_loudness: sample,
        last_packet_at: now,
      });
    let fills_free_slot = self.selected.len() < self.config.max_speakers
      && state.average_loudness > SILENCE_LOUDNESS
      && !self.selected.contains(&participant_id);
    let is_due = self
      .maybe_last_selection_at
      .is_none_or(|last| now.saturating_duration_since(last) >= self.config.reselect_interval);
    if fills_free_slot || is_due {
      self.reselect(now)
    } else {
      None
    }
  }

  /// Forgets a participant. Returns the new selection when it changed.
  pub fn remove(&mut self, participant_id: DeviceId, now: Instant) -> Option<Vec<DeviceId>> {
    self.speakers.remove(&participant_id);
    if self.selected.contains(&participant_id) {
      self.reselect(now)
    } else {
      None
    }
  }

  /// Whether `participant_id`'s audio is currently forwarded.
  pub fn is_selected(&self, participant_id: DeviceId) -> bool {
    self.selected.contains(&participant_id)
  }

  /// The current selection, loudest first.
  pub fn selected(&self) -> &[DeviceId] {
    &self.selected
  }

  /// Recomputes the selection; returns it when the set of speakers changed.
  fn reselect(&mut self, now: Instant) -> Option<Vec<DeviceId>> {
    self.maybe_last_selection_at = Some(now);
    let mut candidates: Vec<(DeviceId, f32)> = self
      .speakers
      .iter()
      .filter(|(_, state)| {
        now.saturating_duration_since(state.last_packet_at) < self.config.timeout
          && state.average_loudness > SILENCE_LOUDNESS
      })
      .map(|(&id, state)| (id, state.average_loudness))
      .collect();
    sort_loudest_first(&mut candidates);

    let mut next = self.keep_and_fill(&candidates);
    self.swap_in_clearly_louder(&candidates, &mut next);
    sort_loudest_first(&mut next);
    let next: Vec<DeviceId> = next.into_iter().map(|(id, _)| id).collect();

    let is_same_set = next.len() == self.selected.len() && next.iter().all(|id| self.selected.contains(id));
    self.selected = next;
    if is_same_set {
      None
    } else {
      Some(self.selected.clone())
    }
  }

  /// Keeps current speakers that are still candidates and fills free slots, loudest first.
  fn keep_and_fill(&self, candidates: &[(DeviceId, f32)]) -> Vec<(DeviceId, f32)> {
    let mut next: Vec<(DeviceId, f32)> = candidates
      .iter()
      .filter(|(id, _)| self.selected.contains(id))
      .copied()
      .collect();
    for &candidate in candidates {
      if next.len() >= self.config.max_speakers {
        break;
      }
      if !next.iter().any(|(id, _)| *id == candidate.0) {
        next.push(candidate);
      }
    }
    next.truncate(self.config.max_speakers);
    next
  }

  /// Replaces the quietest selected speaker while someone unselected is louder by the margin.
  fn swap_in_clearly_louder(&self, candidates: &[(DeviceId, f32)], next: &mut [(DeviceId, f32)]) {
    for &challenger in candidates {
      if next.iter().any(|(id, _)| *id == challenger.0) {
        continue;
      }
      let Some(quietest) = next.iter_mut().min_by(|(_, a), (_, b)| a.total_cmp(b)) else {
        return;
      };
      if challenger.1 > quietest.1 + self.config.switch_margin_db {
        *quietest = challenger;
      } else {
        // Candidates are sorted loudest first, so nobody after this one qualifies either.
        return;
      }
    }
  }
}

/// Loudest first; ties go to the lower id so the result is stable.
fn sort_loudest_first(speakers: &mut [(DeviceId, f32)]) {
  speakers
    .sort_by(|(a_id, a_loudness), (b_id, b_loudness)| b_loudness.total_cmp(a_loudness).then(a_id.cmp(b_id)));
}

#[cfg(test)]
mod tests {
  use super::*;

  const STEP: Duration = Duration::from_millis(20);

  mod selection {
    use super::*;

    #[test]
    fn picks_the_loudest() {
      let mut clock = Clock::new();
      let mut selector = selector(3);
      for (id, level_dbov) in [(1, 60), (2, 10), (3, 30), (4, 20), (5, 90)] {
        selector.record(device(id), level(level_dbov), clock.now);
      }
      clock.advance(Duration::from_millis(100));
      selector.record(device(5), level(90), clock.now);
      assert_eq!(selector.selected(), devices(&[2, 4, 3]));
      assert!(!selector.is_selected(device(1)));
    }

    #[test]
    fn first_speakers_in_a_quiet_room_are_selected_immediately() {
      let clock = Clock::new();
      let mut selector = selector(3);
      selector.record(device(1), level(40), clock.now);
      let changed = selector.record(device(2), level(40), clock.now + Duration::from_millis(1));
      assert_eq!(changed, Some(devices(&[1, 2])));
    }

    #[test]
    fn silence_is_never_selected() {
      let clock = Clock::new();
      let mut selector = selector(3);
      selector.record(device(1), AudioLevel::SILENCE, clock.now);
      selector.record(device(2), level(100), clock.now);
      assert_eq!(selector.selected(), devices(&[2]));
    }

    #[test]
    fn stale_speakers_time_out() {
      let mut clock = Clock::new();
      let mut selector = selector(3);
      selector.record(device(1), level(10), clock.now);
      clock.advance(Duration::from_millis(900));
      selector.record(device(2), level(50), clock.now);
      clock.advance(Duration::from_millis(200));
      let changed = selector.record(device(2), level(50), clock.now);
      assert_eq!(changed, Some(devices(&[2])));
    }

    #[test]
    fn removed_participants_free_their_slot() {
      let clock = Clock::new();
      let mut selector = selector(1);
      selector.record(device(9), level(20), clock.now);
      selector.record(device(3), level(30), clock.now);
      assert_eq!(selector.selected(), devices(&[9]));
      assert_eq!(selector.remove(device(9), clock.now), Some(devices(&[3])));
      assert_eq!(selector.remove(device(4), clock.now), None);
    }
  }

  mod stability {
    use super::*;

    #[test]
    fn single_spike_does_not_displace_a_steady_speaker() {
      let mut clock = Clock::new();
      let mut selector = selector(1);
      for _ in 0..20 {
        selector.record(device(1), level(20), clock.now);
        selector.record(device(2), level(80), clock.now);
        clock.advance(STEP);
      }
      selector.record(device(2), level(0), clock.now);
      clock.advance(Duration::from_millis(100));
      selector.record(device(1), level(20), clock.now);
      assert_eq!(selector.selected(), devices(&[1]));
    }

    #[test]
    fn similar_voices_do_not_flap() {
      let mut clock = Clock::new();
      let mut selector = selector(1);
      let mut changes = 0;
      // Two speakers alternate between 30 and 33 dBov, within the 6 dB margin.
      for round in 0..100 {
        let (a, b) = if round % 2 == 0 { (30, 33) } else { (33, 30) };
        changes += usize::from(selector.record(device(1), level(a), clock.now).is_some());
        changes += usize::from(selector.record(device(2), level(b), clock.now).is_some());
        clock.advance(STEP);
      }
      assert_eq!(changes, 1, "only the initial selection");
    }

    #[test]
    fn clearly_louder_speaker_takes_over_in_both_directions() {
      let mut clock = Clock::new();
      let mut selector = selector(1);
      // 1 fills the free slot first, but 2 is 30 dB louder.
      for _ in 0..50 {
        selector.record(device(1), level(50), clock.now);
        selector.record(device(2), level(20), clock.now);
        clock.advance(STEP);
      }
      assert_eq!(selector.selected(), devices(&[2]));
      for _ in 0..50 {
        selector.record(device(1), level(10), clock.now);
        selector.record(device(2), level(40), clock.now);
        clock.advance(STEP);
      }
      assert_eq!(selector.selected(), devices(&[1]));
    }

    #[test]
    fn selection_is_not_recomputed_on_every_packet() {
      let mut clock = Clock::new();
      let mut selector = selector(1);
      selector.record(device(1), level(60), clock.now);
      selector.record(device(2), level(0), clock.now);
      clock.advance(Duration::from_millis(50));
      // Inside the interval: no change yet, even though 2 is much louder.
      assert_eq!(selector.record(device(2), level(0), clock.now), None);
      clock.advance(Duration::from_millis(50));
      assert_eq!(
        selector.record(device(2), level(0), clock.now),
        Some(devices(&[2]))
      );
    }
  }

  struct Clock {
    now: Instant,
  }

  impl Clock {
    fn new() -> Self {
      Self { now: Instant::now() }
    }

    fn advance(&mut self, by: Duration) {
      self.now += by;
    }
  }

  fn selector(max_speakers: usize) -> SpeakerSelector {
    SpeakerSelector::new(SpeakerSelectionConfig {
      max_speakers,
      ..SpeakerSelectionConfig::default()
    })
  }

  fn level(level_dbov: u8) -> AudioLevel {
    AudioLevel::new(level_dbov, true).unwrap()
  }

  fn devices(values: &[u64]) -> Vec<DeviceId> {
    values.iter().copied().map(device).collect()
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
