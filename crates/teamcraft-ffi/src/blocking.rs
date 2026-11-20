//! Blocking work behind an `async` export (#2184): the apps' UI thread never waits.
//!
//! An export that waits on the store, the disk, a lock or a device is `async` for UniFFI (Swift
//! `async`, Kotlin `suspend`), so the app can't call it on its UI thread and wait. The work itself
//! is synchronous; [`off_thread`] runs it on a thread of its own and the returned future completes
//! when it is done. The future needs no runtime: the foreign executor polls it and the thread wakes
//! it.

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use futures_channel::oneshot;

/// Runs `work` on a new thread named `name` and returns its result once done.
///
/// A panic in `work` panics in the awaiting future too, where UniFFI turns it into the app's
/// error. A thread that can't be started (the process is out of threads) runs `work` on the
/// caller's thread: late beats never.
pub(crate) async fn off_thread<T, F>(name: &str, work: F) -> T
where
  T: Send + 'static,
  F: FnOnce() -> T + Send + 'static,
{
  let (done, answer) = oneshot::channel();
  // Shared so the work is still here when the thread can't start; taken exactly once.
  let slot = Arc::new(Mutex::new(Some(work)));
  let started = thread::Builder::new().name(name.to_owned()).spawn({
    let slot = slot.clone();
    move || {
      if let Some(work) = take(&slot) {
        // The receiver is gone only if the caller stopped waiting: nothing to hand it.
        let _ = done.send(panic::catch_unwind(AssertUnwindSafe(work)));
      }
    }
  });
  if let Err(error) = started {
    tracing::warn!(%error, thread = name, "no thread for blocking work; it runs on the caller's");
    if let Some(work) = take(&slot) {
      return work();
    }
  }
  match answer.await {
    Ok(Ok(value)) => value,
    Ok(Err(payload)) => panic::resume_unwind(payload),
    // The sender went unused: the thread ended without running the work.
    Err(oneshot::Canceled) => panic!("the {name} thread ended without an answer"),
  }
}

fn take<F>(slot: &Mutex<Option<F>>) -> Option<F> {
  // Plain data: a panic elsewhere while it was locked leaves nothing half-changed.
  slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicBool, Ordering};

  use super::*;

  #[tokio::test]
  async fn the_work_runs_on_its_own_thread_and_returns_its_value() {
    let caller = thread::current().id();
    let (value, worker) = off_thread("test-work", || (41 + 1, thread::current().id())).await;
    assert_eq!(value, 42);
    assert_ne!(worker, caller);
  }

  #[tokio::test]
  async fn the_thread_is_named_for_logs_and_hang_reports() {
    let name = off_thread("store-open", || thread::current().name().map(str::to_owned)).await;
    assert_eq!(name.as_deref(), Some("store-open"));
  }

  #[tokio::test]
  async fn the_caller_does_not_wait_while_the_work_waits() {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let has_finished = Arc::new(AtomicBool::new(false));
    let finished = has_finished.clone();
    let pending = tokio::spawn(off_thread("test-wait", move || {
      released.recv().unwrap();
      finished.store(true, Ordering::SeqCst);
    }));
    // The caller's runtime keeps running other work while the thread waits.
    tokio::task::yield_now().await;
    assert!(!has_finished.load(Ordering::SeqCst));
    release.send(()).unwrap();
    pending.await.unwrap();
    assert!(has_finished.load(Ordering::SeqCst));
  }

  #[tokio::test]
  async fn a_panic_in_the_work_reaches_the_caller() {
    let outcome = tokio::spawn(off_thread("test-panic", || -> u8 { panic!("broken") })).await;
    assert!(outcome.unwrap_err().is_panic());
  }
}
