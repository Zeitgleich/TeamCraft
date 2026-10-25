//! Progress of the file transfers in flight: the transport counts the bytes it moved, the UI adapter
//! reads them for the pending message ("12 / 48 MB") and the file card. Kept in memory only: after a
//! restart an upload starts over and shows its progress again from there.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use teamcraft_proto::AttachmentId;

/// Bytes a request moved so far, shared between the transport that counts and the reader.
#[derive(Clone, Debug, Default)]
pub struct ByteCounter(Arc<AtomicU64>);

/// The transfers in flight, by attachment (an upload's announced slot or a download's file). Clones
/// share the same list: the client registers transfers, the UI adapter reads them.
#[derive(Clone, Debug, Default)]
pub struct Transfers(Arc<Mutex<HashMap<AttachmentId, Progress>>>);

/// Where a transfer stands: bytes moved of the bytes on the wire (the ciphertext).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferProgress {
  pub done_bytes: u64,
  pub total_bytes: u64,
}

/// A transfer listed in [`Transfers`] while this lives; dropping it (done, failed, cancelled) takes
/// it off the list.
#[derive(Debug)]
pub struct TransferGuard {
  transfers: Transfers,
  attachment: AttachmentId,
  counter: ByteCounter,
}

#[derive(Clone, Debug)]
struct Progress {
  counter: ByteCounter,
  total_bytes: u64,
}

impl ByteCounter {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn add(&self, bytes: u64) {
    self.0.fetch_add(bytes, Ordering::Relaxed);
  }

  /// For transports that report the running total (the browser's progress events).
  pub fn set(&self, bytes: u64) {
    self.0.store(bytes, Ordering::Relaxed);
  }

  pub fn get(&self) -> u64 {
    self.0.load(Ordering::Relaxed)
  }
}

impl Transfers {
  pub fn new() -> Self {
    Self::default()
  }

  /// Lists a transfer of `total_bytes` until the returned guard is dropped.
  pub fn start(&self, attachment: AttachmentId, total_bytes: u64) -> TransferGuard {
    let counter = ByteCounter::new();
    if let Ok(mut list) = self.0.lock() {
      list.insert(
        attachment,
        Progress {
          counter: counter.clone(),
          total_bytes,
        },
      );
    }
    TransferGuard {
      transfers: self.clone(),
      attachment,
      counter,
    }
  }

  pub fn progress(&self, attachment: AttachmentId) -> Option<TransferProgress> {
    let list = self.0.lock().ok()?;
    list.get(&attachment).map(|progress| TransferProgress {
      done_bytes: progress.counter.get().min(progress.total_bytes),
      total_bytes: progress.total_bytes,
    })
  }

  /// All bytes moved by the transfers in flight, or `None` without any: the host tells the UI to
  /// read again while this changes.
  pub fn moved_bytes(&self) -> Option<u64> {
    let list = self.0.lock().ok()?;
    (!list.is_empty()).then(|| list.values().map(|progress| progress.counter.get()).sum())
  }
}

impl TransferProgress {
  /// The progress scaled to `size_bytes` (the file's own size; the wire carries the ciphertext).
  pub fn of_size(&self, size_bytes: u64) -> u64 {
    if self.total_bytes == 0 {
      return 0;
    }
    let scaled = u128::from(self.done_bytes) * u128::from(size_bytes) / u128::from(self.total_bytes);
    u64::try_from(scaled).unwrap_or(size_bytes).min(size_bytes)
  }
}

impl TransferGuard {
  /// What the transport counts into.
  pub fn counter(&self) -> ByteCounter {
    self.counter.clone()
  }
}

impl Drop for TransferGuard {
  fn drop(&mut self) {
    if let Ok(mut list) = self.transfers.0.lock() {
      list.remove(&self.attachment);
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_transfer_is_listed_while_its_guard_lives() {
    let transfers = Transfers::new();
    let attachment = AttachmentId::from_snowflake(Snowflake::new(7).unwrap());
    assert_eq!(transfers.moved_bytes(), None);
    let guard = transfers.start(attachment, 200);
    guard.counter().add(50);
    assert_eq!(
      transfers.progress(attachment),
      Some(TransferProgress {
        done_bytes: 50,
        total_bytes: 200
      })
    );
    assert_eq!(transfers.moved_bytes(), Some(50));
    drop(guard);
    assert_eq!(transfers.progress(attachment), None);
  }

  #[test]
  fn progress_scales_to_the_file_size_and_never_exceeds_it() {
    let progress = TransferProgress {
      done_bytes: 60,
      total_bytes: 120,
    };
    assert_eq!(progress.of_size(1_000), 500);
    let overshoot = TransferProgress {
      done_bytes: 130,
      total_bytes: 120,
    };
    assert_eq!(overshoot.of_size(1_000), 1_000);
    let empty = TransferProgress {
      done_bytes: 0,
      total_bytes: 0,
    };
    assert_eq!(empty.of_size(10), 0);
  }
}
