//! Large files go up in parts (issue #153): they arrive whole, an upload killed half way continues
//! with the next part after a restart, and a cancelled one is given up at the server.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, clock, run_commands, settings, settle};
use teamcraft_client_api::actions::{
  FileSource, FileTransfer, MessagePriority, OutgoingFile, OutgoingMessage,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Message;
use teamcraft_core::Client;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::files::UploadStep;
use teamcraft_core::client::upload_parts::PART_CHUNKS;
use teamcraft_crypto::attachment::DEFAULT_CHUNK_SIZE;
use teamcraft_proto::ConversationId;

/// Plaintext bytes per part.
const PART: usize = PART_CHUNKS as usize * DEFAULT_CHUNK_SIZE as usize;

#[tokio::test]
async fn a_large_file_goes_up_in_parts_and_arrives_whole() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let content = sample(2 * PART + 1000);
  alice_ui.send_message(with_file(
    channel,
    "video.mp4",
    FileSource::Bytes {
      bytes: content.clone(),
    },
  ));
  run_commands(&mut alice, &alice_ui).await;
  assert_eq!(server.part_upload_count(), 3);
  assert_eq!(server.attachment_count(), 1);

  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(downloaded(&mut bob, channel).await, content);
}

#[tokio::test]
async fn an_upload_killed_half_way_continues_with_the_next_part_after_a_restart() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let content = sample(3 * PART + 77);
  let path = scratch_file("half-way.bin", &content);
  alice_ui.send_message(with_file(
    channel,
    "backup.tar",
    FileSource::Path {
      path: path.to_string_lossy().into_owned(),
    },
  ));
  alice_ui.drain_commands();

  // Two of four parts go up, the third is on its way when the app is killed.
  for _ in 0..2 {
    let mut upload = alice.client.start_upload().await.unwrap().unwrap();
    let sent = upload.sent().await;
    assert_eq!(
      alice.client.finish_upload(upload, sent).await.unwrap(),
      UploadStep::PartUploaded
    );
  }
  let killed = alice.client.start_upload().await.unwrap().unwrap();
  drop(killed);
  assert_eq!(server.part_upload_count(), 2);
  let message = last(&alice_ui, channel);
  assert!(matches!(
    alice_ui.message_extras(channel, &[message.id])[0].files[0].transfer,
    FileTransfer::Uploading { .. }
  ));

  alice.client = Client::open(alice.api.clone(), alice.store.clone(), clock(), settings())
    .unwrap()
    .unwrap();
  let (gateway, _) = alice.client.connect_gateway().await.unwrap();
  alice.gateway = gateway;
  assert_eq!(alice.client.upload_pending().await.unwrap(), 1);
  assert_eq!(
    server.part_upload_count(),
    4,
    "only the parts after the last stored one went up again"
  );
  assert_eq!(server.attachment_count(), 1, "the same upload continued");
  alice.client.flush_outbox().await.unwrap();

  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(downloaded(&mut bob, channel).await, content);
  std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn a_cancelled_upload_is_given_up_at_the_server_also_after_being_offline() {
  let (server, mut alice, _bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(with_file(
    channel,
    "huge.iso",
    FileSource::Bytes {
      bytes: sample(PART + 1),
    },
  ));
  alice_ui.drain_commands();
  let mut upload = alice.client.start_upload().await.unwrap().unwrap();
  let sent = upload.sent().await;
  alice.client.finish_upload(upload, sent).await.unwrap();
  let running = alice.client.start_upload().await.unwrap().unwrap();

  // Cancelled while offline: the server hears of it once it is reached again.
  server.set_offline(alice.client.user_id(), true);
  let message = last(&alice_ui, channel);
  let file_id = alice_ui.message_extras(channel, &[message.id])[0].files[0]
    .file_id
    .clone();
  alice_ui.cancel_upload(channel, message.id, &file_id);
  assert!(!alice.client.is_upload_current(&running), "the host drops it");
  drop(running);
  assert!(alice.client.start_upload().await.unwrap().is_none());
  assert!(server.abandoned_attachments().is_empty());

  server.set_offline(alice.client.user_id(), false);
  assert!(alice.client.start_upload().await.unwrap().is_none());
  assert_eq!(server.abandoned_attachments().len(), 1);
  assert_eq!(server.attachment_count(), 0, "its parts are freed");
  assert!(alice.client.start_upload().await.unwrap().is_none());
  assert_eq!(server.abandoned_attachments().len(), 1, "given up once");
}

/// A part whose answer was lost is sent again; if the file changed meanwhile (here in place, with
/// the same size and modification time), the same key and nonces must not encrypt the new bytes.
#[tokio::test]
async fn a_part_sent_again_with_other_bytes_starts_the_upload_over_with_a_fresh_key() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let path = scratch_file("edited.bin", &sample(PART + 500));
  let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
  alice_ui.send_message(with_file(
    channel,
    "edited.bin",
    FileSource::Path {
      path: path.to_string_lossy().into_owned(),
    },
  ));
  alice_ui.drain_commands();
  // The first part leaves, but its answer is lost (the app stops).
  let lost = alice.client.start_upload().await.unwrap().unwrap();
  drop(lost);
  let first = server.attachment_count();
  assert_eq!(first, 1);

  let edited: Vec<u8> = sample(PART + 500).iter().map(|byte| byte ^ 0x5a).collect();
  std::fs::write(&path, &edited).unwrap();
  std::fs::File::options()
    .write(true)
    .open(&path)
    .unwrap()
    .set_modified(modified)
    .unwrap();
  assert_eq!(alice.client.upload_pending().await.unwrap(), 1);
  assert_eq!(
    server.abandoned_attachments().len(),
    1,
    "the first upload is given up, not continued"
  );
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(downloaded(&mut bob, channel).await, edited);
  std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn an_edited_file_starts_its_upload_over() {
  let (server, mut alice, _bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let path = scratch_file("touched.bin", &sample(2 * PART));
  alice_ui.send_message(with_file(
    channel,
    "touched.bin",
    FileSource::Path {
      path: path.to_string_lossy().into_owned(),
    },
  ));
  alice_ui.drain_commands();
  let mut upload = alice.client.start_upload().await.unwrap().unwrap();
  let sent = upload.sent().await;
  alice.client.finish_upload(upload, sent).await.unwrap();

  let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
  std::fs::File::options()
    .write(true)
    .open(&path)
    .unwrap()
    .set_modified(later)
    .unwrap();
  assert_eq!(alice.client.upload_pending().await.unwrap(), 1);
  assert_eq!(server.abandoned_attachments().len(), 1);
  assert_eq!(server.part_upload_count(), 3, "both parts went up again");
  std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
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
  (server, alice, bob, channel)
}

fn with_file(conversation: ConversationId, name: &str, source: FileSource) -> OutgoingMessage {
  OutgoingMessage {
    conversation,
    maybe_thread: None,
    body: String::new(),
    maybe_reply_to: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: vec![OutgoingFile {
      file_name: name.into(),
      media_type: "application/octet-stream".into(),
      source,
      maybe_thumbnail: None,
      maybe_duration_ms: None,
    }],
  }
}

/// The file of the last message, downloaded to disk and read back.
async fn downloaded(peer: &mut Peer, conversation: ConversationId) -> Vec<u8> {
  let ui = backend_of(peer);
  let received = last(&ui, conversation);
  let directory = scratch_dir(&format!("downloads-{}", peer.name));
  peer.client.set_download_dir(Some(directory.clone()));
  ui.download_file(conversation, received.id);
  run_commands(peer, &ui).await;
  let files = ui.message_extras(conversation, &[received.id])[0].files.clone();
  let FileTransfer::Local { path } = &files[0].transfer else {
    panic!("not downloaded: {:?}", files[0].transfer);
  };
  let content = std::fs::read(path).unwrap();
  std::fs::remove_dir_all(&directory).unwrap();
  content
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  let count = backend.message_count(conversation);
  backend.messages(conversation, count - 1, 1).remove(0)
}

fn sample(len: usize) -> Vec<u8> {
  (0..len).map(|index| (index % 251) as u8).collect()
}

/// Under the target directory: large test files don't belong in a RAM-backed `/tmp`.
fn scratch_dir(name: &str) -> std::path::PathBuf {
  let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
    "upload-parts-{name}-{}-{:?}",
    std::process::id(),
    std::thread::current().id()
  ));
  std::fs::create_dir_all(&directory).unwrap();
  directory
}

fn scratch_file(name: &str, content: &[u8]) -> std::path::PathBuf {
  let path = scratch_dir(name).join(name);
  std::fs::write(&path, content).unwrap();
  path
}
