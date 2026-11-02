//! Getting out of a channel and deleting it (issue #1130, e2ee.md §4.6): every role × state × last
//! admin / last member for leave and delete, leaving with no other device online, and a device
//! that was away when the channel went. Through the fake server.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::store::{LocalStore, MessageFilter, Table};
use teamcraft_proto::{
  ChannelRole, ConversationId, UpdateConversationMemberRequest, UpdateConversationRequest,
};

mod leaving {
  use super::*;

  #[tokio::test]
  async fn the_last_admin_of_an_active_channel_is_refused() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;

    let error = alice.client.leave_channel(channel).await.unwrap_err();
    assert!(error.to_string().contains("delete the channel"), "{error}");
    assert!(alice.client.has_group(channel).unwrap());
  }

  /// The report behind #1130, and leaving with nobody else online: Bob's device doesn't run.
  #[tokio::test]
  async fn the_last_admin_of_an_archived_channel_leaves_with_no_other_device_online() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    archive(&mut alice, channel).await;

    alice.client.leave_channel(channel).await.unwrap();
    assert!(
      !alice
        .api
        .list_members(channel)
        .await
        .is_ok_and(|listed| listed.members.iter().any(|m| m.user.id == alice.client.user_id())),
      "the server's membership is gone without waiting for a commit"
    );
    settle(&mut [&mut alice]).await;
    assert!(!alice.client.has_group(channel).unwrap());
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
      bob.client.leave_channel(channel).await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      alice.client.apply_pending_changes().await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      bob.client.apply_pending_changes().await.unwrap();
      settle(&mut [&mut alice, &mut bob]).await;
      assert!(!is_cached(&bob, channel), "archived {is_archived}");
      assert!(is_cached(&alice, channel), "archived {is_archived}");
    }
  }

  /// A channel doesn't stay behind without members, active or archived.
  #[tokio::test]
  async fn the_last_member_deletes_the_channel_by_leaving() {
    for is_archived in [false, true] {
      let server = FakeServer::new();
      let (mut alice, _) = two(&server).await;
      let channel = alone_in_channel(&mut alice).await;
      alice.client.send_text(channel, None, "hello").await.unwrap();
      if is_archived {
        archive(&mut alice, channel).await;
      }

      alice.client.leave_channel(channel).await.unwrap();
      settle(&mut [&mut alice]).await;

      assert!(
        alice.api.conversation(channel).await.is_err(),
        "archived {is_archived}"
      );
      assert_gone(&alice, channel);
    }
  }

  #[tokio::test]
  async fn the_last_members_other_device_drops_it_on_its_next_sync() {
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

    alice.client.leave_channel(channel).await.unwrap();

    phone.full_sync().await.unwrap();
    assert!(
      phone
        .store()
        .get(Table::Conversations, &channel.to_string())
        .unwrap()
        .is_none()
    );
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

      assert_gone(&alice, channel);
      assert_gone(&bob, channel);
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

    assert_gone(&alice, channel);
  }

  /// Offline when it happened: the next full sync drops it, with its local history.
  #[tokio::test]
  async fn a_device_that_was_offline_converges_on_its_next_sync() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;
    alice.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    assert!(
      !bob
        .store
        .messages(channel, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );

    alice.client.delete_channel(channel).await.unwrap();

    // Bob's device doesn't read its gateway (offline beyond the resume window): only the sync.
    bob.client.full_sync().await.unwrap();
    assert_gone(&bob, channel);
  }

  /// A lost answer, or a second admin: gone already counts as done.
  #[tokio::test]
  async fn deleting_again_finishes_instead_of_failing() {
    let server = FakeServer::new();
    let (mut alice, _) = two(&server).await;
    let channel = alone_in_channel(&mut alice).await;

    alice.client.delete_channel(channel).await.unwrap();
    alice.client.delete_channel(channel).await.unwrap();
    assert_gone(&alice, channel);
  }

  /// A leave waiting for the members' commit has nothing left to do once the channel is gone.
  #[tokio::test]
  async fn a_pending_leave_goes_with_the_channel() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let channel = channel_with(&mut alice, &mut bob).await;

    bob.client.leave_channel(channel).await.unwrap();
    assert!(bob.client.has_group(channel).unwrap(), "waits for the commit");

    alice.client.delete_channel(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    bob.client.apply_pending_changes().await.unwrap();

    assert_gone(&bob, channel);
    assert!(
      bob
        .store
        .list(Table::PendingChanges)
        .unwrap()
        .iter()
        .all(|(key, _)| !key.contains(&channel.to_string()))
    );
  }

  #[tokio::test]
  async fn a_direct_message_is_refused() {
    let server = FakeServer::new();
    let (mut alice, mut bob) = two(&server).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;

    let error = alice.client.delete_channel(dm).await.unwrap_err();
    assert!(!error.is_transient(), "{error}");
    assert!(is_cached(&alice, dm));
  }
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

/// Dropped with its history and group, as e2ee.md §4.6 says.
fn assert_gone(peer: &Peer, channel: ConversationId) {
  assert!(!is_cached(peer, channel));
  assert!(
    peer
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .is_empty()
  );
}
