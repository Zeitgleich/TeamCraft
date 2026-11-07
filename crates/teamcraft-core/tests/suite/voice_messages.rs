//! Voice messages (#204): the waveform and the duration travel inside the encrypted message, so
//! the other device draws the message before downloading it, and the server sees an attachment
//! of some size and nothing else.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, backend_of, run_commands, settle};
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_proto::ConversationId;

/// A waveform no real recording produces, so it can't turn up in ciphertext by chance.
const WAVEFORM: [u8; 12] = [8, 255, 17, 200, 33, 150, 66, 99, 132, 48, 240, 12];
const DURATION_MS: u64 = 4_321;

#[tokio::test]
async fn waveform_and_duration_reach_the_other_device_and_nothing_readable_the_server() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let audio = b"OggS pretend opus bytes of a short voice message".to_vec();
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(voice(channel, &audio));
  run_commands(&mut alice, &alice_ui).await;
  alice.client.upload_pending().await.unwrap();
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let bob_ui = backend_of(&bob);
  let count = bob_ui.message_count(channel);
  let received = bob_ui.messages(channel, count - 1, 1).remove(0);
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  assert_eq!(files.len(), 1);
  assert_eq!(files[0].media_type, "audio/ogg");
  assert_eq!(files[0].maybe_duration_ms, Some(DURATION_MS));
  assert_eq!(files[0].maybe_waveform.as_deref(), Some(WAVEFORM.as_slice()));
  assert_eq!(files[0].size_bytes, audio.len() as u64);

  assert_eq!(server.attachment_count(), 1);
  for stored in server
    .stored_ciphertexts()
    .iter()
    .chain(server.stored_blobs().iter())
  {
    for readable in [
      WAVEFORM.as_slice(),
      b"waveform",
      b"duration_ms",
      b"voice-message",
      b"OggS",
    ] {
      assert!(
        !stored.windows(readable.len()).any(|window| window == readable),
        "the server holds {:?} in the clear",
        String::from_utf8_lossy(readable)
      );
    }
  }
}

#[tokio::test]
async fn a_waveform_out_of_bounds_is_dropped_and_the_file_stays_plain_audio() {
  let (_server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_ui = backend_of(&alice);
  let mut message = voice(channel, b"OggS bytes");
  message.files[0].maybe_waveform = Some(vec![1; 500]);
  alice_ui.send_message(message);
  run_commands(&mut alice, &alice_ui).await;
  alice.client.upload_pending().await.unwrap();
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let bob_ui = backend_of(&bob);
  let count = bob_ui.message_count(channel);
  let received = bob_ui.messages(channel, count - 1, 1).remove(0);
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  assert_eq!(files[0].maybe_waveform, None);
  assert_eq!(files[0].maybe_duration_ms, Some(DURATION_MS));
}

fn voice(conversation: ConversationId, bytes: &[u8]) -> OutgoingMessage {
  OutgoingMessage {
    maybe_client_message_id: None,
    conversation,
    maybe_thread: None,
    body: String::new(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: vec![OutgoingFile {
      file_name: "voice-message-20261105-140312.ogg".into(),
      media_type: "audio/ogg".into(),
      source: FileSource::Bytes {
        bytes: bytes.to_vec(),
      },
      maybe_thumbnail: None,
      maybe_duration_ms: Some(DURATION_MS),
      maybe_waveform: Some(WAVEFORM.to_vec()),
      maybe_draft_id: None,
    }],
  }
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "voice")
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
