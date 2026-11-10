//! One pool of server connections per runtime. Every request of the engine used to build its own
//! HTTP client, so each one opened a connection of its own: DNS, TCP and a TLS handshake (about
//! 150 ms of CPU and two round trips on a phone) before the request, for every profile picture at
//! the start and every push. The handles from here share one pool, and a network change or
//! wake-up the app reports drops it, as connections from before are usually dead then (#883).
//!
//! reqwest's connections belong to the runtime they were opened on: the engine thread and the HTTP
//! runtime (`http_fetch.rs`) each keep their own [`ServerPool`].

use std::sync::{Mutex, PoisonError};

use futures_channel::mpsc;
use teamcraft_core::connection::NetworkChanges;

use crate::engine::Api;

/// The kept handle whose connections every [`ServerPool::api`] shares.
pub(crate) struct ServerPool {
  kept: Api,
  /// One item per network change the app reported since the last handle.
  network: Mutex<mpsc::UnboundedReceiver<()>>,
}

impl ServerPool {
  pub(crate) fn new(kept: Api, network: &NetworkChanges) -> Self {
    Self {
      kept,
      network: Mutex::new(network.subscribe()),
    }
  }

  /// A handle on the server that reuses the pool's open connections; after a network change, on
  /// fresh ones.
  pub(crate) fn api(&self) -> Api {
    if self.has_network_changed() {
      self.kept.drop_connections();
    }
    self.kept.sharing_connections()
  }

  fn has_network_changed(&self) -> bool {
    let mut network = self.network.lock().unwrap_or_else(PoisonError::into_inner);
    let mut has_changed = false;
    while network.try_recv().is_ok() {
      has_changed = true;
    }
    has_changed
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_network_change_is_taken_once() {
    let changes = NetworkChanges::new();
    let pool = ServerPool::new(api(), &changes);
    assert!(!pool.has_network_changed());
    changes.notify();
    changes.notify();
    assert!(pool.has_network_changed());
    assert!(!pool.has_network_changed());
  }

  #[test]
  fn handles_name_the_kept_server() {
    let pool = ServerPool::new(api(), &NetworkChanges::new());
    assert_eq!(pool.api().base_url(), "https://chat.example.com");
  }

  fn api() -> Api {
    crate::engine::test_api("https://chat.example.com/")
  }
}
