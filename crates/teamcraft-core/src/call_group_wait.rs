//! The bounded wait for this device's way into a call's encryption group (#1990).
//!
//! A meeting guest a host just let in, or a member just added to a meeting chat, joins the call
//! while the admitter's commit or the welcome is still on its way: preparing the call finds no
//! group yet ([`crate::error::CoreError::CallEncryptionPending`]). That race lasts seconds, so the
//! app waits for the welcome instead of failing at once: it prepares again whenever the core saw a
//! change (a welcome on the gateway joins the group there) and, in case none comes through the
//! gateway, after a pause that doubles up to [`CallGroupWaitSettings::max_pause`] (each attempt
//! asks the server for waiting welcomes). After [`CallGroupWaitSettings::bound`] the failure stands
//! and the user can retry.
//!
//! The wait never founds a group: preparing a call doesn't either (only the device that scheduled a
//! meeting founds its chat's group, e2ee.md §8).

use std::time::Duration;

use crate::clock::Clock;
use crate::retry::millis;

/// How long the app waits for the welcome, and how often it asks the server meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallGroupWaitSettings {
  /// The longest wait for the group after the first attempt found none; then the failure stands.
  pub bound: Duration,
  /// The pause before the second attempt; it doubles with each further one.
  pub first_pause: Duration,
  /// The longest pause between two attempts.
  pub max_pause: Duration,
}

/// One join's wait: when the next attempt is due, and when to give up.
pub struct CallGroupWait<'c> {
  clock: &'c dyn Clock,
  settings: CallGroupWaitSettings,
  started_ms: i64,
  next_pause: Duration,
}

impl Default for CallGroupWaitSettings {
  fn default() -> Self {
    Self {
      bound: Duration::from_secs(30),
      first_pause: Duration::from_millis(250),
      max_pause: Duration::from_secs(4),
    }
  }
}

impl<'c> CallGroupWait<'c> {
  /// Starts the wait now, after the first attempt found no group.
  pub fn start(clock: &'c dyn Clock, settings: CallGroupWaitSettings) -> Self {
    Self {
      clock,
      settings,
      started_ms: clock.now_ms(),
      next_pause: settings.first_pause,
    }
  }

  /// The longest pause before the next attempt (a change of the core's data ends it sooner), or
  /// `None` once the bound has passed and the failure stands.
  pub fn next_pause(&mut self) -> Option<Duration> {
    let now_ms = self.clock.now_ms();
    // The clock went back: the wait counts from now, it doesn't grow by the jump.
    if now_ms < self.started_ms {
      self.started_ms = now_ms;
    }
    let left_ms = millis(self.settings.bound).saturating_sub(now_ms - self.started_ms);
    if left_ms <= 0 {
      return None;
    }
    let left = Duration::from_millis(u64::try_from(left_ms).unwrap_or(0));
    let pause = self.next_pause.min(left);
    self.next_pause = (self.next_pause * 2).min(self.settings.max_pause);
    Some(pause)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::clock::ManualClock;

  #[test]
  fn pauses_double_up_to_the_cap_and_end_at_the_bound() {
    let clock = ManualClock::new(1_000);
    let mut wait = CallGroupWait::start(&clock, CallGroupWaitSettings::default());
    let mut pauses = Vec::new();
    while let Some(pause) = wait.next_pause() {
      pauses.push(pause.as_millis());
      clock.advance(i64::try_from(pause.as_millis()).unwrap());
    }
    assert_eq!(&pauses[..6], &[250, 500, 1_000, 2_000, 4_000, 4_000]);
    assert_eq!(
      pauses.iter().sum::<u128>(),
      30_000,
      "the last pause ends at the bound"
    );
  }

  #[test]
  fn a_clock_that_goes_back_does_not_lengthen_the_wait() {
    let clock = ManualClock::new(100_000);
    let mut wait = CallGroupWait::start(&clock, CallGroupWaitSettings::default());
    clock.advance(-60_000);
    assert!(wait.next_pause().is_some());
    clock.advance(30_000);
    assert_eq!(wait.next_pause(), None);
  }
}
