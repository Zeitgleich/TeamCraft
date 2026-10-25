//! A new device of a user who is already a member (#161): it reads at once, joins the groups
//! without anybody else online where that is safe, and never announces a join.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, linked_options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::is_waiting_for_device;
use teamcraft_core::store::{
  ConversationEvent, LocalStore, MemoryStore, MessageContent, MessageFilter, Table,
};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{GroupAccess, LinkOffer, PendingEnrollment};
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ConversationId, HistoryPolicy, Id, ListMlsMessagesQuery,
  MlsMessageKind, PostMlsMessageRequest, WorkspacePolicy,
};

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

/// Issue #187: a member handles a device's request to be added once per `rejoin_retry`; repeats
/// (a device asking too often, a server replaying entries) cost it no requests.
#[tokio::test]
async fn a_repeated_device_add_request_costs_no_requests() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  server.forget_group_infos();
  let mut laptop = link_laptop(&mut phone).await;
  laptop.client.full_sync().await.unwrap();
  bob.client.sync_mls(channel).await.unwrap();
  assert!(bob.api.mls_group_info(channel).await.is_ok());
  let bob_id = bob.client.user_id();

  // What reading the queue costs when nothing new is in it.
  let before = server.rest_call_count(bob_id);
  bob.client.sync_mls(channel).await.unwrap();
  let idle_cost = server.rest_call_count(bob_id) - before;

  server.forget_group_infos();
  for _ in 0..3 {
    post_device_add_request(&laptop, channel).await;
  }
  let before = server.rest_call_count(bob_id);
  bob.client.sync_mls(channel).await.unwrap();
  assert_eq!(server.rest_call_count(bob_id) - before, idle_cost);
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

/// Issue #186: leaving a conversation drops none of its removal records. A re-join may share the
/// history again, and a removed member's messages from its time out must still be refused.
#[tokio::test]
async fn removal_records_survive_leaving_and_rejoining_a_conversation() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = phone.space().await;
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  for user in [bob.client.user_id(), carol.client.user_id()] {
    phone.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  let key = format!(
    "{channel}/{}/{}",
    carol.client.user_id(),
    carol.client.device_id()
  );
  let absence = phone.client.account_data().removed_senders[&key];

  server.remove_member_as_admin(channel, phone.client.user_id());
  phone.client.full_sync().await.unwrap();
  bob
    .client
    .add_member(channel, phone.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.full_sync().await.unwrap();
  let data = phone.client.account_data();
  assert_eq!(data.removed_senders.get(&key), Some(&absence));
  assert!(data.pruned_senders.is_empty());
  assert!(phone.store.get(Table::RemovedSenders, &key).unwrap().is_some());
}

/// Issue #186: an absence that ended before anything the workspace's retention limit keeps can't
/// matter any more; a device that is still out keeps its record. Time counts by the server.
#[tokio::test]
async fn removal_records_that_ended_before_the_retention_limit_are_dropped() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let alice_id = phone.client.user_id();
  phone
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  let space = phone.space().await;
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  for user in [
    bob.client.user_id(),
    carol.client.user_id(),
    dave.client.user_id(),
  ] {
    phone.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  // Carol leaves and comes back; three days later (server time) Dave leaves for good.
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  phone
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  server.advance_clock(std::time::Duration::from_secs(3 * 24 * 60 * 60));
  phone
    .client
    .remove_member(channel, dave.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  let key_of = |peer: &Peer| format!("{channel}/{}/{}", peer.client.user_id(), peer.client.device_id());
  let (carol_key, dave_key) = (key_of(&carol), key_of(&dave));
  let data = phone.client.account_data();
  assert!(data.removed_senders[&carol_key].maybe_back_at_ms.is_some());
  assert!(data.removed_senders.contains_key(&dave_key));

  // Pinned messages survive retention: nothing can go yet.
  let mut policy = WorkspacePolicy::default();
  policy.retention.limit.maybe_max_age_days = Some(1);
  policy.retention.keep_pinned = true;
  phone.client.publish_policy(policy.clone()).await.unwrap();
  phone.client.full_sync().await.unwrap();
  assert!(
    phone
      .client
      .account_data()
      .removed_senders
      .contains_key(&carol_key)
  );

  // One day of retention and one of leeway: Carol's return three days ago is past both.
  policy.retention.keep_pinned = false;
  phone.client.publish_policy(policy).await.unwrap();
  phone.client.full_sync().await.unwrap();
  let data = phone.client.account_data();
  assert!(
    !data.removed_senders.contains_key(&carol_key),
    "ended before the horizon"
  );
  assert!(data.removed_senders.contains_key(&dave_key), "still out");
}

#[tokio::test]
async fn messages_after_a_re_add_stay_visible_however_many_commits_follow() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let carol_id = carol.client.user_id();
  phone.client.add_member(channel, carol_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone.client.remove_member(channel, carol_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  server.advance_clock(std::time::Duration::from_secs(10 * 60));
  phone.client.add_member(channel, carol_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  server.advance_clock(std::time::Duration::from_secs(10 * 60));
  carol.client.send_text(channel, None, "back again").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  // More commits after her message: they must not move the time she came back.
  server.advance_clock(std::time::Duration::from_secs(10 * 60));
  let dave_id = dave.client.user_id();
  phone.client.add_member(channel, dave_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  phone.client.remove_member(channel, dave_id).await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol, &mut dave]).await;
  phone.client.full_sync().await.unwrap();

  assert!(phone.texts(channel).contains(&"back again".to_string()));
  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  let texts = laptop.texts(channel);
  assert!(texts.contains(&"back again".to_string()), "{texts:?}");
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

/// Issue #178: the key bag's current key may predate a key switch, so a device that joined on
/// its own writes only once a member handed it the current key; it shows that it waits.
#[tokio::test]
async fn a_device_that_joined_on_its_own_writes_once_a_member_handed_it_the_current_key() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  let mut laptop = link_laptop(&mut phone).await;
  server.set_offline(bob.client.user_id(), true);

  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(channel).unwrap(), GroupAccess::Member);
  assert!(is_waiting_for_device(laptop.store.as_ref(), channel).unwrap());
  laptop
    .client
    .send_text(channel, None, "from the laptop")
    .await
    .unwrap();
  assert_eq!(laptop.client.flush_outbox().await.unwrap().pending, 1);
  // The phone's key bag arrives meanwhile: its current key doesn't end the wait.
  phone.client.upload_key_bag().await.unwrap();
  laptop.client.full_sync().await.unwrap();
  assert!(is_waiting_for_device(laptop.store.as_ref(), channel).unwrap());
  assert_eq!(laptop.client.flush_outbox().await.unwrap().pending, 1);

  server.set_offline(bob.client.user_id(), false);
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert!(!is_waiting_for_device(laptop.store.as_ref(), channel).unwrap());
  // The runner flushes the outbox on its next round.
  assert_eq!(laptop.client.flush_outbox().await.unwrap().sent, 1);
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(bob.texts(channel), ["one", "two", "three", "from the laptop"]);
  assert_eq!(phone.texts(channel), ["one", "two", "three", "from the laptop"]);
}

/// Issue #178 for a device added from a welcome: its key bag, served by the server on recovery,
/// still names the key from before Carol's removal as current. It writes only with the key the
/// members switched to, which it gets from a member that had it in the group.
#[tokio::test]
async fn a_further_device_with_a_stale_key_bag_writes_with_the_current_key_only() {
  let server = FakeServer::new();
  let no_stagger = teamcraft_core::ClientSettings {
    device_add_stagger: std::time::Duration::ZERO,
    ..settings()
  };
  let mut phone = Peer::enroll_with_settings(&server, "Alice", no_stagger).await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let space = phone.space().await;
  let channel = phone.client.create_channel(space, "general").await.unwrap();
  for user in [bob.client.user_id(), carol.client.user_id()] {
    phone.client.add_member(channel, user).await.unwrap();
  }
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  phone.client.send_text(channel, None, "with Carol").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  // The key bag on the server is from before the removal and stays so.
  phone.client.upload_key_bag().await.unwrap();
  phone
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  bob
    .client
    .send_text(channel, None, "without Carol")
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;

  // Recovered with the passphrase: the old key bag; a member adds it from a welcome.
  let mut laptop = Peer::enroll_as(phone.api.clone(), "Alice laptop").await;
  laptop.client.full_sync().await.unwrap();
  phone.client.full_sync().await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.client.group_access(channel).unwrap(), GroupAccess::Member);
  phone.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;

  laptop
    .client
    .send_text(channel, None, "from the laptop")
    .await
    .unwrap();
  laptop.client.flush_outbox().await.unwrap();
  bob
    .client
    .send_text(channel, None, "after the laptop")
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  let laptop_key = laptop.content_key_of(channel, "from the laptop").await;
  assert_ne!(
    laptop_key,
    phone.content_key_of(channel, "with Carol").await,
    "not the key Carol knows"
  );
  assert_eq!(
    laptop_key,
    bob.content_key_of(channel, "after the laptop").await,
    "the key the members use"
  );
  assert!(!carol.client.has_content_key(channel, &laptop_key));
  assert!(bob.texts(channel).contains(&"from the laptop".to_string()));
}

/// A DM from before key epochs were recorded: nobody's current key is known to come from the
/// group. A member doesn't hand over that key but a fresh one, so a device linked now writes.
#[tokio::test]
async fn a_device_linked_after_the_upgrade_writes_in_a_dm_from_before_it() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dm = phone.client.open_dm(bob.client.user_id()).await.unwrap();
  phone.client.send_text(dm, None, "hi Bob").await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  forget_key_epochs(&phone, dm);
  forget_key_epochs(&bob, dm);
  phone.client.upload_key_bag().await.unwrap();
  let mut laptop = link_laptop(&mut phone).await;

  laptop.client.full_sync().await.unwrap();
  assert!(is_waiting_for_device(laptop.store.as_ref(), dm).unwrap());
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  phone.client.apply_pending_changes().await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert!(!is_waiting_for_device(laptop.store.as_ref(), dm).unwrap());

  laptop
    .client
    .send_text(dm, None, "from the laptop")
    .await
    .unwrap();
  laptop.client.flush_outbox().await.unwrap();
  bob.client.send_text(dm, None, "hi laptop").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(bob.texts(dm), ["hi Bob", "from the laptop", "hi laptop"]);
  assert_eq!(laptop.texts(dm), ["hi Bob", "from the laptop", "hi laptop"]);
  let laptop_key = laptop.content_key_of(dm, "from the laptop").await;
  assert_ne!(
    laptop_key,
    phone.content_key_of(dm, "hi Bob").await,
    "the key bag's key is not handed over"
  );
}

/// A member's hand-over fails on the way and another member's arrives first: the member never
/// switched to the key it made, so nobody writes with a key the others don't have.
#[tokio::test]
async fn a_failed_hand_over_never_leaves_a_member_writing_with_a_key_nobody_has() {
  let server = FakeServer::new();
  let (mut phone, mut bob, dm, mut laptop) = pre_upgrade_dm_with_new_laptop(&server).await;
  server.cut_next_application(phone.client.user_id(), NetworkCut::BeforeRequest);
  phone.client.sync_mls(dm).await.unwrap();
  assert!(phone.client.apply_pending_changes().await.is_err());
  bob.client.sync_mls(dm).await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  phone.client.apply_pending_changes().await.unwrap();

  assert_everybody_reads_everybody(dm, &mut phone, &mut bob, &mut laptop).await;
}

/// Two members hand over a fresh key at once, each before seeing the other's: the device takes
/// the first, each member keeps its own, and everybody reads everybody.
#[tokio::test]
async fn two_members_handing_over_at_once_keep_everybody_reading() {
  let server = FakeServer::new();
  let (mut phone, mut bob, dm, mut laptop) = pre_upgrade_dm_with_new_laptop(&server).await;
  server.hold_next_application(phone.client.user_id());
  phone.client.sync_mls(dm).await.unwrap();
  phone.client.apply_pending_changes().await.unwrap();
  bob.client.sync_mls(dm).await.unwrap();
  bob.client.apply_pending_changes().await.unwrap();
  server.release_held_messages();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert!(!is_waiting_for_device(laptop.store.as_ref(), dm).unwrap());

  assert_everybody_reads_everybody(dm, &mut phone, &mut bob, &mut laptop).await;
  assert_ne!(
    phone.content_key_of(dm, "from Alice's phone").await,
    bob.content_key_of(dm, "from Bob").await,
    "two keys in use, both known to everybody"
  );
}

/// A member whose current key only its key bag knows adds somebody with full history: the new
/// member gets a fresh current key, not that one.
#[tokio::test]
async fn a_full_history_add_hands_a_fresh_key_when_the_current_one_is_unknown_to_the_group() {
  let server = FakeServer::new();
  let (mut phone, mut bob, channel) = channel_with_history(&server).await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  forget_key_epochs(&phone, channel);
  phone
    .client
    .add_member_with_history(channel, carol.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  carol.client.full_sync().await.unwrap();
  assert_eq!(carol.texts(channel), ["one", "two", "three"]);

  carol.client.send_text(channel, None, "from Carol").await.unwrap();
  carol.client.flush_outbox().await.unwrap();
  phone.client.send_text(channel, None, "from Alice").await.unwrap();
  settle(&mut [&mut phone, &mut bob, &mut carol]).await;
  let carol_key = carol.content_key_of(channel, "from Carol").await;
  assert_ne!(carol_key, phone.content_key_of(channel, "one").await);
  assert_eq!(carol_key, phone.content_key_of(channel, "from Alice").await);
  assert!(bob.texts(channel).contains(&"from Carol".to_string()));
  assert!(carol.texts(channel).contains(&"from Alice".to_string()));
}

/// Issue #179: a further device joins the user's DMs on its own, like channels.
#[tokio::test]
async fn a_linked_device_joins_a_dm_on_its_own() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let dm = phone.client.open_dm(bob.client.user_id()).await.unwrap();
  phone.client.send_text(dm, None, "hi Bob").await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  phone.client.upload_key_bag().await.unwrap();
  let mut laptop = link_laptop(&mut phone).await;
  server.set_offline(bob.client.user_id(), true);

  laptop.client.full_sync().await.unwrap();
  assert_eq!(laptop.client.group_access(dm).unwrap(), GroupAccess::Member);
  assert_eq!(laptop.texts(dm), ["hi Bob"]);

  server.set_offline(bob.client.user_id(), false);
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  bob.client.send_text(dm, None, "hi laptop").await.unwrap();
  laptop
    .client
    .send_text(dm, None, "hi from the laptop")
    .await
    .unwrap();
  settle(&mut [&mut phone, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.texts(dm), ["hi Bob", "hi laptop", "hi from the laptop"]);
  assert_eq!(bob.texts(dm), ["hi Bob", "hi laptop", "hi from the laptop"]);
}

/// A DM whose group doesn't hold the other participant yet (Bob has no device) may still lose a
/// founding race: a further device doesn't join it by external commit.
#[tokio::test]
async fn a_linked_device_does_not_join_a_dm_group_that_lacks_a_participant() {
  let server = FakeServer::new();
  let mut phone = Peer::enroll(&server, "Alice").await;
  let bob_id = server.add_user("Bob").user_id();
  let dm = phone.client.open_dm(bob_id).await.unwrap();
  let mut laptop = link_laptop(&mut phone).await;

  laptop.client.full_sync().await.unwrap();
  assert_eq!(
    laptop.client.group_access(dm).unwrap(),
    GroupAccess::WaitingForDevice
  );
}

/// A DM of Alice's phone and Bob from before key epochs were recorded (both hand over at once),
/// and Alice's newly linked laptop, which joined it and waits for a current key.
async fn pre_upgrade_dm_with_new_laptop(server: &FakeServer) -> (Peer, Peer, ConversationId, Peer) {
  let no_stagger = teamcraft_core::ClientSettings {
    device_add_stagger: std::time::Duration::ZERO,
    ..settings()
  };
  let mut phone = Peer::enroll_with_settings(server, "Alice", no_stagger.clone()).await;
  let mut bob = Peer::enroll_with_settings(server, "Bob", no_stagger).await;
  let dm = phone.client.open_dm(bob.client.user_id()).await.unwrap();
  phone.client.send_text(dm, None, "hi Bob").await.unwrap();
  settle(&mut [&mut phone, &mut bob]).await;
  forget_key_epochs(&phone, dm);
  forget_key_epochs(&bob, dm);
  phone.client.upload_key_bag().await.unwrap();
  let mut laptop = link_laptop(&mut phone).await;
  laptop.client.full_sync().await.unwrap();
  assert!(is_waiting_for_device(laptop.store.as_ref(), dm).unwrap());
  (phone, bob, dm, laptop)
}

/// As on a device from before key epochs were recorded.
fn forget_key_epochs(peer: &Peer, conversation: ConversationId) {
  peer
    .store
    .delete(Table::AppliedChanges, &format!("key_history/{conversation}"))
    .unwrap();
}

async fn assert_everybody_reads_everybody(
  dm: ConversationId,
  phone: &mut Peer,
  bob: &mut Peer,
  laptop: &mut Peer,
) {
  phone
    .client
    .send_text(dm, None, "from Alice's phone")
    .await
    .unwrap();
  bob.client.send_text(dm, None, "from Bob").await.unwrap();
  laptop
    .client
    .send_text(dm, None, "from the laptop")
    .await
    .unwrap();
  laptop.client.flush_outbox().await.unwrap();
  settle(&mut [&mut *phone, &mut *bob, &mut *laptop]).await;
  let expected = ["hi Bob", "from Alice's phone", "from Bob", "from the laptop"];
  for peer in [&*phone, &*bob, &*laptop] {
    let texts = peer.texts(dm);
    for text in expected {
      assert!(texts.contains(&text.to_string()), "{}: {texts:?}", peer.name);
    }
  }
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

/// Posts the device's request to be added straight to the queue, past its client's own limit.
async fn post_device_add_request(device: &Peer, channel: ConversationId) {
  let query = ListMlsMessagesQuery {
    maybe_after: None,
    maybe_limit: Some(1),
  };
  let epoch = device
    .api
    .list_mls_messages(channel, &query)
    .await
    .unwrap()
    .current_epoch;
  let mut payload = b"TCDA".to_vec();
  payload.extend_from_slice(&device.client.user_id().snowflake().get().to_be_bytes());
  let request = PostMlsMessageRequest {
    device_id: device.client.device_id(),
    epoch,
    kind: MlsMessageKind::Proposal,
    payload: Base64Bytes::new(payload),
  };
  device.api.post_mls_message(channel, &request).await.unwrap();
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
