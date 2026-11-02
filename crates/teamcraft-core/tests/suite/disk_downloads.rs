//! Downloads to disk that stream and resume (issue #153): an interrupted download continues where
//! it broke off, also after a restart; a blob that isn't the announced one never reaches the disk.
#![cfg(all(feature = "ui", not(target_arch = "wasm32")))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::{
  FileFailure, FileSource, FileTransfer, MessagePriority, OutgoingFile, OutgoingMessage,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::ClientSettings;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::disk_downloads::{DownloadStatus, download_record, download_records};
use teamcraft_core::client::files::UploadRetrySettings;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::downloads::Downloads;
use teamcraft_proto::{AttachmentId, ConversationId, MessageId};

const MIB: u64 = 1024 * 1024;
const FILE_BYTES: usize = 3 * MIB as usize;
const START_MS: i64 = 1_800_000_000_000;

#[tokio::test]
async fn an_interrupted_download_continues_where_it_broke_off() {
  let mut setup = Setup::new("resume").await;
  setup.server.cut_next_download_after(MIB);
  let mut downloads = Downloads::new(1);
  downloads.save(setup.channel, setup.attachment);

  setup.run(&mut downloads).await;
  let record = download_record(setup.bob.client.store(), setup.attachment)
    .unwrap()
    .unwrap();
  assert_eq!(record.status, DownloadStatus::Waiting, "waits for its retry");
  assert_eq!(
    setup.transfer(),
    FileTransfer::Failed {
      reason: FileFailure::Network
    },
    "the card says why meanwhile"
  );

  // Not before its retry is due.
  setup.run(&mut downloads).await;
  assert_eq!(setup.server.download_starts(), vec![0]);
  setup.clock.advance(2_000);
  setup.run(&mut downloads).await;

  assert_eq!(setup.server.download_starts(), vec![0, MIB]);
  setup.assert_saved();
}

#[tokio::test]
async fn a_download_the_app_left_running_continues_after_a_restart() {
  let mut setup = Setup::new("restart").await;
  setup.server.cut_next_download_after(MIB + 5);
  let mut downloads = Downloads::new(1);
  downloads.save(setup.channel, setup.attachment);
  setup.run(&mut downloads).await;
  // The retry starts, and the app is killed before it read anything.
  setup.clock.advance(2_000);
  let started = setup
    .bob
    .client
    .start_download_to_disk(setup.channel, setup.attachment)
    .await
    .unwrap();
  drop(started);
  drop(downloads);

  // The app starts again with nothing queued.
  let mut downloads = Downloads::new(1);
  setup.run(&mut downloads).await;

  assert_eq!(setup.server.download_starts(), vec![0, MIB + 5]);
  setup.assert_saved();
}

#[tokio::test]
async fn a_storage_that_ignores_the_range_sends_everything_again() {
  let mut setup = Setup::new("no-range").await;
  setup.server.cut_next_download_after(MIB);
  setup.server.ignore_download_ranges(true);
  let mut downloads = Downloads::new(1);
  downloads.save(setup.channel, setup.attachment);
  setup.run(&mut downloads).await;
  setup.clock.advance(2_000);
  setup.run(&mut downloads).await;

  setup.assert_saved();
}

#[tokio::test]
async fn a_tampered_blob_never_reaches_the_disk() {
  let mut setup = Setup::new("tampered").await;
  setup.server.tamper_blobs();
  let mut downloads = Downloads::new(1);
  downloads.save(setup.channel, setup.attachment);
  setup.run(&mut downloads).await;

  assert_eq!(
    setup.transfer(),
    FileTransfer::Failed {
      reason: FileFailure::Corrupted
    }
  );
  let files: Vec<_> = std::fs::read_dir(&setup.directory)
    .map(|entries| entries.flatten().filter(|entry| entry.path().is_file()).collect())
    .unwrap_or_default();
  assert!(
    files.is_empty(),
    "no plaintext and no partial file left: {files:?}"
  );
  assert_eq!(setup.partial_files(), 0);
}

#[tokio::test]
async fn a_download_that_keeps_failing_stops_for_the_user() {
  let mut setup = Setup::new("gives-up").await;
  for _ in 0..3 {
    setup.server.cut_next_download_after(1000);
  }
  let mut downloads = Downloads::new(1);
  downloads.save(setup.channel, setup.attachment);
  for _ in 0..3 {
    setup.run(&mut downloads).await;
    setup.clock.advance(60_000);
  }
  assert_eq!(
    setup.transfer(),
    FileTransfer::Failed {
      reason: FileFailure::Network
    }
  );
  assert_eq!(setup.server.download_starts(), vec![0, 1000, 2000]);

  // The user's retry continues once more from where the last attempt stopped.
  downloads.save(setup.channel, setup.attachment);
  setup.run(&mut downloads).await;
  assert_eq!(setup.server.download_starts(), vec![0, 1000, 2000, 3000]);
  setup.assert_saved();
  assert_eq!(download_records(setup.bob.client.store()).unwrap().len(), 1);
}

/// Alice sends Bob a file of [`FILE_BYTES`]; Bob's downloads go into a folder of their own.
struct Setup {
  server: FakeServer,
  bob: Peer,
  clock: Arc<ManualClock>,
  channel: ConversationId,
  message: MessageId,
  attachment: AttachmentId,
  directory: PathBuf,
}

impl Setup {
  async fn new(name: &str) -> Self {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(START_MS));
    let directory = std::env::temp_dir().join(format!("teamcraft-disk-{name}-{}", std::process::id()));
    let staging = directory.join("staging");
    let settings = ClientSettings {
      upload_retry: UploadRetrySettings {
        retry_after: Duration::from_secs(2),
        retry_max: Duration::from_secs(10),
        max_attempts: 3,
      },
      maybe_upload_staging_dir: Some(staging),
      maybe_download_dir: Some(directory.clone()),
      ..common::settings()
    };
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob =
      Peer::enroll_on_clock(server.add_user("Bob"), "Bob", settings, Box::new(clock.clone())).await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "files")
      .await
      .unwrap();
    alice
      .client
      .add_member(channel, bob.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(OutgoingMessage {
      conversation: channel,
      maybe_thread: None,
      body: String::new(),
      maybe_reply_to: None,
      priority: MessagePriority::Normal,
      wants_acknowledgement: false,
      is_silent: false,
      also_send_to_channel: false,
      files: vec![OutgoingFile {
        file_name: "archive.bin".into(),
        media_type: "application/octet-stream".into(),
        source: FileSource::Bytes { bytes: sample() },
        maybe_thumbnail: None,
        maybe_duration_ms: None,
      }],
    });
    run_commands(&mut alice, &alice_ui).await;
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_ui = backend_of(&bob);
    let count = bob_ui.message_count(channel);
    let message = bob_ui.messages(channel, count - 1, 1).remove(0).id;
    let attachment = bob_ui.message_extras(channel, &[message])[0].files[0]
      .file_id
      .parse()
      .unwrap();
    Self {
      server,
      bob,
      clock,
      channel,
      message,
      attachment,
      directory,
    }
  }

  /// Starts what is due and waits for everything started to finish.
  async fn run(&mut self, downloads: &mut Downloads) {
    downloads.start_due(&mut self.bob.client).await;
    while downloads.is_running() {
      let finished = downloads.next_finished().await;
      Downloads::finish(&mut self.bob.client, finished);
    }
  }

  fn transfer(&self) -> FileTransfer {
    backend_of(&self.bob).message_extras(self.channel, &[self.message])[0].files[0]
      .transfer
      .clone()
  }

  fn assert_saved(&self) {
    let FileTransfer::Local { path } = self.transfer() else {
      panic!("not saved: {:?}", self.transfer());
    };
    assert!(
      std::fs::read(&path).unwrap() == sample(),
      "the file arrives whole"
    );
    assert_eq!(self.partial_files(), 0, "the partial file is gone");
  }

  fn partial_files(&self) -> usize {
    std::fs::read_dir(self.directory.join("staging").join("partial-downloads"))
      .map(|entries| entries.count())
      .unwrap_or(0)
  }
}

impl Drop for Setup {
  fn drop(&mut self) {
    // Deliberately ignored: a test that failed may have left nothing.
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

fn sample() -> Vec<u8> {
  (0..FILE_BYTES).map(|index| (index * 7 % 251) as u8).collect()
}
