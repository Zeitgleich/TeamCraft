//! Errors the browser raises about itself, which say nothing about the app (issue #1760): they are
//! not reported, only counted.
//!
//! The one that mattered: a `ResizeObserver` callback that changes layout so that another resize
//! is due in the same frame. The browser then reports "ResizeObserver loop completed with
//! undelivered notifications." (Chrome before 2021: "ResizeObserver loop limit exceeded") through
//! `window.onerror`, without a stack or a script line. The notification arrives in the next frame;
//! nothing is lost. Reported, it became about thirty issues in the backend, each with the page it
//! happened on as its culprit.

/// Parts of the messages of the benign error, lower case: with or without a full stop or a prefix
/// such as "Uncaught".
const RESIZE_OBSERVER_LOOP: [&str; 2] = [
  "resizeobserver loop completed with undelivered notifications",
  "resizeobserver loop limit exceeded",
];

/// Whether `message` of an uncaught script error is the browser's ResizeObserver loop notice.
pub fn is_resize_observer_loop(message: &str) -> bool {
  let message = message.to_ascii_lowercase();
  RESIZE_OBSERVER_LOOP.iter().any(|known| message.contains(known))
}

/// How many such errors a page life saw; nothing is sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowserNoiseCount {
  resize_observer_loops: u32,
}

impl BrowserNoiseCount {
  /// Counts `message` if it is browser noise; `true` when it was (and must not be reported).
  pub fn absorb(&mut self, message: &str) -> bool {
    if !is_resize_observer_loop(message) {
      return false;
    }
    self.resize_observer_loops = self.resize_observer_loops.saturating_add(1);
    true
  }

  pub fn resize_observer_loops(&self) -> u32 {
    self.resize_observer_loops
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn both_wordings_of_the_resize_observer_notice_are_noise() {
    assert!(is_resize_observer_loop(
      "ResizeObserver loop completed with undelivered notifications."
    ));
    assert!(is_resize_observer_loop(
      "Uncaught ResizeObserver loop completed with undelivered notifications"
    ));
    assert!(is_resize_observer_loop("ResizeObserver loop limit exceeded"));
  }

  #[test]
  fn other_errors_are_not_noise() {
    assert!(!is_resize_observer_loop("Script error."));
    assert!(!is_resize_observer_loop("ReferenceError: x is not defined"));
    assert!(!is_resize_observer_loop(""));
  }

  #[test]
  fn noise_is_counted_and_real_errors_pass() {
    let mut count = BrowserNoiseCount::default();
    assert!(count.absorb("ResizeObserver loop limit exceeded"));
    assert!(count.absorb("ResizeObserver loop completed with undelivered notifications."));
    assert!(!count.absorb("TypeError: undefined is not a function"));
    assert_eq!(count.resize_observer_loops(), 2);
  }
}
