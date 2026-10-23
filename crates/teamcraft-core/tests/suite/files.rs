//! Files from the UI: several files with text, thumbnails, uploads that resume after network loss,
//! the size limit, downloads to disk and share links.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::io::Cursor;

use common::{Peer, backend_of, run_commands, settle};
use image::{ImageBuffer, ImageFormat, Rgb};
use teamcraft_client_api::actions::{
  FileFailure, FileSource, FileTransfer, MessagePriority, OutgoingFile, OutgoingMessage, ShareLinkOptions,
};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::Message;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::upkeep::Upkeep;
use teamcraft_crypto::attachment::decrypt_attachment;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_crypto::share_link::open_fragment;
use teamcraft_proto::ConversationId;

#[tokio::test]
async fn text_with_files_arrives_with_thumbnails_and_downloads() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let (alice_ui, bob_ui) = (backend_of(&alice), backend_of(&bob));
  alice_ui.send_message(with_files(
    channel,
    "photos from today",
    vec![
      file("beach.jpg", "image/jpeg", jpeg()),
      file("notes.txt", "text/plain", b"sand".to_vec()),
    ],
  ));
  // Shown at once, uploading.
  let pending = last(&alice_ui, channel);
  let files = &alice_ui.message_extras(channel, &[pending.id])[0].files;
  assert_eq!(files.len(), 2);
  assert!(matches!(
    files[0].transfer,
    FileTransfer::Uploading { done_bytes: 0, .. }
  ));

  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let received = last(&bob_ui, channel);
  assert_eq!(received.body, "photos from today");
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  assert_eq!(files[0].file_name, "beach.jpg");
  assert!(files[0].maybe_thumbnail_png.is_some(), "images get a thumbnail");
  assert!(files[1].maybe_thumbnail_png.is_none());
  assert_eq!(files[1].transfer, FileTransfer::Remote);
  assert_eq!(bob_ui.conversation_files(channel).len(), 2);

  let directory = std::env::temp_dir().join(format!("teamcraft-downloads-{}", std::process::id()));
  bob.client.set_download_dir(Some(directory.clone()));
  bob_ui.download_file(channel, received.id);
  run_commands(&mut bob, &bob_ui).await;
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  let FileTransfer::Local { path } = &files[1].transfer else {
    panic!("not downloaded: {:?}", files[1].transfer);
  };
  assert_eq!(std::fs::read(path).unwrap(), b"sand");
  std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test]
async fn uploads_resume_after_network_loss_and_respect_the_size_limit() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  server.set_max_attachment_bytes(1024);
  server.set_offline(alice.client.user_id(), true);
  alice_ui.send_message(with_files(
    channel,
    "",
    vec![
      file("small.txt", "text/plain", b"fits".to_vec()),
      file("big.bin", "application/octet-stream", vec![7; 4096]),
    ],
  ));
  for command in alice_ui.drain_commands() {
    // Offline: the upload stops and stays pending.
    let _ = alice.client.run_ui_command(command).await;
  }
  let message = last(&alice_ui, channel);
  assert!(matches!(
    alice_ui.message_extras(channel, &[message.id])[0].files[0].transfer,
    FileTransfer::Uploading { .. }
  ));
  assert_eq!(alice.client.flush_outbox().await.unwrap_or_default().sent, 0);

  server.set_offline(alice.client.user_id(), false);
  alice.client.upload_pending().await.unwrap();
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  assert_eq!(files.len(), 1, "only the file within the limit went out");
  assert_eq!(received.body, "📎 small.txt");
  // The sender still sees which file didn't make it, and why.
  let sent = last(&alice_ui, channel);
  let mine = alice_ui.message_extras(channel, &[sent.id])[0].files.clone();
  assert_eq!(mine.len(), 2);
  assert_eq!(mine[1].file_name, "big.bin");
  assert_eq!(
    mine[1].transfer,
    FileTransfer::Failed {
      reason: FileFailure::TooLarge
    }
  );
}

/// Every app's loop retries through the same upkeep (the desktop app once skipped uploads).
#[tokio::test]
async fn the_periodic_upkeep_uploads_and_sends_what_waited_for_the_network() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  server.set_offline(alice.client.user_id(), true);
  alice_ui.send_message(with_files(
    channel,
    "written on the train",
    vec![file("notes.txt", "text/plain", b"sand".to_vec())],
  ));
  for command in alice_ui.drain_commands() {
    let _ = alice.client.run_ui_command(command).await;
  }

  server.set_offline(alice.client.user_id(), false);
  let mut upkeep = Upkeep::new(std::time::Duration::from_secs(60));
  assert!(upkeep.tick(&mut alice.client).await);
  settle(&mut [&mut alice, &mut bob]).await;
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(received.body, "written on the train");
  assert_eq!(bob_ui.message_extras(channel, &[received.id])[0].files.len(), 1);
}

#[tokio::test]
async fn share_links_carry_the_key_only_in_the_fragment() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(with_files(
    channel,
    "",
    vec![file("contract.pdf", "application/pdf", b"%PDF signed".to_vec())],
  ));
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let message = last(&alice_ui, channel);
  alice_ui.create_share_link(
    channel,
    message.id,
    ShareLinkOptions {
      maybe_password: Some("open sesame".into()),
      maybe_max_downloads: Some(3),
      ..ShareLinkOptions::default()
    },
  );
  run_commands(&mut alice, &alice_ui).await;

  let link = alice_ui.share_links(channel, message.id).remove(0);
  assert!(link.has_password);
  let (page, fragment) = link.url.split_once('#').unwrap();
  let share_id = page.rsplit('/').next().unwrap();
  // What a downloader without an account does: ciphertext from the server, key from the fragment.
  let ciphertext = server.shared_blob(share_id).unwrap();
  let opened = open_fragment(fragment, Some(&SecretBytes::new(b"open sesame".to_vec()))).unwrap();
  assert_eq!(opened.file_name, "contract.pdf");
  assert_eq!(
    decrypt_attachment(&opened.key, &ciphertext)
      .unwrap()
      .expose_secret(),
    b"%PDF signed"
  );
  assert!(!format!("{link:?}").contains(fragment), "Debug hides the key");
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

fn with_files(conversation: ConversationId, body: &str, files: Vec<OutgoingFile>) -> OutgoingMessage {
  OutgoingMessage {
    conversation,
    maybe_thread: None,
    body: body.into(),
    maybe_reply_to: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files,
  }
}

fn file(name: &str, media_type: &str, bytes: Vec<u8>) -> OutgoingFile {
  OutgoingFile {
    file_name: name.into(),
    media_type: media_type.into(),
    source: FileSource::Bytes { bytes },
  }
}

fn jpeg() -> Vec<u8> {
  let picture = ImageBuffer::from_fn(64, 48, |x, y| Rgb([x as u8 * 4, y as u8 * 5, 120]));
  let mut out = Cursor::new(Vec::new());
  picture.write_to(&mut out, ImageFormat::Jpeg).unwrap();
  out.into_inner()
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  let count = backend.message_count(conversation);
  backend.messages(conversation, count - 1, 1).remove(0)
}
