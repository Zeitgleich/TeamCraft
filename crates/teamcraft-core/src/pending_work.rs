//! What still has to reach the server: messages in the outbox, files going up and queued actions
//! (issue #158). A mobile shell asks before it lets the OS suspend the app: while anything is
//! pending it keeps a background task (iOS) or schedules network-constrained work (Android) that
//! runs the sync loop until the queue is empty or the OS's time is up.
//!
//! Everything counted here lives in the store, so it survives the process being killed; the next
//! start (or the next background run) sends it, and the client message ID makes a send whose answer
//! was lost arrive once (robustness.md, "Offline and queued actions").

use crate::client::files::PendingUpload;
use crate::client::pending_action_count;
use crate::error::CoreError;
use crate::records::{self, OutboxEntry};
use crate::store::{LocalStore, Table};

/// Work waiting for the server; see the module docs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingWork {
  /// Messages in the outbox that will be sent again. Those the server refused for good wait for the
  /// user ("Try again" or "Delete") and aren't counted: no background run can send them.
  pub sends: usize,
  /// Messages whose files are still going up (not uploaded, not failed for good).
  pub uploads: usize,
  /// Read positions, acknowledgements, pins, presence and status changes in the queue.
  pub actions: usize,
}

impl PendingWork {
  /// Reads what `store` holds; works while a runner owns the client.
  pub fn read(store: &impl LocalStore) -> Result<Self, CoreError> {
    let sends = records::outbox(store)?
      .iter()
      .filter(|entry: &&OutboxEntry| entry.maybe_last_error.is_none())
      .count();
    let uploads = records::list_json::<PendingUpload>(store, Table::PendingUploads)?
      .iter()
      .filter(|upload| {
        upload
          .files
          .iter()
          .any(|file| file.maybe_done.is_none() && file.maybe_failure.is_none())
      })
      .count();
    Ok(Self {
      sends,
      uploads,
      actions: pending_action_count(store)?,
    })
  }

  pub fn is_empty(&self) -> bool {
    *self == Self::default()
  }
}
