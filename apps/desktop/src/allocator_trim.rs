//! Linux: hands the shell's freed heap back to the system when the main window goes to the
//! background (focus lost, hidden to the tray). glibc keeps pages freed in the middle of its heap,
//! so what a catch-up or a busy hour allocated stays with the process; `malloc_trim` gives it back
//! (18 of 85 MiB anonymous in `tests/e2e` `desktop_perf.rs`; docs/performance.md, "Desktop on
//! Linux: where the memory is"; #1708). The background is where it pays: the user works in other apps
//! then, and nothing in this one waits for the few milliseconds it takes.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// At most one trim per this long: focus goes back and forth all day, and a heap that was just
/// trimmed has little to give back.
const MIN_INTERVAL: Duration = Duration::from_secs(60);

/// When the last trim started; `None` before the first.
static LAST_TRIM: Mutex<Option<Instant>> = Mutex::new(None);

/// Trims the heap on a thread of its own, unless that happened within [`MIN_INTERVAL`].
pub fn trim_in_background() {
  if !is_due(Instant::now()) {
    return;
  }
  let spawned = std::thread::Builder::new()
    .name("heap-trim".to_owned())
    .spawn(|| {
      let started = Instant::now();
      let was_released = teamcraft_desktop_native::trim_allocator();
      tracing::debug!(
        was_released,
        elapsed_ms = started.elapsed().as_millis(),
        "trimmed the heap"
      );
    });
  if let Err(error) = spawned {
    tracing::debug!(%error, "could not start the heap trim");
  }
}

/// Whether a trim may start at `now`; records it when it may.
fn is_due(now: Instant) -> bool {
  let Ok(mut last) = LAST_TRIM.lock() else {
    // A panic while holding the lock left only a timestamp behind; skipping one trim is harmless.
    return false;
  };
  record_if_due(&mut last, now)
}

fn record_if_due(last: &mut Option<Instant>, now: Instant) -> bool {
  let is_due = last.is_none_or(|last| now.saturating_duration_since(last) >= MIN_INTERVAL);
  if is_due {
    *last = Some(now);
  }
  is_due
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_first_trim_is_due_and_the_next_waits_for_the_interval() {
    let start = Instant::now();
    let mut last = None;
    assert!(record_if_due(&mut last, start));
    assert!(!record_if_due(&mut last, start + Duration::from_secs(5)));
    assert!(!record_if_due(
      &mut last,
      start + MIN_INTERVAL - Duration::from_millis(1)
    ));
    assert!(record_if_due(&mut last, start + MIN_INTERVAL));
    assert_eq!(last, Some(start + MIN_INTERVAL));
  }
}
