//! Waiting for the threads that hold the local store when the app's handle goes (#1980).
//!
//! SQLCipher, which the workspace links whenever teamcraft-core is built with its default
//! `sqlcipher` feature, frees its global state (private heap, providers, static mutexes) in an
//! `atexit` handler and a `.fini_array` finalizer, without waiting for other threads. The engine
//! thread and the read threads close their SQLite connections only after the last handle is
//! dropped; a process that exits meanwhile (a test returning from `main`) has them free memory into
//! that torn-down state, and glibc aborts with "double free or corruption". So dropping the handle
//! waits, for a bounded time, until those threads have let go of the store.

use std::cell::Cell;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest dropping the handle waits for the store's threads: closing the connections takes
/// milliseconds, ending a running sync loop a little more. Past it the handle goes anyway; an app
/// that keeps running is unaffected, the race only matters while the process exits.
pub(crate) const MAX_STORE_CLOSE_WAIT: Duration = Duration::from_secs(2);

thread_local! {
  /// Set on the threads that hold the store: a handle dropped on one of them can't wait for itself.
  static HOLDS_STORE: Cell<bool> = const { Cell::new(false) };
}

/// The threads of one handle that hold the local store, and the wait for them to end.
pub(crate) struct StoreThreads {
  /// The handle's own hold, cloned for every thread it starts; let go before the wait.
  maybe_hold: Mutex<Option<StoreHold>>,
  /// Disconnects once every hold is gone. Nothing is ever sent on it.
  released: Mutex<mpsc::Receiver<()>>,
}

/// Kept by a thread for as long as it holds the store. Dropping it, after the store, is the signal.
#[derive(Clone)]
pub(crate) struct StoreHold {
  _release: mpsc::Sender<()>,
}

/// How [`StoreThreads::wait_until_closed`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StoreClose {
  /// Every thread let go of the store.
  Closed,
  /// A thread still held it when the wait ran out.
  TimedOut,
  /// Called on one of the store's own threads, which would wait for itself: no wait.
  OnStoreThread,
}

impl StoreThreads {
  pub(crate) fn new() -> Self {
    let (hold, released) = mpsc::channel();
    Self {
      maybe_hold: Mutex::new(Some(StoreHold { _release: hold })),
      released: Mutex::new(released),
    }
  }

  /// A hold for a thread about to open the store. After the wait began, a hold nobody waits for.
  pub(crate) fn hold(&self) -> StoreHold {
    self
      .maybe_hold
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
      .unwrap_or_else(StoreHold::detached)
  }

  /// Lets go of the handle's own hold and waits until every thread let go of theirs, at most
  /// `max_wait`. The caller has closed the threads' queues before, so they are ending.
  pub(crate) fn wait_until_closed(&self, max_wait: Duration) -> StoreClose {
    drop(
      self
        .maybe_hold
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take(),
    );
    if HOLDS_STORE.with(Cell::get) {
      return StoreClose::OnStoreThread;
    }
    let deadline = Instant::now() + max_wait;
    let released = self.released.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
      let left = deadline.saturating_duration_since(Instant::now());
      match released.recv_timeout(left) {
        // Nothing is sent; a message would only be a hold's mistake, so keep waiting.
        Ok(()) => {},
        Err(RecvTimeoutError::Disconnected) => return StoreClose::Closed,
        Err(RecvTimeoutError::Timeout) => {
          tracing::warn!("a thread still held the local store when the handle went");
          return StoreClose::TimedOut;
        },
      }
    }
  }
}

impl StoreHold {
  /// A hold no handle waits for (tests that start read threads on their own).
  pub(crate) fn detached() -> Self {
    let (hold, _) = mpsc::channel();
    Self { _release: hold }
  }

  /// Marks the calling thread as one that holds the store, for the rest of its life.
  pub(crate) fn enter(self) -> Self {
    HOLDS_STORE.with(|holds| holds.set(true));
    self
  }
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;

  use super::*;

  #[test]
  fn the_wait_ends_once_every_thread_let_go() {
    let threads = StoreThreads::new();
    let (release, released) = mpsc::channel::<()>();
    let holds = [threads.hold(), threads.hold()];
    let worker = std::thread::spawn(move || {
      let _holds = holds;
      // Holds the store until the test lets it go.
      let _ = released.recv();
    });
    drop(release);
    assert_eq!(
      threads.wait_until_closed(Duration::from_secs(60)),
      StoreClose::Closed
    );
    worker.join().unwrap();
  }

  #[test]
  fn a_thread_that_keeps_the_store_ends_the_wait_at_the_limit() {
    let threads = StoreThreads::new();
    let _kept = threads.hold();
    assert_eq!(
      threads.wait_until_closed(Duration::from_millis(10)),
      StoreClose::TimedOut
    );
  }

  #[test]
  fn a_store_thread_does_not_wait_for_itself() {
    let threads = StoreThreads::new();
    let hold = threads.hold();
    let close = std::thread::spawn(move || {
      let _hold = hold.enter();
      threads.wait_until_closed(Duration::from_secs(60))
    });
    assert_eq!(close.join().unwrap(), StoreClose::OnStoreThread);
  }

  #[test]
  fn a_hold_taken_after_the_wait_is_not_waited_for() {
    let threads = StoreThreads::new();
    assert_eq!(
      threads.wait_until_closed(Duration::from_secs(60)),
      StoreClose::Closed
    );
    let _late = threads.hold();
    assert_eq!(
      threads.wait_until_closed(Duration::from_secs(60)),
      StoreClose::Closed
    );
  }
}
