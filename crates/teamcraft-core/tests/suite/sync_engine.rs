//! The sync engine end to end through the in-process fake server: encrypted exchange between
//! clients, removal with key rotation, the offline outbox, gateway resume and identity recovery.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, clock, options, settings, settle};
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{Client, GatewayOutcome, PendingEnrollment};
use teamcraft_proto::{HistoryPolicy, Id, ThreadId};

#[tokio::test]
async fn members_exchange_encrypted_messages() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;

  let channel = alice
    .client
    .create_channel(alice.space().await, "project-a")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  alice
    .client
    .send_text(channel, None, "Release is on Friday")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  bob.client.send_text(channel, None, "Thanks!").await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  for peer in [&alice, &bob, &carol] {
    assert_eq!(
      peer.texts(channel),
      vec!["Release is on Friday", "Thanks!"],
      "{}",
      peer.name
    );
  }
  for ciphertext in server.stored_ciphertexts() {
    assert!(!ciphertext.windows(7).any(|window| window == b"Release"));
  }
  assert_eq!(bob.client.undecrypted_count().unwrap(), 0);
}

#[tokio::test]
async fn new_member_reads_shared_history() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;

  let channel = alice
    .client
    .create_channel(alice.space().await, "history")
    .await
    .unwrap();
  alice.client.send_text(channel, None, "before Bob").await.unwrap();
  alice
    .client
    .add_member_with_history(channel, bob.client.user_id(), Some(HistoryPolicy::Full))
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  bob.client.load_older_messages(channel).await.unwrap();
  assert_eq!(bob.texts(channel), vec!["before Bob"]);
}

#[tokio::test]
async fn removal_rotates_the_content_key() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "team")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice.client.send_text(channel, None, "everyone").await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  alice
    .client
    .remove_member(channel, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  alice
    .client
    .send_text(channel, None, "without Carol")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;

  assert_eq!(bob.texts(channel), vec!["everyone", "without Carol"]);
  assert_eq!(carol.texts(channel), vec!["everyone"]);
  assert!(!carol.client.has_group(channel).unwrap());
  // Even with the ciphertext in hand, Carol lacks the rotated key.
  let ciphertexts = server.stored_ciphertexts();
  // Two texts and three announcements (Bob and Carol added, Carol removed).
  assert_eq!(ciphertexts.len(), 5);
}

#[tokio::test]
async fn concurrent_commits_are_rebased() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "busy")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // Alice's commit lands first; Bob, still in the old epoch, hits the conflict and rebases.
  alice
    .client
    .add_member(channel, carol.client.user_id())
    .await
    .unwrap();
  bob
    .client
    .add_member(channel, dave.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  dave
    .client
    .send_text(channel, None, "hello from Dave")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  for peer in [&alice, &bob, &carol, &dave] {
    assert_eq!(peer.texts(channel), vec!["hello from Dave"], "{}", peer.name);
  }
}

#[tokio::test]
async fn outbox_survives_being_offline() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "offline")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  server.set_offline(bob.client.user_id(), true);
  bob
    .client
    .send_text(channel, None, "written on the train")
    .await
    .unwrap();
  let pending: Vec<_> = bob
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter(|message| matches!(message.content, MessageContent::Text { .. }))
    .collect();
  assert_eq!(pending.len(), 1);
  assert!(pending[0].maybe_server_id.is_none());
  assert_eq!(bob.client.flush_outbox().await.unwrap().pending, 1);

  server.set_offline(bob.client.user_id(), false);
  let report = bob.client.flush_outbox().await.unwrap();
  assert_eq!(report.sent, 1);
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice.texts(channel), vec!["written on the train"]);
  assert!(
    bob.store.messages(channel, MessageFilter::TopLevel).unwrap()[0]
      .maybe_server_id
      .is_some()
  );
}

#[tokio::test]
async fn a_welcome_that_arrives_during_a_network_error_is_joined_later() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "flaky")
    .await
    .unwrap();

  // The welcome arrives on the gateway, but REST fails while Bob joins.
  server.set_rest_failing(bob.client.user_id(), true);
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  // Every event fails on its REST call; the welcome among them is consumed.
  let mut failures = 0;
  while bob.client.pump(&mut bob.gateway).await.is_err() {
    failures += 1;
    assert!(failures < 10, "events keep failing");
  }
  assert!(failures > 0);

  // Once the network is back, the welcome is fetched again and joined.
  server.set_rest_failing(bob.client.user_id(), false);
  bob.client.sync_welcomes().await.unwrap();
  alice
    .client
    .send_text(channel, None, "welcome after all")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.texts(channel), vec!["welcome after all"]);
}

#[tokio::test]
async fn an_expired_token_keeps_queued_messages_pending() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "after sleep")
    .await
    .unwrap();

  // The laptop woke up and the access token expired before the app refreshed it.
  server.set_signed_out(alice.client.user_id(), true);
  let _ = alice.client.send_text(channel, None, "first").await;
  let _ = alice.client.send_text(channel, None, "second").await;
  let report = alice.client.flush_outbox().await.unwrap();
  assert_eq!((report.pending, report.failed), (2, 0));

  server.set_signed_out(alice.client.user_id(), false);
  assert_eq!(alice.client.flush_outbox().await.unwrap().sent, 2);
}

#[tokio::test]
async fn gateway_resumes_and_falls_back_to_full_sync() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "resume")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // Network drop: the session survives on the server and replays what was missed.
  server.drop_gateways(bob.client.user_id());
  alice
    .client
    .send_text(channel, None, "while you were away")
    .await
    .unwrap();
  let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
  assert_eq!(outcome, GatewayOutcome::Resumed);
  bob.gateway = gateway;
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.texts(channel), vec!["while you were away"]);

  // Server restart: the session is gone, the client refetches over REST.
  server.expire_sessions();
  server.drop_gateways(bob.client.user_id());
  alice
    .client
    .send_text(channel, None, "after the restart")
    .await
    .unwrap();
  let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
  assert_eq!(outcome, GatewayOutcome::Fresh);
  bob.gateway = gateway;
  assert_eq!(
    bob.texts(channel),
    vec!["while you were away", "after the restart"]
  );
}

#[tokio::test]
async fn a_full_sync_that_fails_is_repeated_on_the_next_connect() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "gap")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // A server restart, and the sync after the new session breaks off.
  server.expire_sessions();
  server.drop_gateways(bob.client.user_id());
  alice
    .client
    .send_text(channel, None, "missed during the restart")
    .await
    .unwrap();
  server.set_rest_failing(bob.client.user_id(), true);
  assert!(bob.client.connect_gateway().await.is_err());

  // The next connect must not resume the half-synced session: it syncs again.
  server.set_rest_failing(bob.client.user_id(), false);
  let (gateway, outcome) = bob.client.connect_gateway().await.unwrap();
  assert_eq!(outcome, GatewayOutcome::Fresh);
  bob.gateway = gateway;
  assert_eq!(bob.texts(channel), vec!["missed during the restart"]);
}

#[tokio::test]
async fn direct_messages_work_both_ways() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;

  let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(bob.client.open_dm(alice.client.user_id()).await.unwrap(), dm);
  bob.client.send_text(dm, None, "hi Alice").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice.texts(dm), vec!["hi Alice"]);
}

#[tokio::test]
async fn group_direct_messages_reach_everyone() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());

  // Duplicates and the user themself are ignored.
  let group = alice
    .client
    .open_group_dm(&[bob_id, carol_id, bob_id, alice.client.user_id()])
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert_eq!(
    carol
      .client
      .open_group_dm(&[alice.client.user_id(), bob_id])
      .await
      .unwrap(),
    group,
    "the same people find the same group"
  );
  carol.client.send_text(group, None, "hi both").await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol]).await;
  assert_eq!(alice.texts(group), vec!["hi both"]);
  assert_eq!(bob.texts(group), vec!["hi both"]);
}

#[tokio::test]
async fn reactions_and_thread_replies_are_encrypted_messages() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "threads")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  let root = alice.client.send_text(channel, None, "root").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  let stored_root = bob.store.message(channel, root).unwrap().unwrap();
  let thread = ThreadId::from_snowflake(stored_root.maybe_server_id.unwrap().snowflake());
  bob
    .client
    .send_text(channel, Some(thread), "reply")
    .await
    .unwrap();
  bob.client.react(channel, root, "+1", true).await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  assert_eq!(alice.texts(channel), vec!["root"]);
  let replies = alice
    .store
    .messages(channel, MessageFilter::Thread(thread))
    .unwrap();
  assert_eq!(replies.len(), 1);
  let reactions: Vec<_> = alice
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|m| m.is_reaction())
    .collect();
  assert!(
    matches!(&reactions[0].content, MessageContent::Reaction { target, emoji, is_added: true } if *target == root && emoji == "+1")
  );
}

#[tokio::test]
async fn second_device_recovers_identity_and_history() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "recovery")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  alice.client.send_text(channel, None, "old news").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  alice.client.upload_key_bag().await.unwrap();

  // Alice logs in on a laptop: same account, new device, recovered with the passphrase.
  let mut laptop = Peer::enroll_as(alice.api.clone(), "Alice laptop").await;
  assert_eq!(
    laptop.client.identity_fingerprint(),
    alice.client.identity_fingerprint()
  );
  // Her phone adds the laptop to the group; no admin signature needed for own devices.
  assert_eq!(alice.client.add_missing_devices(channel).await.unwrap(), 1);
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  assert!(laptop.client.has_group(channel).unwrap());

  laptop.client.load_older_messages(channel).await.unwrap();
  bob
    .client
    .send_text(channel, None, "welcome, laptop")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut laptop]).await;
  assert_eq!(laptop.texts(channel), vec!["old news", "welcome, laptop"]);
}

#[tokio::test]
async fn history_on_a_new_device_shows_edits_and_deletions() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "history")
    .await
    .unwrap();
  let draft = alice.client.send_text(channel, None, "draft").await.unwrap();
  alice.client.edit_message(channel, draft, "final").await.unwrap();
  alice.client.send_text(channel, None, "keep").await.unwrap();
  let oops = alice.client.send_text(channel, None, "oops").await.unwrap();
  alice.client.delete_message(channel, oops).await.unwrap();
  alice.client.upload_key_bag().await.unwrap();

  // History arrives newest first, so the edit and the deletion come before their targets.
  let mut laptop = Peer::enroll_as(alice.api.clone(), "Alice laptop").await;
  alice.client.add_missing_devices(channel).await.unwrap();
  settle(&mut [&mut alice, &mut laptop]).await;
  laptop.client.load_older_messages(channel).await.unwrap();
  assert_eq!(laptop.texts(channel), vec!["final", "keep"]);
}

#[tokio::test]
async fn wrong_passphrase_does_not_recover() {
  let server = FakeServer::new();
  let alice = Peer::enroll(&server, "Alice").await;
  let result = PendingEnrollment::new()
    .complete(
      alice.api.clone(),
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      options("wrong"),
    )
    .await;
  assert!(result.is_err());
}

#[tokio::test]
async fn client_reopens_from_its_store() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let channel = alice
    .client
    .create_channel(alice.space().await, "reopen")
    .await
    .unwrap();
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;

  // Bob's app restarts: the client is rebuilt from the store alone.
  let reopened = Client::open(bob.api.clone(), bob.store.clone(), clock(), settings())
    .unwrap()
    .unwrap();
  bob.client = reopened;
  let (gateway, _) = bob.client.connect_gateway().await.unwrap();
  bob.gateway = gateway;
  alice
    .client
    .send_text(channel, None, "still there?")
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  bob.client.send_text(channel, None, "yes").await.unwrap();
  settle(&mut [&mut alice, &mut bob]).await;
  assert_eq!(alice.texts(channel), vec!["still there?", "yes"]);
}
