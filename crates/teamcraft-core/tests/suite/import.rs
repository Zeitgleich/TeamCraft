//! History imported from another chat (e2ee.md §4.10, issue #402), against the fake server: the
//! import account posts encrypted history into a channel and a DM, members (also one who sets up
//! the app only afterwards) read it with its original author, and the import account leaves.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::import::ImportDraft;
use teamcraft_core::imported::{ImportSource, ImportedAuthor, ImportedOrigin};
use teamcraft_core::store::{LocalStore, MessageContent, MessageFilter, MessageMeta};
use teamcraft_proto::{ConversationId, HistoryPolicy};

/// 2024-05-01.
const OLD_MS: i64 = 1_714_521_600_000;

#[tokio::test]
async fn a_channels_history_reaches_its_members_also_one_who_sets_up_the_app_later() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let space = importer.space().await;
  let channel = importer.client.create_channel(space, "History").await.unwrap();

  let ids = importer
    .client
    .send_imported(
      channel,
      &[
        draft(
          "1",
          "first",
          ImportedAuthor::User {
            user_id: alice.client.user_id(),
          },
          OLD_MS,
        ),
        draft("2", "second", ImportedAuthor::Deleted, OLD_MS + 1),
      ],
    )
    .await
    .unwrap();
  assert!(ids[0] < ids[1]);
  // A second run sends the same IDs and stores nothing new.
  let again = importer
    .client
    .send_imported(channel, &[draft("1", "first", ImportedAuthor::Deleted, OLD_MS)])
    .await
    .unwrap();
  assert_eq!(again[0], ids[0]);

  for user in [alice.client.user_id(), bob_id] {
    importer
      .client
      .add_member_with_history(channel, user, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }
  settle(&mut [&mut importer, &mut alice]).await;
  alice.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(alice.texts(channel), vec!["first", "second"]);
  assert_eq!(
    origin_of(&alice, channel, "first").author,
    ImportedAuthor::User {
      user_id: alice.client.user_id()
    }
  );

  // Bob sets up the app only now: the import device adds him with the history.
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  bob.client.full_sync().await.unwrap();
  bob.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(bob.texts(channel), vec!["first", "second"]);
}

#[tokio::test]
async fn a_late_user_whose_device_also_asks_to_be_let_in_gets_the_history() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob_api = server.add_user("Bob");
  let bob_id = bob_api.user_id();
  let space = importer.space().await;
  let channel = importer.client.create_channel(space, "History").await.unwrap();
  importer
    .client
    .send_imported(
      channel,
      &[draft("1", "before Bob", ImportedAuthor::Deleted, OLD_MS)],
    )
    .await
    .unwrap();
  for user in [alice.client.user_id(), bob_id] {
    importer
      .client
      .add_member_with_history(channel, user, Some(HistoryPolicy::Full))
      .await
      .unwrap();
  }
  settle(&mut [&mut importer, &mut alice]).await;

  // Bob's first device syncs (and may ask to be let in) while Alice's device is online and the
  // import device is not.
  let mut bob = Peer::enroll_as(bob_api, "Bob").await;
  bob.client.full_sync().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // The import device, which decided to share the history with him, adds him or, if somebody let
  // him in from the join on meanwhile, seals the history to his device.
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  bob.client.catch_up_messages(channel).await.unwrap();
  assert_eq!(bob.texts(channel), vec!["before Bob"]);
}

#[tokio::test]
async fn a_dm_is_founded_by_the_import_account_which_leaves_once_both_are_in() {
  let server = FakeServer::new();
  let mut importer = Peer::enroll(&server, "Import").await;
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let participants = vec![alice.client.user_id(), bob.client.user_id()];

  let response = importer
    .client
    .open_import_dm(participants.clone())
    .await
    .unwrap();
  let dm = response.id;
  assert!(
    importer
      .client
      .set_up_import_dm(response, &participants)
      .await
      .unwrap()
  );
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer
    .client
    .send_imported(
      dm,
      &[draft(
        "7",
        "hi from the old chat",
        ImportedAuthor::User {
          user_id: bob.client.user_id(),
        },
        OLD_MS,
      )],
    )
    .await
    .unwrap();
  for peer in [&mut alice, &mut bob] {
    peer.client.full_sync().await.unwrap();
    peer.client.catch_up_messages(dm).await.unwrap();
    assert_eq!(peer.texts(dm), vec!["hi from the old chat"], "{}", peer.name);
  }

  importer.client.leave_after_import(dm).await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  alice.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  importer.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  let mut in_group = alice.client.group_users(dm).unwrap();
  in_group.sort();
  let mut expected = participants.clone();
  expected.sort();
  assert_eq!(in_group, expected, "only the two people are left in the group");
  // They still talk, and nobody wrote a goodbye.
  alice.client.send_text(dm, None, "still here").await.unwrap();
  settle(&mut [&mut importer, &mut alice, &mut bob]).await;
  assert_eq!(bob.texts(dm), vec!["hi from the old chat", "still here"]);
}

fn draft(source_id: &str, body: &str, author: ImportedAuthor, sent_at_ms: i64) -> ImportDraft {
  ImportDraft {
    content: MessageContent::Text {
      body: body.into(),
      mentions: Vec::new(),
      meta: MessageMeta {
        maybe_imported: Some(Box::new(ImportedOrigin {
          source: ImportSource::Synology,
          source_id: source_id.into(),
          author,
          sent_at_ms,
          maybe_edited_at_ms: None,
          reactions: Vec::new(),
          missing_files: Vec::new(),
        })),
        ..MessageMeta::default()
      },
    },
    maybe_thread_root: None,
  }
}

fn origin_of(peer: &Peer, conversation: ConversationId, body: &str) -> ImportedOrigin {
  peer
    .store
    .messages(conversation, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .find_map(|message| match message.content {
      MessageContent::Text { body: text, meta, .. } if text == body => meta.maybe_imported.map(|origin| *origin),
      _ => None,
    })
    .unwrap()
}
