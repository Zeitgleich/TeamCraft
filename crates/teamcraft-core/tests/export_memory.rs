//! A data export of a large history keeps memory bounded (issue #211): a page of messages at a
//! time is read and written, never a conversation or the archive. Its own test binary: the counting
//! allocator sees every allocation of the process, so no other test may run beside it.
#![cfg(all(feature = "ui", not(target_arch = "wasm32")))]
#![allow(clippy::unwrap_used)]

#[path = "common/mod.rs"]
mod common;

use common::{Peer, settle};
use peak_alloc::PeakAlloc;
use teamcraft_client_api::data_export::{DataExportLabels, DataExportOptions, DataExportScope};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::export::{ExportError, ExportJob, ExportSink, ExportStep};
use teamcraft_core::store::{LocalStore, MessageContent, MessageMeta, StoredMessage};
use teamcraft_proto::{ClientMessageId, Id, MessageId, Snowflake};

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const MIB: usize = 1024 * 1024;
/// About 40 MiB of text in the store.
const MESSAGES: u64 = 40_000;
const BODY_BYTES: usize = 1_000;
/// A page of messages with their records and the store's sorted list of the feed's places.
const MAX_EXTRA_BYTES: usize = 8 * MIB;

#[tokio::test]
async fn a_large_history_is_exported_a_page_at_a_time() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "archive")
    .await
    .unwrap();
  settle(&mut [&mut alice]).await;
  let me = alice.client.user_id();
  let body = "x".repeat(BODY_BYTES);
  for number in 1..=MESSAGES {
    alice
      .store
      .upsert_message(&StoredMessage {
        conversation_id: channel,
        client_message_id: ClientMessageId::random(),
        maybe_server_id: Some(MessageId::from_snowflake(
          Snowflake::new(1_000 + number as i64).unwrap(),
        )),
        sender_user_id: me,
        maybe_thread_id: None,
        sent_at_ms: 1_700_000_000_000 + number as i64,
        content: MessageContent::Text {
          body: body.clone(),
          mentions: Vec::new(),
          meta: MessageMeta::default(),
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: None,
        maybe_server_change: None,
        maybe_unreadable: None,
      })
      .unwrap();
  }
  let mut job = ExportJob::start(
    &alice.client,
    DataExportOptions {
      scope: DataExportScope::All,
      includes_files: false,
    },
    DataExportLabels::default(),
    "memory".into(),
  )
  .unwrap();
  let mut sink = CountingSink::default();

  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  loop {
    match job.step(&mut alice.client, &mut sink).await.unwrap() {
      ExportStep::Done => break,
      ExportStep::Continue => {},
      ExportStep::RetryAt(_) => panic!("the fake server doesn't fail here"),
    }
  }
  let extra = ALLOCATOR.peak_usage().saturating_sub(before);

  assert_eq!(job.progress().messages, MESSAGES);
  assert!(
    sink.bytes > MESSAGES * BODY_BYTES as u64,
    "every message went out: {} bytes",
    sink.bytes
  );
  assert!(
    extra < MAX_EXTRA_BYTES,
    "exporting {} MiB of messages took {} MiB at its peak",
    MESSAGES as usize * BODY_BYTES / MIB,
    extra / MIB
  );
}

/// Counts what goes out and keeps nothing, like a file on disk.
#[derive(Default)]
struct CountingSink {
  bytes: u64,
}

impl ExportSink for CountingSink {
  fn write_all(&mut self, bytes: &[u8]) -> Result<(), ExportError> {
    self.bytes += bytes.len() as u64;
    Ok(())
  }

  fn flush(&mut self) -> Result<(), ExportError> {
    Ok(())
  }
}
