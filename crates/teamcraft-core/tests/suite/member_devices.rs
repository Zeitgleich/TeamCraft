//! A new device of a user who is already a member (#161): it reads at once, joins the groups
//! without anybody else online where that is safe, and never announces a join.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, linked_options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{ConversationEvent, LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{GroupAccess, LinkOffer, PendingEnrollment};
use teamcraft_proto::{AddConversationMemberRequest, ConversationId, ListMlsMessagesQuery};

#[tokio::test]
async fn a_linked_device_joins_on_its_own_and_reads_while_everybody_else_is_offline() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  // The phone approves the link, then is switched off (it does nothing until settled again).
  let mut laptop = link_laptop(&mut phone).await;
  server.set_offline(bob.client.user_id(), true);

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
async fn a_recovered_device_reads_history_and_waits_for_a_member() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  // Recovered with the passphrase: the key bag came from the server, so its epoch floors may be
  // old; the device doesn't join from group information on its own. The phone is switched off.
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
async fn without_group_information_a_linked_device_waits_and_a_member_publishes_it() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  // The last commit came from a client that published no group information.
  server.forget_group_infos();
  let mut laptop = link_laptop(&mut phone).await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.texts(channel), ["one", "two", "three"]);
  assert_eq!(
    laptop.client.group_access(channel).unwrap(),
    GroupAccess::WaitingForDevice
  );
  // Bob sees the request and publishes the group information (and queues the addition).
  bob.client.sync_mls(channel).await.unwrap();
  assert!(bob.api.mls_group_info(channel).await.is_ok());
}

#[tokio::test]
async fn group_information_from_before_a_removal_is_refused_whatever_epoch_the_server_claims() {
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
  let mut with_carol = phone.api.mls_group_info(channel).await.unwrap();
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone.client.sync_account_data().await.unwrap();
  let current = phone.client.mls_epoch(channel).unwrap().unwrap();

  // The server hands the laptop the old group information and claims it is current.
  with_carol.epoch = current;
  server.serve_group_info(channel, with_carol);
  let mut laptop = link_laptop(&mut phone).await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(
    laptop.client.group_access(channel).unwrap(),
    GroupAccess::WaitingForDevice
  );
}

#[tokio::test]
async fn a_new_device_refuses_what_a_removed_member_signs_after_the_removal() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = phone.space().await;
  // Carol stays in a channel made first (the new device learns her key there before it gets to
  // the channel she was removed from).
  let lobby = phone.client.create_channel(space, "lobby").await.unwrap();
  phone
    .client
    .add_member(lobby, carol.client.user_id())
    .await
    .unwrap();
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  phone
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut carol]).await;
  carol.client.send_text(lobby, None, "hello lobby").await.unwrap();
  carol.client.send_text(channel, None, "before").await.unwrap();
  settle(&mut [&mut phone, &mut carol]).await;
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut carol]).await;
  // The phone recorded the removal as an older version: locally only. Starting again shares it.
  phone
    .client
    .update_account_data(|data, _| {
      data.removed_senders.clear();
      true
    })
    .unwrap();
  phone.client.full_sync().await.unwrap();

  // Later the server lists Carol as a member again and lets her post; she still has the key from
  // before her removal. No member signed her back in.
  server.advance_clock(std::time::Duration::from_secs(10 * 60));
  phone
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
  carol.client.send_text(channel, None, "forged").await.unwrap();
  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.texts(lobby), ["hello lobby"]);
  let texts = laptop.texts(channel);
  assert!(texts.contains(&"before".to_string()), "{texts:?}");
  assert!(!texts.contains(&"forged".to_string()), "{texts:?}");

  // Live too: another forged message over the gateway.
  carol
    .client
    .send_text(channel, None, "forged again")
    .await
    .unwrap();
  settle(&mut [&mut laptop]).await;
  assert!(!laptop.texts(channel).contains(&"forged again".to_string()));
}

#[test]
fn a_device_added_back_counts_again_but_not_for_its_time_away() {
  let absence = teamcraft_core::account_data::SenderAbsence {
    left_at_ms: 1_000,
    maybe_back_at_ms: Some(10_000),
  };
  assert!(!absence.covers(900, 0), "before it left");
  assert!(absence.covers(5_000, 0), "while it was out");
  assert!(!absence.covers(11_000, 0), "after it came back");
  let left_again = teamcraft_core::account_data::SenderAbsence {
    left_at_ms: 20_000,
    ..absence
  };
  assert!(left_again.covers(30_000, 0), "out again since 20 000");
}

#[tokio::test]
async fn a_user_who_is_not_in_the_group_still_joins_with_a_signed_self_add() {
  let server = FakeServer::new();
  let (mut alice, mut bob, channel) = channel_with_history(&server).await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  // The server puts Carol in (an invitation); her user has no device in the group.
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
  // Her device asks with a signed self-add, not as a further device; the members decide by the
  // signed policy whether to commit it.
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

/// Links a laptop of the phone's user (the phone approves) and connects it.
async fn link_laptop(phone: &mut Peer) -> Peer {
  let api = phone.api.clone();
  let offer = LinkOffer::generate();
  let code = offer.to_code();
  let (new_side, phone_side) = tokio::join!(
    teamcraft_core::client::NewDeviceLink::connect(&api, &offer),
    phone.client.start_link_approval(&code)
  );
  let (approved, secrets) = tokio::join!(
    phone.client.complete_link_approval(phone_side.unwrap()),
    new_side.unwrap().receive_secrets()
  );
  approved.unwrap();
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      common::clock(),
      settings(),
      linked_options(secrets.unwrap()),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: "Alice laptop".into(),
    api,
    store,
    client,
    gateway,
  }
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
