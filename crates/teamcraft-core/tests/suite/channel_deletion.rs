//! Getting out of a channel and deleting it (issue #1130, e2ee.md §4.6): every role × state × last
//! admin / last member for leave and delete, leaving with no other device online, a device away
//! when the channel went, and what a deletion on the server's word alone may do to local history. Through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::CoreError;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_proto::{
  ChannelRole, ConversationId, ConversationVisibility, DeleteConversationRequest, ErrorCode, GatewayEvent,
  UpdateConversationMemberRequest, UpdateConversationRequest,
};

mod leaving {
  use super::*;

  #[tokio::test]
  async fn the_last_admin_of_an_active_channel_is_refused() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;

    let error = alice.client.leave_channel(channel, false).await.unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::LastChannelAdmin));
    assert!(alice.client.has_group(channel).unwrap());
  }

  /// The report behind #1130, and leaving with nobody else online: Bob's device doesn't run. The
  /// MLS secrets go with the answer, not with an event the server could withhold.
  #[tokio::test]
  async fn the_last_admin_of_an_archived_channel_leaves_with_no_other_device_online() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    archive(&mut alice, channel).await;

    alice.client.leave_channel(channel, false).await.unwrap();
    assert!(
      !alice.client.has_group(channel).unwrap(),
      "dropped without any event"
    );
    assert!(!is_cached(&alice, channel));

    // Bob comes back: the channel stays for him, and his device commits her signed leave.
    settle(&mut [&mut bob]).await;
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut bob]).await;
    assert!(is_cached(&bob, channel));
    assert!(
      !bob
        .client
        .group_users(channel)
        .unwrap()
        .contains(&alice.client.user_id())
    );
  }

  #[tokio::test]
  async fn a_member_leaves_either_state() {
    for is_archived in [false, true] {
      let server = FakeServer::new();
      let (mut alice, mut bob) = two(&server).await;
      let channel = channel_with(&mut alice, &mut bob).await;
      if is_archived {
        archive(&mut alice, channel).await;
        settle(&mut [&mut alice, &mut bob]).await;
      }
      bob.client.leave_channel(channel, false).await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      alice.client.apply_pending_changes().await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      bob.client.apply_pending_changes().await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      assert!(!is_cached(&bob, channel), "archived {is_archived}");
      assert!(is_cached(&alice, channel), "archived {is_archived}");
    }
  }

  /// A stale list can't delete by accident: without the flag the server refuses.
  #[tokio::test]
  async fn the_last_member_is_asked_before_the_channel_goes() {
    for is_archived in [false, true] {
      let server = FakeServer::new();
      let (mut alice, _) = two(&server).await;
      let channel = alone_in_channel(&mut alice).await;
      alice.client.send_text(channel, None, "hello").await.unwrap();
      if is_archived {
        archive(&mut alice, channel).await;
      }

      let error = alice.client.leave_channel(channel, false).await.unwrap_err();
      assert_eq!(
        api_code(&error),
        Some(ErrorCode::LastMember),
        "archived {is_archived}"
      );
      assert!(is_cached(&alice, channel));

      alice.client.leave_channel(channel, true).await.unwrap();
      settle(&mut [&mut alice]).await;
      assert!(
        alice.api.conversation(channel).await.is_err(),
        "archived {is_archived}"
      );
      assert_purged(&alice, channel);
    }
  }

  /// Anyone can join a public channel and leave it: that must not delete it.
  #[tokio::test]
  async fn a_public_channel_stays_without_members() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;
    alice
      .client
      .update_channel(
        channel,
        &UpdateConversationRequest {
          maybe_visibility: Some(ConversationVisibility::Public),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();

    alice.client.leave_channel(channel, false).await.unwrap();

    assert!(server.has_conversation(channel));
  }

  /// A deletion the user confirmed on another device is still the server's word here.
  #[tokio::test]
  async fn the_last_members_other_device_hides_it_on_its_next_sync() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;
    let mut phone = common::link_device(&mut alice).await;
    phone.full_sync().await.unwrap();
    assert!(
      phone
        .store()
        .get(Table::Conversations, &channel.to_string())
        .unwrap()
        .is_some()
    );

    alice.client.leave_channel(channel, true).await.unwrap();

    phone.full_sync().await.unwrap();
    assert!(
      phone
        .store()
        .get(Table::Conversations, &channel.to_string())
        .unwrap()
        .is_none()
    );
    assert!(!phone.has_group(channel).unwrap());
  }
}

mod deleting {
  use super::*;

  #[tokio::test]
  async fn a_channel_admin_deletes_with_members_in_it_either_state() {
    for is_archived in [false, true] {
      let server = FakeServer::new();
      let (mut alice, mut bob) = two(&server).await;
      let channel = channel_with(&mut alice, &mut bob).await;
      bob.client.send_text(channel, None, "hello").await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      if is_archived {
        archive(&mut alice, channel).await;
        settle(&mut [&mut alice, &mut bob]).await;
      }

      alice.client.delete_channel(channel).await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;

      // Alice confirmed it here. Her role goes back only to her founding claim: Bob keeps the history.
      assert_purged(&alice, channel);
      assert_hidden_with_history(&bob, channel);
    }
  }

  #[tokio::test]
  async fn members_and_moderators_may_not_delete() {
    for role in [ChannelRole::Member, ChannelRole::Moderator] {
      let server = FakeServer::new();
      let (mut alice, mut bob) = two(&server).await;
      let channel = channel_with(&mut alice, &mut bob).await;
      alice
        .api
        .update_member(
          channel,
          bob.client.user_id(),
          &UpdateConversationMemberRequest {
            maybe_role: Some(role.clone()),
            ..UpdateConversationMemberRequest::default()
          },
        )
        .await
        .unwrap();

      let error = bob.client.delete_channel(channel).await.unwrap_err();
      assert!(!error.is_transient(), "{role:?}: {error}");
      settle(&mut [&mut alice, &mut bob]).await;
      assert!(is_cached(&bob, channel), "{role:?}");
      assert!(is_cached(&alice, channel), "{role:?}");
    }
  }

  #[tokio::test]
  async fn a_workspace_admin_outside_the_channel_may_delete() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;
    server.set_admin(bob.client.user_id());

    bob.client.delete_channel(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    assert_hidden_with_history_or_empty(&alice, channel);
  }

  /// A lost answer, or a second admin: gone already counts as done.
  #[tokio::test]
  async fn deleting_again_finishes_instead_of_failing() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;

    alice.client.delete_channel(channel).await.unwrap();
    alice.client.delete_channel(channel).await.unwrap();
    assert_purged(&alice, channel);
  }

  /// Only "the conversation is gone" counts as done, not any 404: the local copy stays otherwise.
  #[tokio::test]
  async fn another_not_found_is_a_refusal_and_keeps_the_history() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    server.answer_next_delete_with(404, ErrorCode::UserNotFound);

    alice.client.delete_channel(channel).await.unwrap_err();

    assert!(is_cached(&alice, channel));
    assert!(
      !alice
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
  }

  /// A leave waiting for the members' commit has nothing left to do once the channel is gone.
  #[tokio::test]
  async fn a_pending_leave_goes_with_the_channel() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;

    bob.client.leave_channel(channel, false).await.unwrap();
    assert!(bob.client.has_group(channel).unwrap(), "waits for the commit");

    alice.client.delete_channel(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    bob.client.apply_pending_changes().await.unwrap();

    assert!(!is_cached(&bob, channel));
    assert!(pending_about(&bob, channel).is_empty());
  }

  #[tokio::test]
  async fn a_direct_message_is_refused() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let error = alice.client.delete_channel(dm).await.unwrap_err();
    assert_eq!(api_code(&error), Some(ErrorCode::NotAChannel));
    assert!(is_cached(&alice, dm));
  }
}

/// e2ee.md §4.6: a deletion the server claims hides the conversation; the history stays for a while.
mod server_claims {
  use super::*;

  // These are about claims: deletions without a signature (`signed_deletions.rs` has the signed
  // ones), as an older client's, or a space admin's whose authority isn't signed.

  /// Offline when it happened: the next full sync hides it, the messages stay.
  #[tokio::test]
  async fn a_device_that_was_offline_converges_and_keeps_the_history_for_now() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    delete_unsigned(&alice, channel).await;

    // Bob's device doesn't read its gateway (offline beyond the resume window): only the sync.
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);
    assert!(
      !bob.client.has_group(channel).unwrap(),
      "the MLS secrets go at once"
    );
  }

  #[tokio::test]
  async fn the_history_goes_when_the_retention_ran_out_and_a_replay_does_not_extend_or_shorten_it() {
    let server = FakeServer::new();
    let (clock, mut alice, mut bob) = on_clock(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    delete_unsigned(&alice, channel).await;
    bob.client.full_sync().await.unwrap();

    clock.advance(RETENTION / 2);
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);

    clock.advance(RETENTION / 2);
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
    // The tombstone stays listed for months: nothing comes back and nothing is left to do.
    bob.client.full_sync().await.unwrap();
    assert_purged(&bob, channel);
  }

  /// The server claims a conversation deleted that it lists all the same: forged or replayed.
  #[tokio::test]
  async fn a_claim_about_what_it_still_lists_changes_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    // By the gateway.
    server.publish_event(
      &[bob.client.user_id()],
      &GatewayEvent::ConversationDelete {
        conversation_id: channel,
        maybe_signed_deletion: None,
      },
    );
    settle(&mut [&mut bob]).await;
    assert!(is_cached(&bob, channel));
    assert!(bob.client.has_group(channel).unwrap());

    // By a sync's list.
    server.claim_deleted(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();
    assert!(is_cached(&bob, channel));
    assert!(bob.client.has_group(channel).unwrap());
  }

  #[tokio::test]
  async fn a_claim_about_something_unknown_does_nothing() {
    let server = FakeServer::new();
    let (_, mut bob) = two(&server).await;
    let unknown: ConversationId = "424242".parse().unwrap();
    server.claim_deleted(bob.client.user_id(), unknown);
    bob.client.full_sync().await.unwrap();
    assert!(
      bob
        .store
        .list(Table::AppliedChanges)
        .unwrap()
        .iter()
        .all(|(key, _)| !key.contains("424242"))
    );
  }

  /// Listed again: the claim was wrong, or the server lied and thought better of it.
  #[tokio::test]
  async fn being_listed_again_lifts_the_quarantine() {
    let server = FakeServer::new();
    let (clock, mut alice, mut bob) = on_clock(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let hidden = server.hide_from(bob.client.user_id(), channel);
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);

    server.restore_for(bob.client.user_id(), channel, hidden);
    bob.client.full_sync().await.unwrap();
    assert!(is_cached(&bob, channel));

    clock.advance(RETENTION * 2);
    bob.client.full_sync().await.unwrap();
    assert!(
      !bob
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
  }

  #[tokio::test]
  async fn notes_to_self_are_never_purged_on_a_claim() {
    let server = FakeServer::new();
    let (clock, _, mut bob) = on_clock(&server).await;
    let me = bob.client.user_id();
    let notes = bob.client.open_dm(me).await.unwrap();
    bob.client.send_text(notes, None, "mine").await.unwrap();
    settle(&mut [&mut bob]).await;

    let hidden = server.hide_from(me, notes);
    bob.client.full_sync().await.unwrap();
    clock.advance(RETENTION * 2);
    bob.client.full_sync().await.unwrap();

    assert!(
      !bob
        .store
        .messages(notes, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
    drop(hidden);
  }

  /// Everything claimed at once: nothing is purged early, and the user is told once.
  #[tokio::test]
  async fn many_claims_at_once_are_suspect_and_told_once() {
    let server = FakeServer::new();
    let (clock, mut alice, mut bob) = on_clock(&server).await;
    let space = alice.space().await;
    let mut channels = Vec::new();
    for index in 0..7 {
      let channel = alice
        .client
        .create_channel(space, &format!("c{index}"))
        .await
        .unwrap();
      alice
        .client
        .add_member(channel, bob.client.user_id())
        .await
        .unwrap();
      channels.push(channel);
    }
    settle(&mut [&mut alice, &mut bob]).await;
    alice.client.send_text(channels[0], None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    for channel in &channels {
      server.hide_from(bob.client.user_id(), *channel);
    }
    bob.client.full_sync().await.unwrap();

    assert!(suspect_notice(&bob));
    assert!(
      !bob
        .store
        .messages(channels[0], MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
    bob.client.dismiss_suspect_deletions().unwrap();
    bob.client.full_sync().await.unwrap();
    assert!(!suspect_notice(&bob), "told once");

    clock.advance(RETENTION / 2);
    bob.client.full_sync().await.unwrap();
    assert!(
      !bob
        .store
        .messages(channels[0], MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
  }

  #[tokio::test]
  async fn a_single_claim_is_not_suspect() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    delete_unsigned(&alice, channel).await;
    bob.client.full_sync().await.unwrap();
    assert!(!suspect_notice(&bob));
  }

  /// A record that went first (an earlier 404) doesn't let the history stay forever.
  #[tokio::test]
  async fn the_history_goes_also_after_an_earlier_not_found() {
    let server = FakeServer::new();
    let (clock, mut alice, mut bob) = on_clock(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    delete_unsigned(&alice, channel).await;
    // As `refresh_conversation` does on a 404, before any sync heard of the deletion.
    bob
      .store
      .delete(Table::Conversations, &channel.to_string())
      .unwrap();
    assert!(
      !bob
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );

    bob.client.full_sync().await.unwrap();
    clock.advance(RETENTION);
    bob.client.full_sync().await.unwrap();

    assert_purged(&bob, channel);
  }

  /// A purge cut off leaves its marker and messages, so the next round finishes it.
  #[tokio::test]
  async fn a_purge_that_was_cut_off_is_finished() {
    let server = FakeServer::new();
    let (clock, mut alice, mut bob) = on_clock(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    delete_unsigned(&alice, channel).await;
    bob.client.full_sync().await.unwrap();

    // What a crash in the middle of older versions' purge left: outbox and search gone, the rest not.
    bob.store.delete(Table::Members, &channel.to_string()).unwrap();
    clock.advance(RETENTION);
    bob.client.full_sync().await.unwrap();

    assert_purged(&bob, channel);
  }

  /// Plaintext kept outside the messages goes too.
  #[tokio::test]
  async fn a_purge_takes_drafts_staged_files_and_pending_work() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;
    alice
      .client
      .update_account_data(|data, now| data.set_draft(channel, None, "secret draft", now))
      .unwrap();
    let staged = format!("{channel}/staged");
    alice.store.put(Table::PendingUploads, &staged, b"{}").unwrap();
    alice.store.put(Table::PendingPreviews, &staged, b"{}").unwrap();
    alice
      .store
      .put(Table::RemovedSenders, &format!("{channel}/1/2"), b"{}")
      .unwrap();
    alice
      .store
      .put(Table::PendingActions, &format!("read/{channel}"), b"{}")
      .unwrap();

    alice.client.delete_channel(channel).await.unwrap();

    assert_eq!(alice.client.account_data().draft(channel, None), None);
    assert!(alice.store.get(Table::PendingUploads, &staged).unwrap().is_none());
    assert!(
      alice
        .store
        .get(Table::PendingPreviews, &staged)
        .unwrap()
        .is_none()
    );
    assert!(
      alice
        .store
        .get(Table::RemovedSenders, &format!("{channel}/1/2"))
        .unwrap()
        .is_none()
    );
    assert!(
      alice
        .store
        .get(Table::PendingActions, &format!("read/{channel}"))
        .unwrap()
        .is_none()
    );
  }
}

mod deleting_a_space {
  use super::*;

  #[tokio::test]
  async fn an_admin_deletes_it_and_a_device_that_was_away_converges() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let space = alice.space().await;
    server.set_admin(alice.client.user_id());

    alice.client.delete_space(space).await.unwrap();
    assert_purged(&alice, channel);
    assert!(
      alice
        .store
        .get(Table::Spaces, &space.to_string())
        .unwrap()
        .is_none()
    );

    // Bob's device doesn't read its gateway: only the sync. A space's deletion is never signed.
    bob.client.full_sync().await.unwrap();
    assert_hidden_with_history(&bob, channel);
    assert!(
      bob
        .store
        .get(Table::Spaces, &space.to_string())
        .unwrap()
        .is_none()
    );
  }

  #[tokio::test]
  async fn a_member_may_not() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    let space = alice.space().await;

    let error = bob.client.delete_space(space).await.unwrap_err();
    assert!(!error.is_transient(), "{error}");
    assert!(is_cached(&bob, channel));
  }
}

/// A deletion without a signature: what an older client asks for.
async fn delete_unsigned(peer: &Peer, channel: ConversationId) {
  peer
    .api
    .delete_conversation(channel, &DeleteConversationRequest::default())
    .await
    .unwrap();
}

async fn two(server: &FakeServer) -> (Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
  )
}

/// Alice's channel, so she is its only admin, with Bob in it.
async fn channel_with(alice: &mut Peer, bob: &mut Peer) -> ConversationId {
  let channel = alone_in_channel(alice).await;
  alice
    .client
    .add_member(channel, bob.client.user_id())
    .await
    .unwrap();
  settle(&mut [alice, bob]).await;
  channel
}

async fn alone_in_channel(alice: &mut Peer) -> ConversationId {
  let space = alice.space().await;
  let channel = alice.client.create_channel(space, "general").await.unwrap();
  settle(&mut [alice]).await;
  channel
}

async fn archive(peer: &mut Peer, channel: ConversationId) {
  peer
    .client
    .update_channel(
      channel,
      &UpdateConversationRequest {
        maybe_is_archived: Some(true),
        ..UpdateConversationRequest::default()
      },
    )
    .await
    .unwrap();
}

fn is_cached(peer: &Peer, channel: ConversationId) -> bool {
  peer
    .store
    .get(Table::Conversations, &channel.to_string())
    .unwrap()
    .is_some()
}

const RETENTION: i64 = 30 * 24 * 60 * 60 * 1000;

async fn on_clock(server: &FakeServer) -> (Arc<ManualClock>, Peer, Peer) {
  let clock = Arc::new(ManualClock::new(1_000_000));
  let alice = Peer::enroll(server, "Alice").await;
  let bob = Peer::enroll_on_clock(
    server.add_user("Bob"),
    "Bob",
    common::settings(),
    Box::new(clock.clone()),
  )
  .await;
  (clock, alice, bob)
}

fn api_code(error: &CoreError) -> Option<ErrorCode> {
  match error {
    CoreError::Api(api) => api.error_code().cloned(),
    _ => None,
  }
}

fn suspect_notice(peer: &Peer) -> bool {
  peer
    .store
    .get(Table::AppliedChanges, "suspect_deletions")
    .unwrap()
    .is_some()
}

fn pending_about(peer: &Peer, channel: ConversationId) -> Vec<String> {
  peer
    .store
    .list(Table::PendingChanges)
    .unwrap()
    .into_iter()
    .map(|(key, _)| key)
    .filter(|key| key.contains(&channel.to_string()))
    .collect()
}

/// Gone with its history and group: the user confirmed it, or its retention ran out.
fn assert_purged(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
  assert!(
    peer
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .is_empty()
  );
  assert!(
    peer
      .store
      .list(Table::AppliedChanges)
      .unwrap()
      .iter()
      .all(|(key, _)| !key.starts_with(&format!("quarantine/{channel}")))
  );
}

/// On the server's word alone: hidden, its messages still here.
fn assert_hidden_with_history(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
  assert!(
    !peer
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .is_empty()
  );
}

fn assert_hidden_with_history_or_empty(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
}
