//! A new device of a user who is already a member (#161): it reads at once, joins the groups
//! without anybody else online where the group information allows it, and never announces a join.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::GroupAccess;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{ConversationEvent, LocalStore, MessageContent, MessageFilter};
use teamcraft_proto::{AddConversationMemberRequest, ConversationId, ListMlsMessagesQuery};

#[tokio::test]
async fn a_new_device_joins_on_its_own_and_reads_while_everybody_else_is_offline() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  // Alice's phone is switched off (it does nothing until settled again); Bob is offline.
  server.set_offline(bob.client.user_id(), true);

  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(channel).unwrap(), GroupAccess::Member);
  assert_eq!(laptop.texts(channel), ["one", "two", "three"]);
  assert_eq!(laptop.client.undecrypted_count().unwrap(), 0);
  assert!(!has_join_event(&laptop, channel));

  // The others merge the laptop's commit when they come back; nobody sees "joined".
  server.set_offline(bob.client.user_id(), false);
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert!(
    bob
      .client
      .group_device_ids(channel)
      .unwrap()
      .contains(&laptop.client.device_id())
  );
  assert_eq!(
    bob.client.mls_epoch(channel).unwrap(),
    laptop.client.mls_epoch(channel).unwrap()
  );
  assert!(!has_join_event(&bob, channel));
  bob.client.send_text(channel, None, "hi laptop").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.texts(channel), ["one", "two", "three", "hi laptop"]);
}

#[tokio::test]
async fn without_group_information_the_device_reads_history_waits_and_is_added_later() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  // The last commit came from a client that published no group information.
  server.forget_group_infos();
  // Alice's phone is switched off until it is settled again.

  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  // The key bag's keys open the history; the senders are checked against their device lists.
  assert_eq!(laptop.texts(channel), ["one", "two", "three"]);
  assert_eq!(
    laptop.client.group_access(channel).unwrap(),
    GroupAccess::WaitingForDevice
  );
  assert!(!has_join_event(&laptop, channel));

  // The phone comes back, finds the request in the group's queue and adds the laptop.
  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.client.group_access(channel).unwrap(), GroupAccess::Member);
  assert!(!has_join_event(&bob, channel));
  bob.client.send_text(channel, None, "hi laptop").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.texts(channel), ["one", "two", "three", "hi laptop"]);
}

#[tokio::test]
async fn old_group_information_from_before_a_removal_is_refused() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  phone
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  // The group information while Carol was still in the group.
  let with_carol = phone.api.mls_group_info(channel).await.unwrap();
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone.client.sync_account_data().await.unwrap();

  // The server hands the laptop the old group information to fork it into a group with Carol.
  server.serve_group_info(channel, with_carol);
  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(
    laptop.client.group_access(channel).unwrap(),
    GroupAccess::WaitingForDevice
  );
}

#[tokio::test]
async fn a_user_who_is_not_in_the_group_still_joins_with_a_signed_self_add() {
  let server = FakeServer::new();
  let (mut alice, mut bob, channel) = channel_with_history(&server).await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  // The server puts Carol in (an invitation); the group information shows no device of hers.
  alice
    .api
    .add_member(
      channel,
      &AddConversationMemberRequest {
        user_id: carol.client.user_id(),
        maybe_role: None,
      },
    )
    .await
    .unwrap();
  carol.client.full_sync().await.unwrap();
  // Her device asks with a signed self-add (her user has no device in the group), not as a further
  // device; the members decide by the signed policy whether to commit it.
  let queue = alice
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap();
  let from_carol: Vec<&[u8]> = queue
    .messages
    .iter()
    .filter(|entry| entry.sender_device_id == carol.client.device_id())
    .map(|entry| entry.payload.as_slice())
    .collect();
  assert!(from_carol.iter().any(|payload| payload.starts_with(b"TCAC")));
  assert!(!from_carol.iter().any(|payload| payload.starts_with(b"TCDA")));
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
}

/// Alice's phone and Bob share a channel with three messages; Alice uploaded her key bag.
async fn channel_with_history(server: &FakeServer) -> (Peer, Peer, ConversationId) {
  let mut phone = Peer::enroll(server, "Alice").await;
  let mut bob = Peer::enroll(server, "Bob").await;
  let space = phone.space().await;
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  phone
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  for text in ["one", "two"] {
    phone.client.send_text(channel, None, text).await.unwrap();
  }
  bob.client.send_text(channel, None, "three").await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.upload_key_bag().await.unwrap();
  (phone, bob, channel)
}

fn has_join_event(peer: &Peer, channel: ConversationId) -> bool {
  peer
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .iter()
    .any(|message| {
      matches!(
        message.content,
        MessageContent::Event {
          event: ConversationEvent::Joined
        }
      )
    })
}
