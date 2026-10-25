//! A sent message shows at once and stays until the server has it: offline it waits in the outbox
//! (also across a restart) and goes out when the network is back; a message the server refuses for
//! good says so, with "Try again" and "Delete"; a file still going up can be cancelled.
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, backend_of, clock, run_commands, settings, settle};
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::model::{Message, MessageKind, is_pending};
use teamcraft_core::Client;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_proto::ConversationId;

#[tokio::test]
async fn a_message_sent_offline_shows_at_once_survives_a_restart_and_goes_out_when_back() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  let alice_id = alice.client.user_id();
  server.set_offline(alice_id, true);
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(text(channel, "written in the tunnel"));
  let shown = last(&alice_ui, channel);
  assert_eq!(
    shown.body, "written in the tunnel",
    "shown before the server answered"
  );
  assert!(is_pending(shown.id));
  run_commands(&mut alice, &alice_ui).await;
  let still = last(&alice_ui, channel);
  assert!(is_pending(still.id), "offline it waits, it doesn't fail");
  assert!(!alice_ui.message_extras(channel, &[still.id])[0].is_send_failed);

  // The app is killed and started again: the message is still there, still waiting.
  let store: Arc<MemoryStore> = alice.store.clone();
  drop(alice.client);
  alice.client = Client::open(alice.api.clone(), store.clone(), clock(), settings())
    .unwrap()
    .unwrap();
  let reopened = CoreChatBackend::new(store, alice_id, clock());
  assert!(is_pending(last(&reopened, channel).id));

  server.set_offline(alice_id, false);
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(
    !is_pending(last(&reopened, channel).id),
    "confirmed by the server"
  );
  assert_eq!(last(&backend_of(&bob), channel).body, "written in the tunnel");
}

#[tokio::test]
async fn a_message_the_server_refuses_shows_as_not_sent_until_deleted() {
  let (server, mut alice, mut bob, channel) = channel_with_two().await;
  // Removed from the channel meanwhile: the server refuses what this device still sends.
  server.remove_member_as_admin(channel, alice.client.user_id());
  let alice_ui = backend_of(&alice);
  alice_ui.send_message(text(channel, "too late"));
  run_commands(&mut alice, &alice_ui).await;
  let refused = last(&alice_ui, channel);
  assert_eq!(refused.body, "too late");
  assert!(alice_ui.message_extras(channel, &[refused.id])[0].is_send_failed);

  // "Try again" puts it back in line; refused again, it says so again.
  alice_ui.retry_send(channel, refused.id);
  run_commands(&mut alice, &alice_ui).await;
  assert!(alice_ui.message_extras(channel, &[refused.id])[0].is_send_failed);

  alice_ui.discard_unsent(channel, refused.id);
  let discarded = last(&alice_ui, channel);
  assert!(!alice_ui.message_extras(channel, &[discarded.id])[0].is_send_failed);
  assert_ne!(discarded.body, "too late");
  settle(&mut [&mut alice, &mut bob]).await;
  assert!(
    bob_ui_bodies(&bob, channel).iter().all(|body| body != "too late"),
    "never sent"
  );
}

mod cancel {
  use super::*;

  #[tokio::test]
  async fn a_cancelled_file_is_left_out_of_its_message() {
    let (_server, mut alice, mut bob, channel) = channel_with_two().await;
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(with_files(
      channel,
      "two files, one kept",
      vec![file("keep.txt", b"keep"), file("drop.txt", b"drop")],
    ));
    let pending = last(&alice_ui, channel);
    alice_ui.cancel_upload(channel, pending.id, "pending-1");
    let files = alice_ui.message_extras(channel, &[pending.id])[0].files.clone();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name, "keep.txt");

    run_commands(&mut alice, &alice_ui).await;
    alice.client.upload_pending().await.unwrap();
    alice.client.flush_outbox().await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let bob_ui = backend_of(&bob);
    let received = last(&bob_ui, channel);
    assert_eq!(received.body, "two files, one kept");
    let names: Vec<String> = bob_ui.message_extras(channel, &[received.id])[0]
      .files
      .iter()
      .map(|file| file.file_name.clone())
      .collect();
    assert_eq!(names, ["keep.txt"]);
  }

  #[tokio::test]
  async fn cancelling_the_only_file_of_a_message_without_text_drops_it() {
    let (_server, mut alice, mut bob, channel) = channel_with_two().await;
    let before = backend_of(&bob).message_count(channel);
    let alice_ui = backend_of(&alice);
    alice_ui.send_message(with_files(channel, "", vec![file("photo.jpg", b"pixels")]));
    let pending = last(&alice_ui, channel);
    alice_ui.cancel_upload(channel, pending.id, "pending-0");
    assert!(
      alice_ui.message_extras(channel, &[pending.id])[0]
        .files
        .is_empty()
    );

    run_commands(&mut alice, &alice_ui).await;
    alice.client.upload_pending().await.unwrap();
    assert_eq!(alice.client.flush_outbox().await.unwrap().sent, 0);
    settle(&mut [&mut alice, &mut bob]).await;
    assert_eq!(
      backend_of(&bob).message_count(channel),
      before,
      "nothing was sent"
    );
  }
}

async fn channel_with_two() -> (FakeServer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "sending")
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

fn text(conversation: ConversationId, body: &str) -> OutgoingMessage {
  with_files(conversation, body, Vec::new())
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

fn file(name: &str, bytes: &[u8]) -> OutgoingFile {
  OutgoingFile {
    file_name: name.into(),
    media_type: "text/plain".into(),
    source: FileSource::Bytes {
      bytes: bytes.to_vec(),
    },
    maybe_thumbnail: None,
  }
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  let count = backend.message_count(conversation);
  backend.messages(conversation, count - 1, 1).remove(0)
}

fn bob_ui_bodies(bob: &Peer, conversation: ConversationId) -> Vec<String> {
  let backend = backend_of(bob);
  let count = backend.message_count(conversation);
  backend
    .messages(conversation, 0, count)
    .into_iter()
    .filter(|message| message.kind == MessageKind::User)
    .map(|message| message.body)
    .collect()
}
