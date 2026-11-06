//! Wakes the host while a connection's sync stores what the UI should show: the first sync's
//! progress, the server's answer to `identify` or `resume` (the connection banner), and every
//! conversation a catch-up finished. A host that polls the client for nothing else, the mobile apps'
//! sync loop, heard of none of it until the whole catch-up was done: after a long absence the chat
//! list and the open conversation stayed as they were for many seconds while their messages were
//! already stored (the browser reads on its own timer, `web_core.rs`).

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::future::{Either, select};
use futures_util::task::AtomicWaker;

use super::Client;
use crate::api::ServerApi;
use crate::platform;
use crate::store::LocalStore;

/// Least time between two calls of the host's `notify` while a sync runs: a catch-up of hundreds of
/// conversations would otherwise have the UI read its list once per conversation. Short enough that
/// a change shows as if at once.
const SYNC_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Counts what a sync stored for the UI and wakes the one host waiting. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct SyncProgressSignal {
  changes: Arc<AtomicU64>,
  /// The host waiting in [`Self::changed_since`] (one waiter).
  waker: Arc<AtomicWaker>,
}

impl SyncProgressSignal {
  /// Completes with the count of changes once it differs from `seen`: no timer, the sync wakes it.
  pub fn changed_since(&self, seen: u64) -> impl Future<Output = u64> + '_ {
    std::future::poll_fn(move |context| {
      self.waker.register(context.waker());
      match self.changes.load(Ordering::Relaxed) {
        count if count != seen => std::task::Poll::Ready(count),
        _ => std::task::Poll::Pending,
      }
    })
  }

  pub fn count(&self) -> u64 {
    self.changes.load(Ordering::Relaxed)
  }

  pub(super) fn note_change(&self) {
    self.changes.fetch_add(1, Ordering::Relaxed);
    self.waker.wake();
  }
}

/// Runs `work` (a connection and its sync, which can take minutes) and calls `notify` whenever the
/// sync stored something to show meanwhile, at most once per [`SYNC_PROGRESS_INTERVAL`], so the UI
/// shows the list, the open conversation and the connection as they get there.
pub async fn notify_sync_progress_during<F: Future>(
  signal: &SyncProgressSignal,
  notify: impl Fn(),
  work: F,
) -> F::Output {
  let mut work = pin!(work);
  let mut seen = signal.count();
  loop {
    let changed = pin!(signal.changed_since(seen));
    match select(work.as_mut(), changed).await {
      Either::Left((output, _)) => return output,
      Either::Right((count, _)) => {
        seen = count;
        notify();
      },
    }
    let pause = pin!(platform::sleep(SYNC_PROGRESS_INTERVAL));
    if let Either::Left((output, _)) = select(work.as_mut(), pause).await {
      return output;
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The handle a host waits on while it connects ([`notify_sync_progress_during`]).
  pub fn sync_progress_signal(&self) -> SyncProgressSignal {
    self.sync_progress.clone()
  }

  /// The sync stored something the UI should show now.
  pub(super) fn note_sync_progress(&self) {
    self.sync_progress.note_change();
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;

  use futures_util::FutureExt;

  use super::*;

  #[test]
  fn the_signal_wakes_whoever_waits_and_only_on_a_change() {
    let signal = SyncProgressSignal::default();
    assert_eq!(signal.changed_since(0).now_or_never(), None);
    signal.clone().note_change();
    assert_eq!(signal.changed_since(0).now_or_never(), Some(1));
    assert_eq!(signal.changed_since(1).now_or_never(), None);
  }

  #[tokio::test(start_paused = true)]
  async fn the_host_hears_of_progress_during_the_work_at_most_once_per_interval() {
    let signal = SyncProgressSignal::default();
    let notified = Cell::new(0);
    let work = async {
      // Three conversations caught up at once: one read.
      signal.note_change();
      signal.note_change();
      signal.note_change();
      tokio::time::sleep(Duration::from_millis(10)).await;
      assert_eq!(notified.get(), 1);
      // Another right after: it waits for the interval.
      signal.note_change();
      tokio::time::sleep(Duration::from_millis(10)).await;
      assert_eq!(notified.get(), 1);
      tokio::time::sleep(SYNC_PROGRESS_INTERVAL).await;
      assert_eq!(notified.get(), 2);
      "done"
    };
    let output = notify_sync_progress_during(&signal, || notified.set(notified.get() + 1), work).await;
    assert_eq!(output, "done");
    assert_eq!(notified.get(), 2);
  }
}
