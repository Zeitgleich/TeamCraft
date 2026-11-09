//! What the one upload control of a message shows: a pill in the corner of its pictures (or a row
//! for documents) with one ring for all its files, instead of a ring and a "Cancel" per file.
//!
//! The same rule on every client: desktop and web use this, Android (`UploadPill.kt`) and iOS
//! (`UploadPill.swift`) mirror it, each with tests of the same cases.

use crate::actions::{FileFailure, FileInfo, FileTransfer};

/// The state of a message's upload, for its one control. `None` from [`UploadPill::of`] means
/// nothing is going up (done, or never was): the control is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPill {
  /// Going up, but no byte has left yet (encrypting, waiting for the connection): "Processing…"
  /// and a turning ring.
  Processing,
  /// No byte has left yet and an attempt failed; the core tries again by itself (#742).
  Retrying,
  /// Bytes are moving: "3.2 MB / 70 MB" and a ring filled to the share of all files together.
  Sending { done_bytes: u64, total_bytes: u64 },
  /// A file stopped retrying at the usual pace: "Try again" restarts the message's upload.
  Failed { reason: FileFailure },
}

impl UploadPill {
  /// The control for a message's files.
  ///
  /// Files that finished count with their whole size, so the ring doesn't jump back when one of
  /// them is done; a file that failed for good (the message went out without it) doesn't count.
  pub fn of(files: &[FileInfo]) -> Option<Self> {
    let transfers: Vec<&FileTransfer> = files.iter().map(|file| &file.transfer).collect();
    if let Some(reason) = transfers.iter().find_map(|transfer| match transfer {
      FileTransfer::UploadStalled { reason } => Some(*reason),
      _ => None,
    }) {
      return Some(Self::Failed { reason });
    }
    if !transfers
      .iter()
      .any(|transfer| matches!(transfer, FileTransfer::Uploading { .. }))
    {
      return None;
    }
    let (mut done_bytes, mut total_bytes, mut has_failed_attempts) = (0_u64, 0_u64, false);
    for file in files {
      match file.transfer {
        FileTransfer::Uploading {
          done_bytes: done,
          total_bytes: total,
          failed_attempts,
        } => {
          done_bytes = done_bytes.saturating_add(done.min(total));
          total_bytes = total_bytes.saturating_add(total);
          has_failed_attempts |= failed_attempts > 0;
        },
        FileTransfer::UploadFailed { .. } => {},
        _ => {
          done_bytes = done_bytes.saturating_add(file.size_bytes);
          total_bytes = total_bytes.saturating_add(file.size_bytes);
        },
      }
    }
    Some(match (done_bytes, has_failed_attempts) {
      (0, true) => Self::Retrying,
      (0, false) => Self::Processing,
      _ => Self::Sending {
        done_bytes,
        total_bytes,
      },
    })
  }

  /// How far the files got, 0.0..=1.0; `None` while the ring turns without a value.
  pub fn fraction(self) -> Option<f64> {
    match self {
      Self::Sending {
        done_bytes,
        total_bytes,
      } if total_bytes > 0 => Some((done_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0)),
      _ => None,
    }
  }

  /// Whether the ring's stop square cancels the upload (it does while going up; a failed one
  /// offers "Try again" instead).
  pub fn is_going_up(self) -> bool {
    !matches!(self, Self::Failed { .. })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  const MB: u64 = 1_000_000;

  #[test]
  fn nothing_going_up_has_no_control() {
    assert_eq!(UploadPill::of(&[]), None);
    assert_eq!(UploadPill::of(&[file(FileTransfer::Remote, MB)]), None);
    let failed = FileTransfer::UploadFailed {
      reason: FileFailure::TooLarge,
    };
    assert_eq!(UploadPill::of(&[file(failed, MB)]), None);
  }

  #[test]
  fn before_the_first_byte_it_is_processing() {
    let files = [
      file(uploading(0, 5 * MB, 0), 5 * MB),
      file(uploading(0, 3 * MB, 0), 3 * MB),
    ];
    assert_eq!(UploadPill::of(&files), Some(UploadPill::Processing));
    assert_eq!(UploadPill::Processing.fraction(), None);
  }

  #[test]
  fn a_failed_attempt_before_the_first_byte_is_retrying() {
    let files = [file(uploading(0, 5 * MB, 2), 5 * MB)];
    assert_eq!(UploadPill::of(&files), Some(UploadPill::Retrying));
  }

  #[test]
  fn bytes_add_up_over_all_files() {
    let files = [
      file(uploading(2 * MB, 4 * MB, 0), 4 * MB),
      file(uploading(0, 6 * MB, 0), 6 * MB),
    ];
    let pill = UploadPill::of(&files);
    assert_eq!(
      pill,
      Some(UploadPill::Sending {
        done_bytes: 2 * MB,
        total_bytes: 10 * MB
      })
    );
    assert_eq!(pill.and_then(UploadPill::fraction), Some(0.2));
  }

  #[test]
  fn finished_files_count_whole_so_the_ring_never_goes_back() {
    let files = [
      file(FileTransfer::Remote, 4 * MB),
      file(uploading(0, 6 * MB, 0), 6 * MB),
    ];
    assert_eq!(
      UploadPill::of(&files),
      Some(UploadPill::Sending {
        done_bytes: 4 * MB,
        total_bytes: 10 * MB
      })
    );
  }

  #[test]
  fn files_that_failed_for_good_do_not_count() {
    let failed = FileTransfer::UploadFailed {
      reason: FileFailure::TooLarge,
    };
    let files = [file(failed, 90 * MB), file(uploading(MB, 2 * MB, 0), 2 * MB)];
    assert_eq!(
      UploadPill::of(&files),
      Some(UploadPill::Sending {
        done_bytes: MB,
        total_bytes: 2 * MB
      })
    );
  }

  #[test]
  fn a_stalled_file_makes_the_message_failed() {
    let stalled = FileTransfer::UploadStalled {
      reason: FileFailure::StorageUnavailable,
    };
    let files = [file(uploading(MB, 2 * MB, 0), 2 * MB), file(stalled, MB)];
    let pill = UploadPill::of(&files);
    assert_eq!(
      pill,
      Some(UploadPill::Failed {
        reason: FileFailure::StorageUnavailable
      })
    );
    assert!(!pill.is_some_and(UploadPill::is_going_up));
  }

  #[test]
  fn reported_bytes_beyond_the_total_do_not_overfill() {
    let files = [file(uploading(9 * MB, 4 * MB, 0), 4 * MB)];
    assert_eq!(UploadPill::of(&files).and_then(UploadPill::fraction), Some(1.0));
  }

  fn uploading(done_bytes: u64, total_bytes: u64, failed_attempts: u32) -> FileTransfer {
    FileTransfer::Uploading {
      done_bytes,
      total_bytes,
      failed_attempts,
    }
  }

  fn file(transfer: FileTransfer, size_bytes: u64) -> FileInfo {
    FileInfo {
      conversation: id_from_raw(1),
      message: id_from_raw(2),
      file_id: "pending-0".to_owned(),
      file_name: "photo.jpg".to_owned(),
      media_type: "image/jpeg".to_owned(),
      size_bytes,
      maybe_thumbnail_png: None,
      transfer,
      maybe_sender: None,
      maybe_sent_at: None,
      maybe_imported_author: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_waveform: None,
      maybe_content_sha256: None,
    }
  }
}
