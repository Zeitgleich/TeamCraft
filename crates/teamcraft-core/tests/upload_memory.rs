//! A large file goes up with bounded memory (issue #153): one part at a time is read, encrypted and
//! sent, never the whole file. Its own test binary: the counting allocator sees every allocation of
//! the process, so no other test may run beside it.
#![cfg(all(feature = "ui", not(target_arch = "wasm32")))]
#![allow(clippy::unwrap_used)]

#[path = "common/mod.rs"]
mod common;

use common::{Peer, backend_of, settle};
use peak_alloc::PeakAlloc;
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::upload_parts::PART_CHUNKS;
use teamcraft_crypto::attachment::DEFAULT_CHUNK_SIZE;

#[global_allocator]
static ALLOCATOR: PeakAlloc = PeakAlloc;

const MIB: usize = 1024 * 1024;
const PART: usize = PART_CHUNKS as usize * DEFAULT_CHUNK_SIZE as usize;
/// Six parts; read whole, the file and its ciphertext alone would take 96 MiB.
const FILE_BYTES: usize = 6 * PART;
/// A few parts' worth: plaintext and ciphertext of the part on its way, and the transport's copy.
const MAX_EXTRA_BYTES: usize = 4 * PART;

#[tokio::test]
async fn a_large_file_goes_up_one_part_at_a_time() {
  let server = FakeServer::new();
  server.discard_parts(true);
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "files")
    .await
    .unwrap();
  settle(&mut [&mut alice]).await;
  let path = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
    .join(format!("upload-memory-{}.bin", std::process::id()));
  write_sample(&path);

  let ui = backend_of(&alice);
  ui.send_message(OutgoingMessage {
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
      source: FileSource::Path {
        path: path.to_string_lossy().into_owned(),
      },
      maybe_thumbnail: None,
    }],
  });
  ui.drain_commands();
  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  assert_eq!(alice.client.upload_pending().await.unwrap(), 1);
  let extra = ALLOCATOR.peak_usage().saturating_sub(before);
  std::fs::remove_file(&path).unwrap();

  assert_eq!(server.part_upload_count(), 6);
  assert!(
    extra < MAX_EXTRA_BYTES,
    "uploading {} MiB took {} MiB at its peak",
    FILE_BYTES / MIB,
    extra / MIB
  );
}

/// Written a part at a time, so the test itself doesn't hold the file either.
fn write_sample(path: &std::path::Path) {
  use std::io::Write;

  let mut file = std::fs::File::create(path).unwrap();
  let part: Vec<u8> = (0..PART).map(|index| (index % 251) as u8).collect();
  for _ in 0..FILE_BYTES / PART {
    file.write_all(&part).unwrap();
  }
}
