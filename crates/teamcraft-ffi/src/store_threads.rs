//! Waiting for the threads that hold the local store when the app's handle goes (#1980).
//!
//! SQLCipher, which the workspace links whenever teamcraft-core is built with its default
//! `sqlcipher` feature, frees its global state (private heap, providers, static mutexes) in an
//! `atexit` handler and a `.fini_array` finalizer, without waiting for other threads. The engine
//! thread and the read threads close their SQLite connections only after the last handle is
//! dropped; a process that exits meanwhile (a test returning from `main`) has them free memory into
//! that torn-down state, and glibc aborts with "double free or corruption". So dropping the handle
//! waits, for a bounded time, until those threads have let go of the store.
//!
//! The same wait closes the store while the iOS app is suspended (#2179): a suspended app that
//! still has the database open in the App Group container is killed (0xdead10cc).

use std::cell::Cell;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest dropping the handle waits for the store's threads: closing the connections takes
/// milliseconds, ending a running sync loop a little more. Past it the handle goes anyway; an app
/// that keeps running is unaffected, the race only matters while the process exits.
pub(crate) const MAX_STORE_CLOSE_WAIT: Duration = Duration::from_secs(2);
/// The longest opening the store again after a suspension (#2179) waits for the threads before to
/// let go: a job they still ran (a long download) ends with its own timeout; past this the call
/// fails and the next one tries again.
pub(crate) const MAX_STORE_REOPEN_WAIT: Duration = Duration::from_secs(10);

thread_local! {
  /// Set on the threads that hold the store: a handle dropped on one of them can't wait for itself.
  static HOLDS_STORE: Cell<bool> = const { Cell::new(false) };
}

/// The threads of one handle that hold the local store, and the wait for them to end. The threads
/// opened between two closes are one generation: suspending the store (#2179) waits for the old one
/// while the next opening starts a new one.
pub(crate) struct StoreThreads {
  generation: Mutex<Generation>,
}

/// The holds of the threads opened since the last close.
struct Generation {
  /// The handle's own hold, cloned for every thread it starts; let go when they are closed. `None`
  /// once the handle went: a later thread gets a hold nobody waits for.
  maybe_hold: Option<StoreHold>,
  released: StoreClosing,
}

/// The wait for one generation of the store's threads to let go of it.
#[derive(Clone)]
pub(crate) struct StoreClosing {
  /// Disconnects once every hold of the generation is gone. Nothing is ever sent on it.
  released: Arc<Mutex<mpsc::Receiver<()>>>,
}

/// Kept by a thread for as long as it holds the store. Dropping it, after the store, is the signal.
#[derive(Clone)]
pub(crate) struct StoreHold {
  _release: mpsc::Sender<()>,
}

/// How [`StoreClosing::wait`] ended.
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
    Self {
      generation: Mutex::new(Generation::new()),
    }
  }

  /// A hold for a thread about to open the store. After the handle went, a hold nobody waits for.
  pub(crate) fn hold(&self) -> StoreHold {
    self.lock().maybe_hold.clone().unwrap_or_else(StoreHold::detached)
  }

  /// Lets go of the handle's hold on the threads opened so far and starts a new generation for
  /// the threads opened next (#2179): the answer waits for the old threads only. The caller has
  /// closed their queues before, so they are ending.
  pub(crate) fn close(&self) -> StoreClosing {
    std::mem::replace(&mut *self.lock(), Generation::new()).released
  }

  /// Lets go of the handle's own hold for good and waits until every thread let go of theirs, at
  /// most `max_wait`. The caller has closed the threads' queues before, so they are ending.
  pub(crate) fn wait_until_closed(&self, max_wait: Duration) -> StoreClose {
    let closing = std::mem::replace(&mut *self.lock(), Generation::ended()).released;
    closing.wait(max_wait)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Generation> {
    self.generation.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl Generation {
  fn new() -> Self {
    let (hold, released) = mpsc::channel();
    Self {
      maybe_hold: Some(StoreHold { _release: hold }),
      released: StoreClosing {
        released: Arc::new(Mutex::new(released)),
      },
    }
  }

  /// No hold: the threads of no later opening are waited for.
  fn ended() -> Self {
    let (_, released) = mpsc::channel();
    Self {
      maybe_hold: None,
      released: StoreClosing {
        released: Arc::new(Mutex::new(released)),
      },
    }
  }
}

impl StoreClosing {
  /// Waits until every thread of the generation let go of the store, at most `max_wait`.
  pub(crate) fn wait(&self, max_wait: Duration) -> StoreClose {
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
          tracing::warn!("a thread still held the local store when the wait ran out");
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

  /// #2179: suspending waits for the threads opened before; the store's next opening is a
  /// generation of its own, waited for by the close after it.
  #[test]
  fn a_close_waits_for_the_threads_before_and_starts_a_new_generation() {
    let threads = StoreThreads::new();
    let before = threads.hold();
    let closing = threads.close();
    assert_eq!(closing.wait(Duration::from_millis(10)), StoreClose::TimedOut);
    let after = threads.hold();
    drop(before);
    assert_eq!(closing.wait(Duration::from_secs(60)), StoreClose::Closed);
    let next = threads.close();
    assert_eq!(next.wait(Duration::from_millis(10)), StoreClose::TimedOut);
    drop(after);
    assert_eq!(next.wait(Duration::from_secs(60)), StoreClose::Closed);
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
