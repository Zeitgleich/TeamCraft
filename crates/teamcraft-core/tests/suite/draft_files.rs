//! Files of unsent drafts (#1220): uploaded as they are attached, they travel with the draft to the
//! user's other devices, which send them without uploading them again.
#![cfg(feature = "ui")]
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, TestClient, backend_of, link_device, run_commands, settle};
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::drafts::{DraftFileState, DraftFileTravel};
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
    DraftFileTravel::WithDraft,
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
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![file("draft.txt", b"maybe")],
    DraftFileTravel::WithDraft,
  );
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
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![file("local.txt", b"here only")],
    DraftFileTravel::WithDraft,
  );
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
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![file("quick.txt", b"fast")],
    DraftFileTravel::WithDraft,
  );
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

#[tokio::test]
async fn a_send_that_overtakes_its_files_attachment_sends_the_file_once() {
  let (server, mut phone, _laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  let mut picked = file("raced.txt", b"overtaken");
  picked.maybe_draft_id = Some("0123456789abcdef0123456789abcdef".into());
  // The desktop app's actions may reach the core out of order: the send comes first.
  phone_ui.send_message(message(channel, "", vec![picked.clone()]));
  phone_ui.attach_draft_files(channel, None, vec![picked], DraftFileTravel::WithDraft);
  run_commands(&mut phone, &phone_ui).await;
  settle(&mut [&mut phone, &mut bob]).await;
  assert!(
    phone_ui.draft_files(channel, None).is_empty(),
    "not back in the draft"
  );
  assert_eq!(server.attachment_ids().len(), 1);
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(
    bob_ui.message_extras(channel, &[received.id])[0].files[0].file_name,
    "raced.txt"
  );
}

#[tokio::test]
async fn a_file_the_server_wont_keep_yet_waits_without_holding_up_other_messages() {
  let server = FakeServer::new();
  server.set_draft_file_retention_days(30);
  let clock = Arc::new(ManualClock::new(1_800_000_000_000));
  let mut alice = Peer::enroll_on_clock(
    server.add_user("Alice"),
    "Alice",
    common::settings(),
    Box::new(clock.clone()),
  )
  .await;
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
  let alice_ui = backend_of(&alice);
  alice_ui.attach_draft_files(
    channel,
    None,
    vec![file("held.txt", b"held")],
    DraftFileTravel::WithDraft,
  );
  run_commands(&mut alice, &alice_ui).await;
  let attached = alice_ui.draft_files(channel, None);

  server.fail_keeps(true);
  alice_ui.send_message(message(channel, "with the draft's file", drafts_files(&attached)));
  alice_ui.send_message(message(channel, "another one", vec![file("plain.txt", b"plain")]));
  // The keep fails and backs off; the other message's file goes up and it is sent.
  run_commands(&mut alice, &alice_ui).await;
  settle(&mut [&mut alice, &mut bob]).await;
  let bob_ui = backend_of(&bob);
  assert_eq!(last(&bob_ui, channel).body, "another one");

  server.fail_keeps(false);
  clock.advance(10 * 60 * 1000);
  alice.client.upload_pending().await.unwrap();
  alice.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let received = last(&bob_ui, channel);
  assert_eq!(received.body, "with the draft's file");
  assert_eq!(
    bob_ui.message_extras(channel, &[received.id])[0].files[0].file_name,
    "held.txt"
  );
}

#[tokio::test]
async fn a_file_removed_while_it_goes_up_stays_removed() {
  let (server, mut phone, _laptop, _bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![file("second thoughts.txt", b"no")],
    DraftFileTravel::WithDraft,
  );
  let attached = phone_ui.draft_files(channel, None);
  // The host drives the upload; the user removes the file while its blob is on the way.
  let mut upload = phone
    .client
    .start_upload()
    .await
    .unwrap()
    .expect("the file goes up");
  let announced = server.attachment_ids();
  assert_eq!(announced.len(), 1);
  phone_ui.remove_draft_file(channel, None, &attached[0].id);
  let sent = upload.sent().await;
  phone.client.finish_upload(upload, sent).await.unwrap();
  run_commands(&mut phone, &phone_ui).await;

  assert!(phone_ui.draft_files(channel, None).is_empty());
  assert!(phone.client.account_data().live_draft_files().next().is_none());
  assert_eq!(
    server.abandoned_attachments(),
    announced,
    "given up at the server"
  );
}

#[tokio::test]
async fn a_file_held_too_long_shows_expired_and_goes_up_again_from_where_it_was_picked() {
  let (server, mut phone, _laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  let mut picked = file("old.txt", b"from last month");
  picked.maybe_draft_id = Some("00112233445566778899aabbccddeeff".into());
  phone_ui.attach_draft_files(channel, None, vec![picked.clone()], DraftFileTravel::WithDraft);
  run_commands(&mut phone, &phone_ui).await;
  let first_upload = server.attachment_ids();

  // A month later, by the device's clock.
  let later = CoreChatBackend::new(
    phone.store.clone(),
    phone.client.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000 + 30 * 24 * 60 * 60 * 1000)),
  );
  assert_eq!(later.draft_files(channel, None)[0].state, DraftFileState::Expired);
  // The composer still has the picked file: it goes up again instead of being lost.
  later.send_message(message(channel, "still wanted", vec![picked]));
  run_commands(&mut phone, &later).await;
  settle(&mut [&mut phone, &mut bob]).await;
  assert!(
    server.kept_attachments().is_empty(),
    "the expired one is not counted on"
  );
  let uploads = server.attachment_ids();
  assert_eq!(uploads.len(), 2);
  let resent = uploads.iter().find(|id| !first_upload.contains(id)).unwrap();
  assert!(!server.is_held(*resent));
  assert!(later.draft_files(channel, None).is_empty());
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  assert_eq!(
    bob_ui.message_extras(channel, &[received.id])[0].files[0].file_name,
    "old.txt"
  );
}

#[tokio::test]
async fn files_attached_to_go_up_on_send_wait_on_the_device_and_go_up_with_their_message() {
  let (server, mut phone, mut laptop, mut bob, channel) = setup(Some(30)).await;
  let phone_ui = backend_of(&phone);
  phone_ui.attach_draft_files(
    channel,
    None,
    vec![picture("photo 1.png"), file("notes.txt", b"two")],
    DraftFileTravel::OnSend,
  );
  run_commands(&mut phone, &phone_ui).await;
  phone.client.upload_pending().await.unwrap();
  assert!(
    server.attachment_ids().is_empty(),
    "nothing announced or uploaded before the send"
  );
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  assert!(
    ui_of(&laptop).draft_files(channel, None).is_empty(),
    "the files don't travel to the other devices"
  );

  // The app was killed: a new adapter over the same store still has them.
  let restarted = CoreChatBackend::new(
    phone.store.clone(),
    phone.client.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  );
  let attached = restarted.draft_files(channel, None);
  let names: Vec<&str> = attached.iter().map(|item| item.file_name.as_str()).collect();
  assert_eq!(names, ["photo 1.png", "notes.txt"]);
  assert!(
    attached
      .iter()
      .all(|item| item.state == DraftFileState::OnThisDevice)
  );
  assert!(
    attached[0].maybe_thumbnail.is_some(),
    "the waiting picture shows its thumbnail"
  );

  restarted.send_message(message(channel, "both", drafts_files(&attached)));
  run_commands(&mut phone, &restarted).await;
  settle(&mut [&mut phone, &mut bob]).await;
  let uploaded = server.attachment_ids();
  assert_eq!(uploaded.len(), 2, "each file went up once, with its message");
  assert!(
    uploaded.iter().all(|id| !server.is_held(*id)),
    "a message's files, not a draft's"
  );
  assert!(restarted.draft_files(channel, None).is_empty());
  let bob_ui = backend_of(&bob);
  let received = last(&bob_ui, channel);
  let files = bob_ui.message_extras(channel, &[received.id])[0].files.clone();
  let names: Vec<&str> = files.iter().map(|file| file.file_name.as_str()).collect();
  assert_eq!(names, ["photo 1.png", "notes.txt"]);
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
      maybe_draft_id: None,
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
    maybe_draft_id: None,
  }
}

fn picture(name: &str) -> OutgoingFile {
  let image = image::RgbImage::from_pixel(64, 48, image::Rgb([200, 40, 40]));
  let mut bytes = std::io::Cursor::new(Vec::new());
  image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
  OutgoingFile {
    media_type: "image/png".into(),
    ..file(name, &bytes.into_inner())
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
