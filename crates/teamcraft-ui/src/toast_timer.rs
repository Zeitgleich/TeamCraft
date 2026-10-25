//! Countdowns of toasts that close by themselves (#367): "Link copied" and the Undo window of a
//! deletion. They pause while the pointer is on the toasts, while one of them has keyboard focus and
//! for a while after the page was hidden, so nobody loses an Undo under the cursor or by switching
//! apps for a moment (WCAG 2.2.1). The clock is passed in; `AppState` drives this with one browser
//! timer for the nearest deadline.

use std::collections::HashMap;
use std::time::Duration;

use leptos::leptos_dom::helpers::TimeoutHandle;

use crate::model::TimestampMs;

/// How long a hidden page holds the countdowns. Longer and a deletion would wait for a tab the
/// system may kill meanwhile: the message would be gone on screen but never deleted.
pub const HIDDEN_HOLD_MS: TimestampMs = 30_000;

/// What happens when a toast ends, by its countdown or by being closed: an Undo toast commits the
/// deletion or the leaving it held back.
pub type OnToastEnd = Box<dyn FnOnce()>;

/// All running countdowns, paused together while a [`Hold`] is on.
#[derive(Debug, Default)]
pub struct ToastTimers {
  countdowns: Vec<Countdown>,
  holds: Holds,
  /// Up to when the countdowns are booked.
  settled_at: TimestampMs,
}

/// [`ToastTimers`] on the browser's clock, kept by `AppState`: one browser timer wakes it for the
/// nearest deadline, none runs while held.
#[derive(Default)]
pub struct ToastClock {
  pub timers: ToastTimers,
  pub on_end: HashMap<u64, OnToastEnd>,
  pub maybe_wake: Option<TimeoutHandle>,
}

/// Why the countdowns stand still.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
  /// The pointer is on the toasts.
  Pointer,
  /// A toast (its Undo, its close button) has keyboard focus.
  Focus,
  /// The tab is in the background or the window is minimized; holds for [`HIDDEN_HOLD_MS`] at most.
  Hidden,
}

#[derive(Debug)]
struct Countdown {
  toast: u64,
  remaining_ms: TimestampMs,
}

#[derive(Debug, Default)]
struct Holds {
  is_pointer: bool,
  is_focus: bool,
  maybe_hidden_since: Option<TimestampMs>,
}

impl ToastTimers {
  /// Starts the countdown of `toast`; it runs at once unless a hold is on.
  pub fn start(&mut self, toast: u64, duration: Duration, now: TimestampMs) {
    self.settle(now);
    let remaining_ms = TimestampMs::try_from(duration.as_millis()).unwrap_or(TimestampMs::MAX);
    self.countdowns.push(Countdown { toast, remaining_ms });
  }

  /// The toast closed some other way (its close button, Undo).
  pub fn cancel(&mut self, toast: u64) {
    self.countdowns.retain(|countdown| countdown.toast != toast);
  }

  /// Turns one hold on or off; the countdowns run only while none is on.
  pub fn set_hold(&mut self, hold: Hold, is_on: bool, now: TimestampMs) {
    self.settle(now);
    match hold {
      Hold::Pointer => self.holds.is_pointer = is_on,
      Hold::Focus => self.holds.is_focus = is_on,
      Hold::Hidden => {
        self.holds.maybe_hidden_since = match (is_on, self.holds.maybe_hidden_since) {
          (true, Some(since)) => Some(since),
          (true, None) => Some(now),
          (false, _) => None,
        }
      },
    }
  }

  /// The toasts whose time ran out by `now`, in the order they were started; they are forgotten.
  pub fn take_expired(&mut self, now: TimestampMs) -> Vec<u64> {
    self.settle(now);
    let (expired, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.countdowns)
      .into_iter()
      .partition(|countdown| countdown.remaining_ms <= 0);
    self.countdowns = running;
    expired.into_iter().map(|countdown| countdown.toast).collect()
  }

  /// When the next countdown runs out, or `None` while held by the pointer or focus or with nothing
  /// to count.
  pub fn next_deadline(&self) -> Option<TimestampMs> {
    let runs_from = self.runs_from()?;
    self
      .countdowns
      .iter()
      .map(|countdown| runs_from.saturating_add(countdown.remaining_ms))
      .min()
  }

  /// Books the time counted since the last change. A clock that went backwards counts as no time.
  fn settle(&mut self, now: TimestampMs) {
    if let Some(runs_from) = self.runs_from() {
      let elapsed = now.saturating_sub(runs_from).max(0);
      for countdown in &mut self.countdowns {
        countdown.remaining_ms = countdown.remaining_ms.saturating_sub(elapsed);
      }
    }
    self.settled_at = now;
  }

  /// From when the countdowns count again with the holds as they are: now, after the hidden hold
  /// ran out, or never while the pointer or focus holds them.
  fn runs_from(&self) -> Option<TimestampMs> {
    if self.holds.is_pointer || self.holds.is_focus {
      return None;
    }
    Some(match self.holds.maybe_hidden_since {
      Some(since) => self.settled_at.max(since.saturating_add(HIDDEN_HOLD_MS)),
      None => self.settled_at,
    })
  }
}

impl ToastClock {
  /// Forgets the countdown of `toast` and hands out what its end does.
  pub fn end(&mut self, toast: u64) -> Option<OnToastEnd> {
    self.timers.cancel(toast);
    self.on_end.remove(&toast)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const UNDO: Duration = Duration::from_secs(6);

  #[test]
  fn a_countdown_runs_out_after_its_duration() {
    let mut timers = ToastTimers::default();
    timers.start(1, UNDO, 1_000);
    assert_eq!(timers.next_deadline(), Some(7_000));
    assert!(timers.take_expired(6_999).is_empty());
    assert_eq!(timers.take_expired(7_000), vec![1]);
    assert_eq!(timers.next_deadline(), None, "an expired toast is forgotten");
  }

  mod holds {
    use super::*;

    #[test]
    fn hovering_stops_the_countdown_and_leaving_resumes_it() {
      let mut timers = ToastTimers::default();
      timers.start(1, UNDO, 0);
      timers.set_hold(Hold::Pointer, true, 2_000);
      assert_eq!(timers.next_deadline(), None);
      assert!(
        timers.take_expired(60_000).is_empty(),
        "no time passes under the pointer"
      );
      timers.set_hold(Hold::Pointer, false, 60_000);
      assert_eq!(timers.next_deadline(), Some(64_000), "4 s were left");
      assert_eq!(timers.take_expired(64_000), vec![1]);
    }

    #[test]
    fn the_countdown_waits_until_every_hold_is_off() {
      let mut timers = ToastTimers::default();
      timers.start(1, UNDO, 0);
      timers.set_hold(Hold::Focus, true, 1_000);
      timers.set_hold(Hold::Pointer, true, 2_000);
      timers.set_hold(Hold::Focus, false, 3_000);
      assert_eq!(timers.next_deadline(), None, "the pointer still holds it");
      timers.set_hold(Hold::Pointer, false, 10_000);
      assert_eq!(timers.next_deadline(), Some(15_000));
    }

    #[test]
    fn a_short_trip_to_another_app_does_not_eat_the_countdown() {
      let mut timers = ToastTimers::default();
      timers.start(1, UNDO, 0);
      timers.set_hold(Hold::Hidden, true, 1_000);
      assert!(timers.take_expired(20_000).is_empty());
      timers.set_hold(Hold::Hidden, false, 25_000);
      assert_eq!(timers.next_deadline(), Some(30_000), "5 s were left");
    }

    /// Review of #387: the system may kill a hidden tab; a deletion must not wait for it forever.
    #[test]
    fn a_long_hidden_page_counts_again_after_the_hidden_hold() {
      let mut timers = ToastTimers::default();
      timers.start(1, UNDO, 0);
      timers.set_hold(Hold::Hidden, true, 1_000);
      assert_eq!(timers.next_deadline(), Some(1_000 + HIDDEN_HOLD_MS + 5_000));
      assert!(timers.take_expired(1_000 + HIDDEN_HOLD_MS + 4_999).is_empty());
      assert_eq!(timers.take_expired(1_000 + HIDDEN_HOLD_MS + 5_000), vec![1]);
    }

    #[test]
    fn a_toast_started_while_held_waits_too() {
      let mut timers = ToastTimers::default();
      timers.set_hold(Hold::Pointer, true, 0);
      timers.start(1, UNDO, 500);
      assert_eq!(timers.next_deadline(), None);
      timers.set_hold(Hold::Pointer, false, 9_000);
      assert_eq!(timers.next_deadline(), Some(15_000));
    }
  }

  #[test]
  fn the_nearest_deadline_comes_first_and_cancelled_toasts_never_expire() {
    let mut timers = ToastTimers::default();
    timers.start(1, UNDO, 0);
    timers.start(2, Duration::from_secs(4), 0);
    assert_eq!(timers.next_deadline(), Some(4_000));
    timers.cancel(2);
    assert_eq!(timers.next_deadline(), Some(6_000));
    assert_eq!(timers.take_expired(10_000), vec![1]);
  }

  #[test]
  fn a_clock_that_went_backwards_counts_as_no_time() {
    let mut timers = ToastTimers::default();
    timers.start(1, UNDO, 10_000);
    assert!(timers.take_expired(5_000).is_empty());
    assert_eq!(timers.next_deadline(), Some(11_000));
  }
}
