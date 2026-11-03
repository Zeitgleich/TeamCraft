//! Threads that are stopped with a bounded wait. A capture or encoder thread can hang in a driver
//! or an OS framework (a camera that never delivers a frame blocks its read for good): whoever
//! stops it must not hang with it, least of all the call's task (issue #683). After the wait the
//! thread is left to finish on its own, and the log says so.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::warn;

/// A thread whose end can be waited for with a bound.
pub(crate) struct WorkerThread {
  name: &'static str,
  handle: JoinHandle<()>,
  /// Disconnects when the thread's body returned or unwound.
  done: mpsc::Receiver<()>,
}

/// How waiting for a [`WorkerThread`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Joined {
  Finished,
  Panicked,
  /// Still running after the wait; left to finish on its own.
  Detached,
}

impl WorkerThread {
  pub(crate) fn spawn(name: &'static str, body: impl FnOnce() + Send + 'static) -> std::io::Result<Self> {
    let (done_guard, done) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new().name(name.into()).spawn(move || {
      // Dropped when the body returns or unwinds: the waiting side sees the channel close.
      let _done_guard = done_guard;
      body();
    })?;
    Ok(Self { name, handle, done })
  }

  pub(crate) fn is_finished(&self) -> bool {
    self.handle.is_finished()
  }

  /// Waits up to `bound` for the thread to end; leaves it running after that.
  pub(crate) fn join_within(self, bound: Duration) -> Joined {
    if let Err(RecvTimeoutError::Timeout) = self.done.recv_timeout(bound) {
      warn!(
        thread = self.name,
        ?bound,
        "thread did not stop in time; left to finish on its own"
      );
      return Joined::Detached;
    }
    match self.handle.join() {
      Ok(()) => Joined::Finished,
      Err(_) => {
        warn!(thread = self.name, "thread panicked");
        Joined::Panicked
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Instant;

  use super::*;

  const BOUND: Duration = Duration::from_millis(100);

  #[test]
  fn a_thread_that_ends_is_joined() {
    let thread = WorkerThread::spawn("test-worker", || {}).unwrap();
    assert_eq!(thread.join_within(Duration::from_secs(5)), Joined::Finished);
  }

  #[test]
  fn a_hanging_thread_is_left_after_the_bound() {
    let (_never_sent, blocked) = mpsc::channel::<()>();
    let thread = WorkerThread::spawn("test-hanging", move || {
      // Blocks for good: nothing is ever sent, and the sender lives on in the test.
      let _ = blocked.recv();
    })
    .unwrap();
    let start = Instant::now();
    assert_eq!(thread.join_within(BOUND), Joined::Detached);
    assert!(start.elapsed() < BOUND * 20, "waited {:?}", start.elapsed());
  }

  #[test]
  fn a_panic_is_reported() {
    let thread = WorkerThread::spawn("test-panicking", || panic!("broken driver")).unwrap();
    assert_eq!(thread.join_within(Duration::from_secs(5)), Joined::Panicked);
  }
}
