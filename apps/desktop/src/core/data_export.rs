//! "Export my data" on the desktop (issue #211, docs/data-export.md): the core's [`ExportJob`]
//! writes the archive into `<target>.part` next to the file the user picked in the save dialog,
//! and the finished archive is renamed to the target. Nothing goes to a temporary folder: the
//! plaintext is only ever in the place the user chose.
//!
//! - **Cancel or failure:** the partial file is deleted at once, with the checkpoint.
//! - **The app stopped in the middle:** the checkpoint in the encrypted store names the target; the
//!   card offers to continue (the partial file is cut back to the checkpoint's length) or to
//!   discard it (deleted).

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use teamcraft_client_api::data_export::{DataExportFailure, DataExportProgress, DataExportStatus};
use teamcraft_core::export::{ExportCheckpoint, ExportError, ExportJob, ExportSink};

/// What the archive writes through before it reaches the disk.
const BUFFER_BYTES: usize = 256 * 1024;

/// Where the desktop's export stands.
#[derive(Default)]
pub(super) enum ExportSlot {
  /// Nothing running; the store may still hold an earlier run's checkpoint.
  #[default]
  Idle,
  Running(Box<ExportRun>),
  /// What the last export ended with, until the user dismisses it.
  Ended(DataExportStatus),
}

/// An export under way: the job and the file it writes.
pub(super) struct ExportRun {
  pub(super) job: ExportJob,
  pub(super) sink: FileSink,
}

/// The archive's partial file.
pub(super) struct FileSink {
  writer: BufWriter<File>,
  part: PathBuf,
  target: PathBuf,
}

impl FileSink {
  /// A new partial file for `target`, replacing one an earlier export left (the user confirmed
  /// the target in the save dialog, and the partial file's name is ours).
  pub(super) fn create(target: &Path) -> Result<Self, ExportError> {
    let part = part_path(target);
    let file = OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(true)
      .open(&part)
      .map_err(sink_error)?;
    Ok(Self::with(file, part, target))
  }

  /// The partial file of `checkpoint`, cut back to what the checkpoint vouches for.
  pub(super) fn reopen(checkpoint: &ExportCheckpoint) -> Result<Self, ExportError> {
    let target = PathBuf::from(&checkpoint.target);
    let part = part_path(&target);
    let mut file = OpenOptions::new()
      .write(true)
      .open(&part)
      .map_err(|error| ExportError::NotResumable(error.to_string()))?;
    let length = file
      .metadata()
      .map_err(|error| ExportError::NotResumable(error.to_string()))?
      .len();
    if length < checkpoint.len() {
      return Err(ExportError::NotResumable(format!(
        "the partial file holds {length} bytes, the checkpoint {}",
        checkpoint.len()
      )));
    }
    file.set_len(checkpoint.len()).map_err(sink_error)?;
    file.seek(SeekFrom::End(0)).map_err(sink_error)?;
    Ok(Self::with(file, part, &target))
  }

  fn with(file: File, part: PathBuf, target: &Path) -> Self {
    Self {
      writer: BufWriter::with_capacity(BUFFER_BYTES, file),
      part,
      target: target.to_owned(),
    }
  }

  /// The archive is complete: it takes the target's name. If that fails, the partial file is
  /// deleted: no plaintext stays behind under a name the user didn't pick.
  pub(super) fn finish(mut self) -> Result<PathBuf, ExportError> {
    let renamed = self
      .flush()
      .and_then(|()| std::fs::rename(&self.part, &self.target).map_err(sink_error));
    match renamed {
      Ok(()) => Ok(self.target),
      Err(error) => {
        self.discard();
        Err(error)
      },
    }
  }

  /// Deletes the partial file.
  pub(super) fn discard(self) {
    let part = self.part.clone();
    drop(self.writer);
    remove_part(&part);
  }
}

impl ExportSink for FileSink {
  fn write_all(&mut self, bytes: &[u8]) -> Result<(), ExportError> {
    self.writer.write_all(bytes).map_err(sink_error)
  }

  fn flush(&mut self) -> Result<(), ExportError> {
    self.writer.flush().map_err(sink_error)?;
    self.writer.get_ref().sync_data().map_err(sink_error)
  }
}

/// `<target>.part`, next to the target.
pub(super) fn part_path(target: &Path) -> PathBuf {
  let mut name = target.file_name().unwrap_or_default().to_os_string();
  name.push(".part");
  target.with_file_name(name)
}

/// Deletes the partial file an export wrote, if it is there.
pub(super) fn remove_part(part: &Path) {
  match std::fs::remove_file(part) {
    Ok(()) => tracing::info!("the partial data export was deleted"),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
    Err(error) => tracing::warn!(%error, "could not delete the partial data export"),
  }
}

/// Deletes what an earlier run's checkpoint left: its partial file and the checkpoint.
pub(super) fn discard_checkpoint(
  store: &impl teamcraft_core::store::LocalStore,
  checkpoint: &ExportCheckpoint,
) {
  remove_part(&part_path(Path::new(&checkpoint.target)));
  if let Err(error) = ExportJob::forget(store) {
    tracing::warn!(%error, "could not delete the data export's checkpoint");
  }
}

/// The status of an export that ended with `error`.
pub(super) fn failed(error: &ExportError, progress: DataExportProgress) -> DataExportStatus {
  DataExportStatus::Failed {
    reason: error.failure(),
    progress,
  }
}

/// A checkpoint that can't be resumed, as the card shows it.
pub(super) fn not_resumable(progress: DataExportProgress) -> DataExportStatus {
  DataExportStatus::Failed {
    reason: DataExportFailure::NotResumable,
    progress,
  }
}

fn sink_error(error: std::io::Error) -> ExportError {
  ExportError::Sink(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-export-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("export.zip")
  }

  #[test]
  fn the_partial_file_sits_next_to_the_target_and_takes_its_name_at_the_end() {
    let target = scratch("finish");
    let mut sink = FileSink::create(&target).unwrap();
    sink.write_all(b"archive").unwrap();
    assert!(part_path(&target).exists());
    assert_eq!(part_path(&target).file_name().unwrap(), "export.zip.part");

    let saved = sink.finish().unwrap();

    assert_eq!(saved, target);
    assert_eq!(std::fs::read(&target).unwrap(), b"archive");
    assert!(!part_path(&target).exists(), "nothing is left beside it");
    std::fs::remove_file(&target).unwrap();
  }

  #[test]
  fn a_discarded_export_leaves_no_file() {
    let target = scratch("discard");
    let mut sink = FileSink::create(&target).unwrap();
    sink.write_all(b"plaintext").unwrap();
    sink.flush().unwrap();

    sink.discard();

    assert!(!part_path(&target).exists());
    assert!(!target.exists());
  }
}
