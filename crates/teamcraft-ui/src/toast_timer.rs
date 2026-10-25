//! Countdowns of toasts that close by themselves (#367): "Link copied" and the Undo window of a
//! deletion. They pause while the pointer is on the toasts, while one of them has keyboard focus and
//! while the page is hidden, so nobody loses an Undo under the cursor (WCAG 2.2.1). The clock is
//! passed in; `AppState` drives this with one browser timer for the nearest deadline.

use std::collections::HashMap;
use std::time::Duration;

use leptos::leptos_dom::helpers::TimeoutHandle;

use crate::model::TimestampMs;

/// What happens when a toast ends, by its countdown or by being closed: an Undo toast commits the
/// deletion or the leaving it held back.
pub type OnToastEnd = Box<dyn FnOnce()>;

/// All running countdowns, paused together while any [`Hold`] is on.
#[derive(Debug, Default)]
pub struct ToastTimers {
  countdowns: Vec<Countdown>,
  holds: Holds,
  /// Since when the countdowns run; `None` while held.
  maybe_running_since: Option<TimestampMs>,
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
  /// The tab is in the background or the window is minimized.
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
  is_hidden: bool,
}

impl ToastTimers {
  /// Starts the countdown of `toast`; it runs at once unless a hold is on.
  pub fn start(&mut self, toast: u64, duration: Duration, now: TimestampMs) {
    self.settle(now);
    let remaining_ms = TimestampMs::try_from(duration.as_millis()).unwrap_or(TimestampMs::MAX);
    self.countdowns.push(Countdown { toast, remaining_ms });
    self.resume_if_free(now);
  }

  /// The toast closed some other way (its close button, Undo).
  pub fn cancel(&mut self, toast: u64) {
    self.countdowns.retain(|countdown| countdown.toast != toast);
  }

  /// Turns one hold on or off; the countdowns run only while none is on.
  pub fn set_hold(&mut self, hold: Hold, is_on: bool, now: TimestampMs) {
    self.settle(now);
    let flag = match hold {
      Hold::Pointer => &mut self.holds.is_pointer,
      Hold::Focus => &mut self.holds.is_focus,
      Hold::Hidden => &mut self.holds.is_hidden,
    };
    *flag = is_on;
    if self.holds.is_any() {
      self.maybe_running_since = None;
    } else {
      self.resume_if_free(now);
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

  /// When the next countdown runs out, or `None` while held or with nothing to count.
  pub fn next_deadline(&self) -> Option<TimestampMs> {
    let since = self.maybe_running_since?;
    self
      .countdowns
      .iter()
      .map(|countdown| since.saturating_add(countdown.remaining_ms))
      .min()
  }

  /// Books the time run since the last change. A clock that went backwards counts as no time.
  fn settle(&mut self, now: TimestampMs) {
    let Some(since) = self.maybe_running_since else {
      return;
    };
    let elapsed = now.saturating_sub(since).max(0);
    for countdown in &mut self.countdowns {
      countdown.remaining_ms = countdown.remaining_ms.saturating_sub(elapsed);
    }
    self.maybe_running_since = Some(now);
  }

  fn resume_if_free(&mut self, now: TimestampMs) {
    if !self.holds.is_any() && self.maybe_running_since.is_none() {
      self.maybe_running_since = Some(now);
    }
  }
}

impl ToastClock {
  /// Forgets the countdown of `toast` and hands out what its end does.
  pub fn end(&mut self, toast: u64) -> Option<OnToastEnd> {
    self.timers.cancel(toast);
    self.on_end.remove(&toast)
  }
}

impl Holds {
  fn is_any(&self) -> bool {
    self.is_pointer || self.is_focus || self.is_hidden
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
    fn a_hidden_page_does_not_eat_the_countdown() {
      let mut timers = ToastTimers::default();
      timers.start(1, UNDO, 0);
      timers.set_hold(Hold::Hidden, true, 1_000);
      timers.set_hold(Hold::Hidden, false, 3_600_000);
      assert_eq!(timers.next_deadline(), Some(3_605_000));
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
