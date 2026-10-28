//! When the call task's periodic work is due, independent of what wakes it.
//!
//! Page timers (`setInterval`) are throttled in hidden tabs: to once a second, and after five
//! minutes hidden and silent to about once a minute in Chrome. A shared screen would drop to a
//! frame a second while the user looks at what they share. So the call task is woken by pulses
//! from two clocks, the audio clock (an AudioWorklet that posts every few milliseconds of audio,
//! which no browser throttles while the call's audio runs) and a page timer (the fallback, should
//! the audio clock stop), and on each pulse every [`Cadence`] says how many of its ticks fell due
//! since the last one. Duplicate or early pulses are harmless: a cadence runs on its own grid.

use std::time::Duration;

use web_time::Instant;

/// One periodic job's schedule: due every `period` on a fixed grid from its start, so a pulse
/// arriving late doesn't shift the rate.
#[derive(Debug, Clone)]
pub(super) struct Cadence {
  period: Duration,
  next_due: Instant,
  max_per_pulse: u32,
}

impl Cadence {
  /// First due one `period` after `now`. A pulse that comes after several periods runs at most
  /// `max_per_pulse` ticks (audio catches up on frames it owes the playback; video and timers run
  /// once and skip the rest).
  pub fn new(period: Duration, max_per_pulse: u32, now: Instant) -> Self {
    let period = period.max(Duration::from_millis(1));
    Self {
      period,
      next_due: now + period,
      max_per_pulse: max_per_pulse.max(1),
    }
  }

  /// A cadence for `per_second` ticks a second that never catches up (video capture).
  pub fn per_second(per_second: u8, now: Instant) -> Self {
    Self::new(Duration::from_secs(1) / u32::from(per_second.max(1)), 1, now)
  }

  /// How many ticks are due at `now` (0 before the next one); those beyond `max_per_pulse` are
  /// skipped. The grid moves past every tick counted or skipped.
  pub fn due(&mut self, now: Instant) -> u32 {
    if now < self.next_due {
      return 0;
    }
    let behind = now.duration_since(self.next_due);
    let missed = behind.as_nanos() / self.period.as_nanos();
    let elapsed = u32::try_from(missed.saturating_add(1)).unwrap_or(u32::MAX);
    // Far behind (a page asleep for minutes): restart the grid from now instead of adding up.
    self.next_due = match self.period.checked_mul(elapsed) {
      Some(step) => self.next_due + step,
      None => now + self.period,
    };
    elapsed.min(self.max_per_pulse)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const FRAME: Duration = Duration::from_millis(20);

  #[test]
  fn nothing_is_due_before_the_first_period() {
    let start = Instant::now();
    let mut cadence = Cadence::new(FRAME, 5, start);
    assert_eq!(cadence.due(start), 0);
    assert_eq!(cadence.due(start + ms(19)), 0);
    assert_eq!(cadence.due(start + ms(20)), 1);
    assert_eq!(cadence.due(start + ms(20)), 0);
  }

  #[test]
  fn duplicate_pulses_from_two_clocks_tick_once() {
    let start = Instant::now();
    let mut cadence = Cadence::new(FRAME, 5, start);
    // The audio clock and the page timer both pulse every 10 ms, a little apart.
    let mut ticks = 0;
    for step in 1..=100 {
      ticks += cadence.due(start + ms(step * 10));
      ticks += cadence.due(start + ms(step * 10 + 3));
    }
    assert_eq!(ticks, 50);
  }

  #[test]
  fn late_pulses_keep_the_rate_on_a_coarse_pulse() {
    let start = Instant::now();
    // 30 fps video on 10 ms pulses: ticks land on the next pulse, the rate stays 30 a second.
    let mut video = Cadence::per_second(30, start);
    let ticks: u32 = (1..=100).map(|step| video.due(start + ms(step * 10))).sum();
    assert_eq!(ticks, 30);
  }

  #[test]
  fn audio_catches_up_a_bounded_number_of_frames() {
    let start = Instant::now();
    let mut cadence = Cadence::new(FRAME, 5, start);
    // A pulse 70 ms late owes three frames, all of them played.
    assert_eq!(cadence.due(start + ms(60)), 3);
    // A stall of a second owes 50; at most five catch up, the rest is skipped.
    assert_eq!(cadence.due(start + ms(1060)), 5);
    assert_eq!(cadence.due(start + ms(1070)), 0);
    assert_eq!(cadence.due(start + ms(1080)), 1);
  }

  #[test]
  fn video_skips_frames_it_missed_instead_of_bursting() {
    let start = Instant::now();
    let mut video = Cadence::per_second(30, start);
    // A throttled page timer once a second: one capture, not thirty at once.
    assert_eq!(video.due(start + ms(1000)), 1);
    assert_eq!(video.due(start + ms(1001)), 0);
    assert_eq!(video.due(start + ms(1034)), 1);
  }

  #[test]
  fn a_page_asleep_for_hours_resumes_at_the_normal_rate() {
    let start = Instant::now();
    let mut timer = Cadence::new(Duration::from_millis(100), 1, start);
    let later = start + Duration::from_secs(6 * 3600);
    assert_eq!(timer.due(later), 1);
    assert_eq!(timer.due(later + ms(50)), 0);
    assert_eq!(timer.due(later + ms(100)), 1);
  }

  #[test]
  fn a_zero_rate_or_period_still_ticks() {
    let start = Instant::now();
    let mut once_a_second = Cadence::per_second(0, start);
    assert_eq!(once_a_second.due(start + ms(999)), 0);
    assert_eq!(once_a_second.due(start + ms(1000)), 1);
    let mut fastest = Cadence::new(Duration::ZERO, 0, start);
    assert_eq!(fastest.due(start + ms(1)), 1);
  }

  fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
  }
}
