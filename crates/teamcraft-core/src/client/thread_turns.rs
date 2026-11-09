//! Long work handing its thread back (issue #1575). In the browser the client runs on the page's
//! only thread: a catch-up of a hundred conversations after a long absence is seconds of decrypting
//! and writing, and until it gives the thread back nothing else runs, not the UI's input, not its
//! reads of the store, not a paint. So where such work has a natural break (between two
//! conversations, two entries of a queue, two parts of a page of messages), it asks
//! [`Client::hand_back_thread`], which lets the rest of the thread run once its turn
//! ([`super::ClientSettings::maybe_thread_turn`]) is used up.
//!
//! Natively the client has a thread of its own and the readers theirs; they take turns at the store
//! instead (`store.is_awaited()`), and these breaks cost nothing there.

use super::Client;
use crate::api::ServerApi;
use crate::platform;
use crate::store::LocalStore;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Lets the thread's other work run if this turn is used up; only where the host asked for turns.
  pub(crate) async fn hand_back_thread(&mut self) {
    if self.is_thread_turn_over() {
      platform::yield_now().await;
      self.thread_turn_began_ms = self.now_ms();
    }
  }

  /// Whether work on the client's thread has run for a whole turn since it last handed the thread
  /// back: a group of writes ends then, so the next break can hand it back.
  pub(crate) fn is_thread_turn_over(&self) -> bool {
    self.settings.maybe_thread_turn.is_some_and(|turn| {
      let turn_ms = i64::try_from(turn.as_millis()).unwrap_or(i64::MAX);
      self.now_ms().saturating_sub(self.thread_turn_began_ms) >= turn_ms
    })
  }
}
