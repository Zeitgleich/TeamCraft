//! Graceful shutdown (docs/operations.md, "Upgrades"): on SIGTERM the server stops reporting ready,
//! closes every gateway and relay socket with 1012 ("service restart") so clients reconnect at
//! once instead of waiting for a dead connection to time out, and waits a bounded time for the
//! socket tasks to send their close frames before the process exits.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{Notify, watch};

/// WebSocket close code "service restart" (RFC 6455, IANA registry): try again soon.
pub const CLOSE_SERVICE_RESTART: u16 = 1012;

/// Whether the server is shutting down, and the WebSockets that still have to close.
#[derive(Debug)]
pub struct Drain {
  is_draining: watch::Sender<bool>,
  open_sockets: AtomicUsize,
  closed: Notify,
}

/// Counts one open WebSocket until it is dropped.
#[derive(Debug)]
pub struct SocketGuard {
  drain: Arc<Drain>,
}

impl Drain {
  pub fn new() -> Self {
    Self {
      is_draining: watch::Sender::new(false),
      open_sockets: AtomicUsize::new(0),
      closed: Notify::new(),
    }
  }

  /// Starts the shutdown: readiness fails from now on and every socket task closes its socket.
  pub fn begin(&self) {
    self.is_draining.send_replace(true);
  }

  pub fn is_draining(&self) -> bool {
    *self.is_draining.borrow()
  }

  /// Resolves once the shutdown has begun (at once if it already has).
  pub async fn started(&self) {
    let mut receiver = self.is_draining.subscribe();
    // The sender lives as long as `self`, so the wait can't fail while anyone awaits it.
    let _ = receiver.wait_for(|is_draining| *is_draining).await;
  }

  /// Registers an open WebSocket; it counts until the guard is dropped.
  pub fn track(self: &Arc<Self>) -> SocketGuard {
    self.open_sockets.fetch_add(1, Ordering::AcqRel);
    SocketGuard { drain: self.clone() }
  }

  pub fn open_sockets(&self) -> usize {
    self.open_sockets.load(Ordering::Acquire)
  }

  /// Waits until every tracked socket is closed, at most `limit`. Returns whether they all closed.
  pub async fn wait_closed(&self, limit: Duration) -> bool {
    let all_closed = async {
      loop {
        let notified = self.closed.notified();
        if self.open_sockets() == 0 {
          return;
        }
        notified.await;
      }
    };
    tokio::time::timeout(limit, all_closed).await.is_ok()
  }
}

impl Default for Drain {
  fn default() -> Self {
    Self::new()
  }
}

impl Drop for SocketGuard {
  fn drop(&mut self) {
    if self.drain.open_sockets.fetch_sub(1, Ordering::AcqRel) == 1 {
      self.drain.closed.notify_waiters();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test(start_paused = true)]
  async fn waiting_ends_when_the_last_socket_closes_or_at_the_limit() {
    let drain = Arc::new(Drain::new());
    assert!(drain.wait_closed(Duration::from_secs(1)).await, "nothing open");
    let first = drain.track();
    let second = drain.track();
    drain.begin();
    assert!(drain.is_draining());
    drain.started().await;

    assert!(!drain.wait_closed(Duration::from_secs(1)).await, "bounded");
    drop(first);
    let waiter = {
      let drain = drain.clone();
      tokio::spawn(async move { drain.wait_closed(Duration::from_secs(10)).await })
    };
    tokio::task::yield_now().await;
    drop(second);
    assert!(waiter.await.unwrap());
    assert_eq!(drain.open_sockets(), 0);
  }
}
