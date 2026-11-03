//! A large file comes down to disk with bounded memory (issue #153): the ciphertext goes piece by
//! piece into a partial file and is decrypted from there chunk by chunk, never held whole. Its own
//! test binary: the counting allocator sees every allocation of the process, so no other test may
//! run beside it.
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
/// Five parts; read whole, the ciphertext and the plaintext alone would take 80 MiB.
const FILE_BYTES: usize = 5 * PART;
/// The read and write buffers and a piece or two in flight.
const MAX_EXTRA_BYTES: usize = 2 * MIB;

#[tokio::test]
async fn a_large_file_comes_down_a_piece_at_a_time() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
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
  let directory =
    std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("download-memory-{}", std::process::id()));
  std::fs::create_dir_all(&directory).unwrap();
  let source = directory.join("source.bin");
  write_sample(&source);
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
        path: source.to_string_lossy().into_owned(),
      },
      maybe_thumbnail: None,
      maybe_duration_ms: None,
    }],
  });
  ui.drain_commands();
  assert_eq!(alice.client.upload_pending().await.unwrap(), 1);
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let bob_ui = backend_of(&bob);
  let count = bob_ui.message_count(channel);
  let message = bob_ui.messages(channel, count - 1, 1).remove(0).id;
  let attachment = bob_ui.message_extras(channel, &[message])[0].files[0]
    .file_id
    .parse()
    .unwrap();
  let saved_into = directory.join("saved");
  bob.client.set_download_dir(Some(saved_into.clone()));

  let before = ALLOCATOR.current_usage();
  ALLOCATOR.reset_peak_usage();
  let path = bob
    .client
    .download_attachment_to_disk(channel, attachment)
    .await
    .unwrap();
  let extra = ALLOCATOR.peak_usage().saturating_sub(before);

  let size = std::fs::metadata(&path).unwrap().len();
  std::fs::remove_dir_all(&directory).unwrap();
  assert_eq!(size, FILE_BYTES as u64);
  assert!(
    extra < MAX_EXTRA_BYTES,
    "downloading {} MiB took {} KiB at its peak",
    FILE_BYTES / MIB,
    extra / 1024
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
