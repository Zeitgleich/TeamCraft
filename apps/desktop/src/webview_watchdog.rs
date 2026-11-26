//! A page whose web process hangs must not leave a frozen window (#2450, robustness.md "No dead
//! app"). WebKitGTK 2.54 can deadlock its web process while releasing memory: the window then takes
//! no input, nothing redraws, and only killing the app helped. WebKit itself never notices.
//!
//! The shell asks the page a trivial question every [`PING_INTERVAL`] while the window is shown.
//! A page that leaves [`MISSED_PINGS_TO_HANG`] intervals in a row without an answer is taken as
//! hung: its web process is terminated, and `webview_recovery.rs` loads the page again, as after
//! any other dead web process. A page that is merely busy (a long task, a slow machine) answers
//! within that time; a hidden window isn't asked, so nothing runs while nobody looks, and a hang
//! there is found within the same time once the window is shown.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::WidgetExt;
use webkit2gtk::WebViewExt;

use crate::crash_reports;

/// How often a shown page is asked. A page answers in milliseconds; this only bounds how long a
/// hang lasts before it is found.
const PING_INTERVAL: Duration = Duration::from_secs(10);
/// Intervals in a row without an answer after which the page counts as hung: a hang lasts between
/// 20 and 30 s before the page loads again, far longer than any task of the page that still runs.
const MISSED_PINGS_TO_HANG: u32 = 2;
/// The question: any answer means the web process's main thread runs.
const PING_SCRIPT: &str = "0";

/// Asks the page whether it still runs while the webview is shown, and ends its web process when it
/// stops answering. Call it once per webview, on the main thread.
pub fn watch(webview: &webkit2gtk::WebView) {
  let watchdog = Rc::new(RefCell::new(Watchdog::default()));
  let weak_webview = glib::object::ObjectExt::downgrade(webview);
  glib::timeout_add_local(PING_INTERVAL, move || {
    // The window was closed or made anew (webview_recovery.rs): its own watch takes over.
    let Some(webview) = weak_webview.upgrade() else {
      return glib::ControlFlow::Break;
    };
    let step = watchdog.borrow_mut().tick(webview.is_mapped());
    match step {
      Step::Wait => {},
      Step::Ping(ping) => {
        let watchdog = Rc::clone(&watchdog);
        // An error (no page, a page that is gone or loading) is an answer too: a hung web process
        // gives none.
        webview.evaluate_javascript(
          PING_SCRIPT,
          None,
          None,
          None::<&gtk::gio::Cancellable>,
          move |_| {
            watchdog.borrow_mut().answered(ping);
          },
        );
      },
      Step::Hung => end_hung_page(&webview),
    }
    glib::ControlFlow::Continue
  });
}

/// Terminates the hung page's web process, which `webview_recovery::watch` sees as a dead one and
/// reloads. Reported on the next start of the page, like a panic of the shell.
fn end_hung_page(webview: &webkit2gtk::WebView) {
  let unanswered_for = PING_INTERVAL * MISSED_PINGS_TO_HANG;
  tracing::error!(
    unanswered_for_secs = unanswered_for.as_secs(),
    "the webview's page stopped answering; ending its web process"
  );
  crash_reports::record_problem(&format!(
    "webview hang: the page answered nothing for {} s, its web process was terminated",
    unanswered_for.as_secs()
  ));
  webview.terminate_web_process();
}

/// What the timer does on a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
  /// Nothing to ask now.
  Wait,
  /// Ask the page; the answer names this ping.
  Ping(PingId),
  /// The page didn't answer for [`MISSED_PINGS_TO_HANG`] intervals.
  Hung,
}

/// Which ping an answer belongs to: an answer from before a hang or a reload doesn't count for the
/// ping after it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PingId(u64);

/// The state between ticks: one ping at most is open at a time.
#[derive(Debug, Default)]
struct Watchdog {
  maybe_open: Option<PingId>,
  missed: u32,
  last: PingId,
}

impl Watchdog {
  /// One tick of the timer; `is_shown` is whether anybody can see the page.
  fn tick(&mut self, is_shown: bool) -> Step {
    // Hidden: no new question, and the time doesn't count (a laptop's lid, the tray).
    if !is_shown {
      self.missed = 0;
      return Step::Wait;
    }
    if self.maybe_open.is_none() {
      self.missed = 0;
      self.last = PingId(self.last.0.wrapping_add(1));
      self.maybe_open = Some(self.last);
      return Step::Ping(self.last);
    }
    self.missed += 1;
    if self.missed < MISSED_PINGS_TO_HANG {
      return Step::Wait;
    }
    // The next page starts afresh; the hung ping's late error (the process is gone) is ignored.
    self.maybe_open = None;
    self.missed = 0;
    Step::Hung
  }

  /// The page answered `ping`.
  fn answered(&mut self, ping: PingId) {
    if self.maybe_open == Some(ping) {
      self.maybe_open = None;
      self.missed = 0;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_page_that_answers_is_never_hung() {
    let mut watchdog = Watchdog::default();
    for _ in 0..100 {
      let Step::Ping(ping) = watchdog.tick(true) else {
        panic!("an answered page is asked again on the next tick");
      };
      watchdog.answered(ping);
    }
  }

  #[test]
  fn a_page_that_misses_two_intervals_is_hung() {
    let mut watchdog = Watchdog::default();
    assert!(matches!(watchdog.tick(true), Step::Ping(_)));
    assert_eq!(watchdog.tick(true), Step::Wait);
    assert_eq!(watchdog.tick(true), Step::Hung);
  }

  #[test]
  fn a_slow_answer_within_the_time_resets_the_count() {
    let mut watchdog = Watchdog::default();
    let Step::Ping(ping) = watchdog.tick(true) else {
      panic!("the first tick asks");
    };
    assert_eq!(watchdog.tick(true), Step::Wait);
    watchdog.answered(ping);
    let Step::Ping(_) = watchdog.tick(true) else {
      panic!("an answered page is asked again");
    };
    assert_eq!(watchdog.tick(true), Step::Wait);
  }

  #[test]
  fn a_hidden_page_is_not_asked_and_its_time_does_not_count() {
    let mut watchdog = Watchdog::default();
    assert_eq!(watchdog.tick(false), Step::Wait);
    assert!(matches!(watchdog.tick(true), Step::Ping(_)));
    assert_eq!(watchdog.tick(true), Step::Wait);
    // Hidden before the second miss: shown again, it gets the whole time once more.
    for _ in 0..10 {
      assert_eq!(watchdog.tick(false), Step::Wait);
    }
    assert_eq!(watchdog.tick(true), Step::Wait);
    assert_eq!(watchdog.tick(true), Step::Hung);
  }

  #[test]
  fn the_hung_pings_late_answer_does_not_count_for_the_next_page() {
    let mut watchdog = Watchdog::default();
    let Step::Ping(hung) = watchdog.tick(true) else {
      panic!("the first tick asks");
    };
    watchdog.tick(true);
    assert_eq!(watchdog.tick(true), Step::Hung);
    let Step::Ping(next) = watchdog.tick(true) else {
      panic!("the reloaded page is asked");
    };
    assert_ne!(hung, next);
    watchdog.answered(hung);
    assert_eq!(watchdog.tick(true), Step::Wait);
    assert_eq!(watchdog.tick(true), Step::Hung);
  }
}
