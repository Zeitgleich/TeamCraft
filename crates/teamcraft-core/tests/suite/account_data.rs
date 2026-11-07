//! Saved items, drafts and saved searches follow the user across devices inside the encrypted
//! key bag (e2ee.md §6.4).
#![cfg(feature = "ui")]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device, settle};
use teamcraft_client_api::backend::{ChatBackend, SearchBackend};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MessageFilter};
use teamcraft_core::ui_backend::{CoreChatBackend, CoreCommand};

mod drafts_and_sent {
  use std::sync::Arc;

  use teamcraft_client_api::model::{ConversationId, Id, ThreadId};
  use teamcraft_core::records::{self, Draft};
  use teamcraft_core::store::{MemoryStore, MessageContent};
  use teamcraft_proto::{CreateSpaceRequest, JoinPolicy};

  use super::*;

  #[tokio::test]
  async fn drafts_list_newest_first_with_their_place_and_clear_everywhere() {
    let server = FakeServer::new();
    let mut phone = Peer::enroll(&server, "Alice").await;
    let mut laptop = link_device(&mut phone).await;
    let space = phone.space().await;
    let first = phone.client.create_channel(space, "first").await.unwrap();
    let second = phone.client.create_channel(space, "second").await.unwrap();
    phone
      .client
      .send_text(second, None, "the thread's root")
      .await
      .unwrap();
    let root = phone.store.messages(second, MessageFilter::TopLevel).unwrap()[0].clone();
    let thread = ThreadId::from_snowflake(root.maybe_server_id.unwrap().snowflake());

    phone
      .client
      .run_ui_command(set_draft(first, None, "older"))
      .await
      .unwrap();
    phone
      .client
      .run_ui_command(set_draft(second, Some(thread), "newer"))
      .await
      .unwrap();
    phone.client.sync_account_data().await.unwrap();
    laptop.sync_account_data().await.unwrap();
    for channel in [first, second] {
      phone.client.add_missing_devices(channel).await.unwrap();
    }
    laptop.sync_welcomes().await.unwrap();
    laptop.load_older_messages(second).await.unwrap();

    let ui = CoreChatBackend::new(
      laptop.store().clone(),
      laptop.user_id(),
      Box::new(ManualClock::new(1_000)),
    );
    let drafts = ui.drafts(None);
    let places: Vec<_> = drafts
      .iter()
      .map(|item| (item.conversation, item.maybe_thread, item.body.as_str()))
      .collect();
    assert_eq!(places.len(), 2);
    assert!(places.contains(&(first, None, "older")));
    assert!(places.contains(&(second, Some(thread), "newer")));
    let in_thread = drafts.iter().find(|item| item.maybe_thread.is_some()).unwrap();
    assert_eq!(in_thread.conversation_name, "second");
    assert_eq!(in_thread.maybe_thread_root.as_deref(), Some("the thread's root"));
    assert!(
      drafts
        .windows(2)
        .all(|pair| pair[0].edited_at >= pair[1].edited_at)
    );

    // The conversation's entry carries its own draft only: the thread lists mark the other one.
    assert_eq!(
      ui.conversation(first).unwrap().maybe_draft_preview.as_deref(),
      Some("older")
    );
    assert_eq!(ui.conversation(second).unwrap().maybe_draft_preview, None);

    phone
      .client
      .run_ui_command(set_draft(first, None, ""))
      .await
      .unwrap();
    phone.client.sync_account_data().await.unwrap();
    laptop.sync_account_data().await.unwrap();
    assert_eq!(
      ui.drafts(None).len(),
      1,
      "a deleted draft is gone from the other device too"
    );
    assert_eq!(ui.conversation(first).unwrap().maybe_draft_preview, None);
  }

  #[tokio::test]
  async fn drafts_and_sent_can_be_scoped_to_a_space() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let space = alice.space().await;
    let other_space = alice
      .client
      .create_space(&CreateSpaceRequest {
        name: "Project A".into(),
        maybe_icon: None,
        maybe_color: None,
        maybe_description: None,
        join_policy: JoinPolicy::Open,
        maybe_idempotency_key: None,
      })
      .await
      .unwrap()
      .id;
    let here = alice.client.create_channel(space, "here").await.unwrap();
    let there = alice.client.create_channel(other_space, "there").await.unwrap();
    for channel in [here, there] {
      alice.client.send_text(channel, None, "sent").await.unwrap();
      alice
        .client
        .run_ui_command(set_draft(channel, None, "unsent"))
        .await
        .unwrap();
    }
    let ui = ui_of(&alice);
    assert_eq!(ui.drafts(None).len(), 2);
    assert_eq!(ui.sent_messages(None).len(), 2);
    let drafts: Vec<ConversationId> = ui
      .drafts(Some(space))
      .iter()
      .map(|item| item.conversation)
      .collect();
    assert_eq!(drafts, [here]);
    let sent: Vec<ConversationId> = ui
      .sent_messages(Some(space))
      .iter()
      .map(|item| item.conversation)
      .collect();
    assert_eq!(sent, [here]);
  }

  #[tokio::test]
  async fn sent_lists_own_messages_and_replies_newest_first_without_others_or_deleted() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "work")
      .await
      .unwrap();
    alice
      .client
      .send_text(channel, None, "deleted later")
      .await
      .unwrap();
    let gone = alice.store.messages(channel, MessageFilter::TopLevel).unwrap()[0].clone();
    alice.client.send_text(channel, None, "the root").await.unwrap();
    let root = alice.store.messages(channel, MessageFilter::TopLevel).unwrap()[1].clone();
    let thread = ThreadId::from_snowflake(root.maybe_server_id.unwrap().snowflake());
    alice
      .client
      .send_text(channel, Some(thread), "a reply")
      .await
      .unwrap();
    alice
      .client
      .delete_message(channel, gone.client_message_id)
      .await
      .unwrap();

    let sent = ui_of(&alice).sent_messages(None);
    let rows: Vec<(&str, Option<ThreadId>)> = sent
      .iter()
      .map(|item| (item.excerpt.as_str(), item.maybe_thread))
      .collect();
    assert_eq!(rows, [("a reply", Some(thread)), ("the root", None)]);
    assert!(sent.iter().all(|item| item.conversation_name == "work"));
    assert_eq!(Some(sent[1].message), root.maybe_server_id);
  }

  #[tokio::test]
  async fn drafts_and_sent_in_the_notes_are_marked_for_the_apps_to_name() {
    let server = FakeServer::new();
    let mut alice =
      Peer::enroll_with_settings(&server, "Alice", common::settings().with_notes_to_self()).await;
    let notes = alice.client.open_dm(alice.client.user_id()).await.unwrap();
    let channel = alice
      .client
      .create_channel(alice.space().await, "work")
      .await
      .unwrap();
    alice.client.send_text(notes, None, "buy milk").await.unwrap();
    alice.client.send_text(channel, None, "ship it").await.unwrap();
    for conversation in [notes, channel] {
      alice
        .client
        .run_ui_command(set_draft(conversation, None, "later"))
        .await
        .unwrap();
    }
    alice.client.sync_account_data().await.unwrap();

    let ui = ui_of(&alice);
    let drafts = ui.drafts(None);
    assert_eq!(drafts.len(), 2);
    let draft = |conversation| {
      drafts
        .iter()
        .find(|item| item.conversation == conversation)
        .unwrap()
    };
    assert!(draft(notes).is_notes_to_self);
    assert!(draft(notes).conversation_name.is_empty());
    assert!(!draft(channel).is_notes_to_self);
    assert_eq!(draft(channel).conversation_name, "work");

    let sent = ui.sent_messages(None);
    assert_eq!(sent.len(), 2);
    let item = |conversation| {
      sent
        .iter()
        .find(|item| item.conversation == conversation)
        .unwrap()
    };
    assert!(item(notes).is_notes_to_self);
    assert!(!item(channel).is_notes_to_self);
    assert_eq!(item(channel).conversation_name, "work");
  }

  #[tokio::test]
  async fn sent_keeps_to_its_limit_and_follows_a_new_message() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "busy")
      .await
      .unwrap();
    let ui = ui_of(&alice);
    for number in 0..205 {
      alice
        .client
        .send_text(channel, None, &format!("message {number}"))
        .await
        .unwrap();
    }
    let sent = ui.sent_messages(None);
    assert_eq!(sent.len(), 200, "recent messages, not the whole history");
    assert_eq!(sent[0].excerpt, "message 204");
    assert_eq!(sent[199].excerpt, "message 5");
    alice.client.send_text(channel, None, "one more").await.unwrap();
    assert_eq!(
      ui.sent_messages(None)[0].excerpt,
      "one more",
      "a change is read again"
    );
  }

  #[tokio::test]
  async fn a_message_still_in_the_outbox_is_not_sent_yet() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let channel = alice
      .client
      .create_channel(alice.space().await, "work")
      .await
      .unwrap();
    alice.client.send_text(channel, None, "accepted").await.unwrap();
    let ui = ui_of(&alice);
    records::enqueue_message(
      &alice.store,
      alice.client.user_id(),
      1,
      Draft {
        conversation_id: channel,
        maybe_thread_id: None,
        content: MessageContent::Text {
          body: "still waiting".into(),
          mentions: Vec::new(),
          meta: Default::default(),
        },
      },
    )
    .unwrap();
    let rows: Vec<String> = ui
      .sent_messages(None)
      .into_iter()
      .map(|item| item.excerpt)
      .collect();
    assert_eq!(rows, ["accepted"]);
  }

  fn set_draft(conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) -> CoreCommand {
    CoreCommand::SetDraft {
      conversation,
      maybe_thread,
      body: body.into(),
    }
  }

  fn ui_of(peer: &Peer) -> CoreChatBackend<Arc<MemoryStore>> {
    CoreChatBackend::new(
      peer.store.clone(),
      peer.client.user_id(),
      Box::new(ManualClock::new(1_000)),
    )
  }
}

#[tokio::test]
async fn saved_items_drafts_and_searches_reach_the_other_device() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "notes")
    .await
    .unwrap();
  phone
    .client
    .send_text(channel, None, "remember this")
    .await
    .unwrap();
  let message = phone.store.messages(channel, MessageFilter::TopLevel).unwrap()[0].clone();

  for command in [
    CoreCommand::SetSaved {
      conversation: channel,
      client_message_id: message.client_message_id,
      is_saved: true,
    },
    CoreCommand::SetDraft {
      conversation: channel,
      maybe_thread: None,
      body: "half a sentence".into(),
    },
    CoreCommand::SaveSearch {
      id: 7,
      name: "Mine".into(),
      query: "from:@me".into(),
    },
  ] {
    phone.client.run_ui_command(command).await.unwrap();
  }
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();

  let data = laptop.account_data();
  assert!(data.is_saved(channel, message.client_message_id));
  assert_eq!(data.draft(channel, None), Some("half a sentence"));
  assert_eq!(data.saved_searches()[0].1.name, "Mine");

  // The UI reads them from the laptop's store.
  phone.client.add_missing_devices(channel).await.unwrap();
  laptop.sync_welcomes().await.unwrap();
  laptop.load_older_messages(channel).await.unwrap();
  let backend = CoreChatBackend::new(
    laptop.store().clone(),
    laptop.user_id(),
    Box::new(ManualClock::new(1_800_000_000_000)),
  );
  assert_eq!(backend.saved_items()[0].message.body, "remember this");
  assert_eq!(backend.draft(channel, None).as_deref(), Some("half a sentence"));
  assert_eq!(backend.saved_searches()[0].name, "Mine");
  settle(&mut [&mut phone]).await;
}

#[tokio::test]
async fn concurrent_changes_on_two_devices_merge() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut laptop = link_device(&mut phone).await;
  let channel = phone
    .client
    .create_channel(phone.space().await, "drafts")
    .await
    .unwrap();

  // Both change different things before either syncs; the second upload hits a version conflict
  // and merges instead of overwriting.
  phone
    .client
    .run_ui_command(CoreCommand::SaveSearch {
      id: 1,
      name: "from phone".into(),
      query: "a".into(),
    })
    .await
    .unwrap();
  laptop
    .run_ui_command(CoreCommand::SetDraft {
      conversation: channel,
      maybe_thread: None,
      body: "from laptop".into(),
    })
    .await
    .unwrap();
  phone.client.sync_account_data().await.unwrap();
  laptop.sync_account_data().await.unwrap();
  phone.client.sync_account_data().await.unwrap();

  for data in [phone.client.account_data(), laptop.account_data()] {
    assert_eq!(data.saved_searches().len(), 1);
    assert_eq!(data.draft(channel, None), Some("from laptop"));
  }
  assert!(!phone.client.is_key_bag_dirty() && !laptop.is_key_bag_dirty());
}
