//! The app hears that the local store changed ([`EventListener::on_change`]) on a thread of its own
//! (issue #2225).
//!
//! The core's threads used to call the app's listener themselves: the actions' thread did so in the
//! middle of an action, while it held the store's writer. On Android the call goes through JNI into
//! the app, and on 1.0.41 one such call hung for 7.2 s: the action held the writer meanwhile, a read
//! that wrote waited for it with every read queued behind it, and the engine thread waited for its
//! next group of writes, its request to the server unattended. Now the core's threads only post the
//! change here, which never waits, and this thread tells the app. Changes that come while the app is
//! told are told once more afterwards, so none is lost and a burst is one call.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use crate::EventListener;
use crate::error::TeamCraftError;

/// A call into the app longer than this is logged: it held up nothing of the core, but the app's
/// screens only re-read once it returned.
const SLOW_APP_CALL: Duration = Duration::from_secs(1);

/// The app's [`EventListener`] behind the thread that calls it. The thread ends once every clone of
/// the handle that holds this is gone.
pub(crate) struct AppChanges {
  /// Room for one change: one that waits already covers the next.
  pending: SyncSender<()>,
}

impl AppChanges {
  /// Starts the thread that tells `listener`.
  pub(crate) fn start(listener: Arc<dyn EventListener>) -> Result<Arc<dyn EventListener>, TeamCraftError> {
    let (pending, changes) = mpsc::sync_channel(1);
    std::thread::Builder::new()
      .name("teamcraft-changes".into())
      .spawn(move || tell_app(&changes, listener.as_ref()))
      .map_err(|error| TeamCraftError::Core(error.to_string()))?;
    Ok(Arc::new(Self { pending }))
  }
}

impl EventListener for AppChanges {
  /// Never waits: posts the change for the app's thread.
  fn on_change(&self) {
    match self.pending.try_send(()) {
      // A change waits already: the app re-reads everything once it hears of it.
      Ok(()) | Err(TrySendError::Full(())) => {},
      // The thread ended with the handle: nobody is left to tell.
      Err(TrySendError::Disconnected(())) => {},
    }
  }
}

/// The thread: tells the app of each change posted, until no core thread can post any more.
fn tell_app(changes: &Receiver<()>, listener: &dyn EventListener) {
  while changes.recv().is_ok() {
    let started = Instant::now();
    listener.on_change();
    let took = started.elapsed();
    if took >= SLOW_APP_CALL {
      tracing::warn!(
        ms = u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        "the app took long to hear of a change"
      );
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;

  #[test]
  fn posting_a_change_never_waits_for_the_app() {
    let (listener, entered, release) = BlockingListener::new();
    let changes = AppChanges::start(listener.clone()).unwrap();
    changes.on_change();
    entered.recv().unwrap();

    // The app is stuck in its callback: the core's threads go on.
    for _ in 0..100 {
      changes.on_change();
    }

    release.send(()).unwrap();
    // The changes that came meanwhile are told once more, in one call.
    entered.recv().unwrap();
    release.send(()).unwrap();
    drop(changes);
    listener.wait_until_calls(2);
  }

  #[test]
  fn every_change_reaches_the_app() {
    let (listener, entered, release) = BlockingListener::new();
    let changes = AppChanges::start(listener.clone()).unwrap();
    changes.on_change();
    entered.recv().unwrap();
    release.send(()).unwrap();
    // Posted after the app was told: a call of its own.
    changes.on_change();
    entered.recv().unwrap();
    release.send(()).unwrap();
    listener.wait_until_calls(2);
  }

  /// An app whose callback waits until the test lets it return.
  struct BlockingListener {
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<Receiver<()>>,
    calls: AtomicUsize,
  }

  impl BlockingListener {
    fn new() -> (Arc<Self>, Receiver<()>, mpsc::Sender<()>) {
      let (entered, has_entered) = mpsc::channel();
      let (release, released) = mpsc::channel();
      let listener = Arc::new(Self {
        entered: Mutex::new(entered),
        release: Mutex::new(released),
        calls: AtomicUsize::new(0),
      });
      (listener, has_entered, release)
    }

    fn wait_until_calls(&self, count: usize) {
      while self.calls.load(Ordering::SeqCst) < count {
        std::thread::yield_now();
      }
      assert_eq!(self.calls.load(Ordering::SeqCst), count);
    }
  }

  impl EventListener for BlockingListener {
    fn on_change(&self) {
      // The test may be over.
      let _ = self.entered.lock().unwrap().send(());
      let _ = self.release.lock().unwrap().recv();
      self.calls.fetch_add(1, Ordering::SeqCst);
    }
  }
}
