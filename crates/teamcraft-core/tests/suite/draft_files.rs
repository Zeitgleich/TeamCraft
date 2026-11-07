//! Files of unsent drafts (#1220): uploaded as they are attached, they travel with the draft to the
//! user's other devices, which send them without uploading them again.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, TestClient, backend_of, link_device, run_commands, settle};
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::drafts::DraftFileState;
use teamcraft_client_api::model::Message;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::MemoryStore;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_proto::{AttachmentId, ConversationId};

#[tokio::test]
async fn a_drafts_files_travel_to_the_other_device_and_go_out_from_there_unchanged() {
  let (server, mut phone, mut laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![file("notes.txt", b"sand"), file("plan.txt", b"dunes")],
  );
  assert!(
    phone_ui
      .draft_files(channel, None)
      .iter()
      .all(|item| item.state == DraftFileState::Uploading)
  );
  run_commands(&mut phone, &phone_ui).await;
  let attached = phone_ui.draft_files(channel, None);
  let names: Vec<&str> = attached.iter().map(|item| item.file_name.as_str()).collect();
  assert_eq!(names, ["notes.txt", "plan.txt"], "in the order attached");
  assert!(attached.iter().all(|item| item.state == DraftFileState::Ready));
  let uploaded = server.attachment_ids();
  assert_eq!(uploaded.len(), 2);
  assert!(
    uploaded.iter().all(|id| server.is_held(*id)),
    "held for the draft"
  );

  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  let laptop_ui = ui_of(&laptop);
  let on_laptop = laptop_ui.draft_files(channel, None);
  assert_eq!(on_laptop, attached, "the same files, ready on the laptop");
  assert_eq!(
    laptop_ui.drafts(None)[0].file_count,
    2,
    "a draft of files only is listed"
  );

  laptop_ui.send_message(message(channel, "here they are", drafts_files(&on_laptop)));
  run(&mut laptop, &laptop_ui).await;
  assert_eq!(server.attachment_ids().len(), 2, "nothing uploaded again");
  let mut kept = server.kept_attachments();
  kept.sort();
  assert_eq!(kept, uploaded);
  assert!(laptop_ui.draft_files(channel, None).is_empty());

  settle(&mut [&mut bob]).await;
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(received.body, "here they are");
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  let names: Vec<&str> = files.iter().map(|file| file.file_name.as_str()).collect();
  assert_eq!(names, ["notes.txt", "plan.txt"]);

  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();
  assert!(
    phone_ui.draft_files(channel, None).is_empty(),
    "sent: gone from the draft on every device"
  );
}

#[tokio::test]
async fn a_file_removed_on_one_device_is_given_up_and_another_sends_without_it() {
  let (server, mut phone, mut laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(channel, None, vec![file("draft.txt", b"maybe")]);
  run_commands(&mut phone, &phone_ui).await;
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  let laptop_ui = ui_of(&laptop);
  let stale = laptop_ui.draft_files(channel, None);
  assert_eq!(stale.len(), 1);

  let uploaded: AttachmentId = server.attachment_ids()[0];
  phone_ui.remove_draft_file(channel, None, &stale[0].id);
  run_commands(&mut phone, &phone_ui).await;
  assert!(phone_ui.draft_files(channel, None).is_empty());
  assert_eq!(server.abandoned_attachments(), [uploaded]);

  // The laptop hasn't heard of the removal yet and sends the draft as it knows it.
  laptop_ui.send_message(message(channel, "only the text", drafts_files(&stale)));
  run(&mut laptop, &laptop_ui).await;
  assert!(server.kept_attachments().is_empty());
  settle(&mut [&mut bob]).await;
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(received.body, "only the text");
  assert!(bob_ui.message_extras(channel, &[received.id])[0].files.is_empty());
}

#[tokio::test]
async fn an_older_server_keeps_a_drafts_files_on_the_device_until_they_are_sent() {
  let (server, mut phone, _laptop, mut bob, channel) = setup(None).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(channel, None, vec![file("local.txt", b"here only")]);
  run_commands(&mut phone, &phone_ui).await;
  let attached = phone_ui.draft_files(channel, None);
  assert_eq!(attached[0].state, DraftFileState::OnThisDevice);
  assert!(
    server.attachment_ids().is_empty(),
    "nothing went up for the draft"
  );

  phone_ui.send_message(message(channel, "", drafts_files(&attached)));
  run_commands(&mut phone, &phone_ui).await;
  settle(&mut [&mut phone, &mut bob]).await;
  let uploaded = server.attachment_ids();
  assert_eq!(uploaded.len(), 1);
  assert!(!server.is_held(uploaded[0]), "a message's file, not a draft's");
  assert!(phone_ui.draft_files(channel, None).is_empty());
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(
    bob_ui.message_extras(channel, &[received.id])[0].files[0].file_name,
    "local.txt"
  );
}

#[tokio::test]
async fn a_file_sent_while_still_going_up_continues_as_the_messages_and_is_kept() {
  let (server, mut phone, _laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(channel, None, vec![file("quick.txt", b"fast")]);
  let attached = phone_ui.draft_files(channel, None);
  // Sent before the upload step ran.
  phone_ui.send_message(message(channel, "", drafts_files(&attached)));
  run_commands(&mut phone, &phone_ui).await;
  settle(&mut [&mut phone, &mut bob]).await;
  let uploaded = server.attachment_ids();
  assert_eq!(uploaded.len(), 1);
  assert!(!server.is_held(uploaded[0]));
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(
    bob_ui.message_extras(channel, &[received.id])[0].files[0].file_name,
    "quick.txt"
  );
}

/// Alice's phone and laptop and Bob in one channel; the server holds drafts' files for
/// `maybe_retention_days` (`None`: an older server that holds none).
async fn setup(maybe_retention_days: Option<u32>) -> (FakeServer, Peer, TestClient, Peer, ConversationId) {
  let server = FakeServer::new();
  if let Some(days) = maybe_retention_days {
    server.set_draft_file_retention_days(days);
  }
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "files")
    .await
    .unwrap();
  phone
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  let mut laptop = link_device(&mut phone).await;
  phone.client.add_missing_devices(channel).await.unwrap();
  // Bob's device takes in the laptop before it writes; the laptop syncs as at a start.
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.sync_account_data().await.unwrap();
  laptop.connect_gateway().await.unwrap();
  (server, phone, laptop, bob, channel)
}

fn ui_of(client: &TestClient) -> CoreChatBackend<Arc<MemoryStore>> {
  CoreChatBackend::new(
    client.store().clone(),
    client.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  )
}

/// Runs what the laptop's UI queued and sends its outbox.
async fn run(client: &mut TestClient, ui: &CoreChatBackend<Arc<MemoryStore>>) {
  for command in ui.drain_commands() {
    client.run_ui_command(command).await.unwrap();
  }
  client.upload_pending().await.unwrap();
  client.flush_outbox().await.unwrap();
}

fn drafts_files(items: &[teamcraft_client_api::drafts::DraftFileItem]) -> Vec<OutgoingFile> {
  items
    .iter()
    .map(|item| OutgoingFile {
      file_name: item.file_name.clone(),
      media_type: item.media_type.clone(),
      source: FileSource::Draft { id: item.id.clone() },
      maybe_thumbnail: None,
      maybe_duration_ms: None,
      maybe_waveform: None,
    })
    .collect()
}

fn file(name: &str, bytes: &[u8]) -> OutgoingFile {
  OutgoingFile {
    file_name: name.into(),
    media_type: "text/plain".into(),
    source: FileSource::Bytes {
      bytes: bytes.to_vec(),
    },
    maybe_thumbnail: None,
    maybe_duration_ms: None,
    maybe_waveform: None,
  }
}

fn message(conversation: ConversationId, body: &str, files: Vec<OutgoingFile>) -> OutgoingMessage {
  OutgoingMessage {
    maybe_client_message_id: None,
    conversation,
    maybe_thread: None,
    body: body.into(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files,
  }
}

fn last(backend: &impl ChatBackend, conversation: ConversationId) -> Message {
  let count = backend.message_count(conversation);
  backend.messages(conversation, count - 1, 1).remove(0)
}
