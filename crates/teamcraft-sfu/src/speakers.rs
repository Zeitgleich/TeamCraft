//! Selection of the loudest speakers from the unencrypted audio level extension.
//!
//! Each packet's loudness feeds an exponential moving average per participant, so a single loud
//! click doesn't steal a slot. Participants that stopped sending for `timeout`, or whose average is
//! pure silence, are never selected. Time is passed in, which keeps the logic deterministic.

use std::{
  collections::HashMap,
  time::{Duration, Instant},
};

use teamcraft_media::rtp::AudioLevel;

use crate::ids::DeviceId;

/// Weight of a new sample in the moving average (out of 1.0).
const SMOOTHING_FACTOR: f32 = 0.2;
/// Averages at or below this loudness count as silence.
const SILENCE_LOUDNESS: f32 = 0.5;

/// Tracks recent loudness per participant and picks the top `max_speakers`.
#[derive(Debug)]
pub struct SpeakerSelector {
  max_speakers: usize,
  timeout: Duration,
  speakers: HashMap<DeviceId, SpeakerState>,
}

#[derive(Debug, Clone, Copy)]
struct SpeakerState {
  average_loudness: f32,
  last_packet_at: Instant,
}

impl SpeakerSelector {
  pub fn new(max_speakers: usize, timeout: Duration) -> Self {
    Self {
      max_speakers,
      timeout,
      speakers: HashMap::new(),
    }
  }

  /// Records one audio packet's level.
  pub fn record(&mut self, participant_id: DeviceId, level: AudioLevel, now: Instant) {
    let sample = f32::from(level.loudness());
    self
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
  }

  pub fn remove(&mut self, participant_id: DeviceId) {
    self.speakers.remove(&participant_id);
  }

  /// Whether `participant_id` is currently among the loudest speakers.
  pub fn is_selected(&self, participant_id: DeviceId, now: Instant) -> bool {
    self.selected(now).contains(&participant_id)
  }

  /// The loudest active speakers, loudest first. Ties go to the lower participant id so the
  /// result is stable.
  pub fn selected(&self, now: Instant) -> Vec<DeviceId> {
    let mut candidates: Vec<(DeviceId, f32)> = self
      .speakers
      .iter()
      .filter(|(_, state)| {
        now.saturating_duration_since(state.last_packet_at) < self.timeout
          && state.average_loudness > SILENCE_LOUDNESS
      })
      .map(|(&id, state)| (id, state.average_loudness))
      .collect();
    candidates.sort_by(|(a_id, a_loudness), (b_id, b_loudness)| {
      b_loudness.total_cmp(a_loudness).then(a_id.cmp(b_id))
    });
    candidates
      .into_iter()
      .take(self.max_speakers)
      .map(|(id, _)| id)
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const TIMEOUT: Duration = Duration::from_secs(1);

  #[test]
  fn picks_the_loudest() {
    let now = Instant::now();
    let mut selector = SpeakerSelector::new(3, TIMEOUT);
    for (id, level_dbov) in [(1, 60), (2, 10), (3, 30), (4, 20), (5, 90)] {
      selector.record(device(id), level(level_dbov), now);
    }
    assert_eq!(selector.selected(now), ids(&[2, 4, 3]));
    assert!(!selector.is_selected(device(1), now));
  }

  #[test]
  fn silence_is_never_selected() {
    let now = Instant::now();
    let mut selector = SpeakerSelector::new(3, TIMEOUT);
    selector.record(device(1), AudioLevel::SILENCE, now);
    selector.record(device(2), level(100), now);
    assert_eq!(selector.selected(now), ids(&[2]));
  }

  #[test]
  fn stale_speakers_time_out() {
    let start = Instant::now();
    let mut selector = SpeakerSelector::new(3, TIMEOUT);
    selector.record(device(1), level(10), start);
    selector.record(device(2), level(50), start + Duration::from_millis(900));
    assert_eq!(selector.selected(start + Duration::from_millis(1100)), ids(&[2]));
  }

  #[test]
  fn single_spike_does_not_displace_a_steady_speaker() {
    let now = Instant::now();
    let mut selector = SpeakerSelector::new(1, TIMEOUT);
    for _ in 0..20 {
      selector.record(device(1), level(20), now);
      selector.record(device(2), level(80), now);
    }
    selector.record(device(2), level(0), now);
    assert_eq!(selector.selected(now), ids(&[1]));
  }

  #[test]
  fn ties_prefer_lower_id_and_removed_participants_vanish() {
    let now = Instant::now();
    let mut selector = SpeakerSelector::new(1, TIMEOUT);
    selector.record(device(9), level(20), now);
    selector.record(device(3), level(20), now);
    assert_eq!(selector.selected(now), ids(&[3]));
    selector.remove(device(3));
    assert_eq!(selector.selected(now), ids(&[9]));
  }

  fn level(level_dbov: u8) -> AudioLevel {
    AudioLevel::new(level_dbov, true).unwrap()
  }

  fn ids(values: &[u64]) -> Vec<DeviceId> {
    values.iter().copied().map(device).collect()
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
