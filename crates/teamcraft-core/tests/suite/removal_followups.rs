//! What happens around a removal when it doesn't go through at once (e2ee.md §4.6, removal
//! order): people the server dropped who are still in the group, an admin's change that would get
//! too old, refused removals, announcements after a crash, a server refusing the queue, and a
//! welcome whose join was cut off. Through the fake server, with injected clocks.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common;

use common::{PASSPHRASE, Peer, options, settings, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeServer, NetworkCut};
use teamcraft_core::client::{
  RemovalStatus, group_leftovers, is_queue_refused, pending_removals, unconfirmed_removals,
};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{
  ConversationEvent, LocalStore, MemoryStore, MessageContent, MessageFilter, Table,
};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{Client, ClientSettings, PendingEnrollment};
use teamcraft_crypto::mls::MAX_CHANGE_AGE_EPOCHS;
use teamcraft_proto::{ConversationId, ListMlsMessagesQuery, MlsMessageKind, UserId};

const HOUR: Duration = Duration::from_secs(60 * 60);
const MINUTE: Duration = Duration::from_secs(60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

mod left_in_group {
  use super::*;

  /// The server drops Carol without any signed change: nobody signs her removal on its word (A8),
  /// but every member notes her, and an admin's one click signs it.
  #[tokio::test]
  async fn somebody_the_server_dropped_is_shown_and_removed_by_an_admin() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;
    let carol_id = carol.client.user_id();

    server.remove_member_as_admin(channel, carol_id);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before);
    assert_eq!(
      group_leftovers(alice.store.as_ref(), channel).unwrap(),
      [carol_id]
    );
    assert_eq!(group_leftovers(bob.store.as_ref(), channel).unwrap(), [carol_id]);
    #[cfg(feature = "ui")]
    {
      use teamcraft_client_api::backend::ChatBackend;
      use teamcraft_client_api::model::MembershipIssue;
      let issue = MembershipIssue::LeftInGroup { user: carol_id };
      assert!(
        common::backend_of(&alice)
          .membership_issues(channel)
          .contains(&issue)
      );
      // Only admins are asked.
      assert!(
        !common::backend_of(&bob)
          .membership_issues(channel)
          .contains(&issue)
      );
    }

    alice.client.retry_removal(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert!(group_leftovers(alice.store.as_ref(), channel).unwrap().is_empty());
    assert!(group_leftovers(bob.store.as_ref(), channel).unwrap().is_empty());
    assert_eq!(epoch(&mut alice, channel), epoch(&mut bob, channel));
    alice.client.send_text(channel, None, "after").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(bob.texts(channel).last().map(String::as_str), Some("after"));
  }

  /// Only admins of the signed roster sign a removal of somebody left in the group.
  #[tokio::test]
  async fn a_member_who_is_no_admin_cannot_sign_it() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let alice_id = alice.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner)])
      .await
      .unwrap();
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;
    let carol_id = carol.client.user_id();
    server.remove_member_as_admin(channel, carol_id);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    let error = bob.client.retry_removal(channel, carol_id).await.unwrap_err();
    assert!(
      matches!(error, teamcraft_core::CoreError::NotAnAdmin(_)),
      "{error}"
    );
    assert_eq!(commits(&alice, channel).await, commits_before);
  }

  /// Somebody the server lists again isn't removed by the click.
  #[tokio::test]
  async fn the_click_does_nothing_for_somebody_the_server_lists() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let commits_before = commits(&alice, channel).await;

    let error = alice
      .client
      .retry_removal(channel, carol.client.user_id())
      .await
      .unwrap_err();
    assert!(!error.is_transient(), "{error}");
    assert_eq!(commits(&alice, channel).await, commits_before);
  }
}

mod admin_outside_the_group {
  use super::*;

  /// "Remove from all chats" (#631): the admin's client signs Carol's removal from her channel,
  /// the members commit it (her devices leave, keys rotate), and only then does the server drop
  /// the rest. Only admins of the signed roster can.
  #[tokio::test]
  async fn removal_from_all_chats_is_signed_by_the_admin_and_committed_by_members() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut admin = enroll_with_clock(&server, "Admin", &clock, settings()).await;
    let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(admin_id);
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut admin]).await;
    let carol_id = carol.client.user_id();
    let commits_before = commits(&alice, channel).await;

    let error = bob
      .client
      .admin_remove_from_all_chats(carol_id)
      .await
      .unwrap_err();
    assert!(
      matches!(error, teamcraft_core::CoreError::NotAnAdmin(_)),
      "{error}"
    );

    admin.client.admin_remove_from_all_chats(carol_id).await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 1);
    assert!(!is_listed(&alice, channel, carol_id).await);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut admin]).await;

    assert_eq!(commits(&alice, channel).await, commits_before + 1);
    assert_eq!(epoch(&mut alice, channel), epoch(&mut bob, channel));
    assert!(group_leftovers(alice.store.as_ref(), channel).unwrap().is_empty());
  }

  /// The admin can't see the members' commit; it learns the group's epoch and signs again
  /// before members would refuse the change as too old, then stops watching.
  #[tokio::test]
  async fn the_admin_signs_again_before_the_change_gets_too_old() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut admin = enroll_with_clock(&server, "Admin", &clock, settings()).await;
    let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(admin_id);
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    admin.client.admin_remove_member(channel, carol_id).await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 1);
    assert!(!is_listed(&alice, channel, carol_id).await);

    // Members committed other changes meanwhile (the fake only moves the epoch).
    server.bump_epoch(channel, MAX_CHANGE_AGE_EPOCHS / 2);
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 1, "not due yet");

    clock.advance(millis(HOUR));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 2, "signed again");

    // Fresh again: the next check only looks.
    clock.advance(millis(HOUR));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 2);

    // Still within `removal_watch` of the first signing: signed again.
    clock.advance(millis(29 * DAY));
    server.bump_epoch(channel, MAX_CHANGE_AGE_EPOCHS / 2);
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 3);

    // Signing again doesn't extend the watch: 31 days after the first signing it is over.
    clock.advance(millis(2 * DAY));
    server.bump_epoch(channel, MAX_CHANGE_AGE_EPOCHS / 2);
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, 3);
  }

  /// Somebody added Carol back after the admin's removal: the admin, who sees the server list
  /// her again, doesn't sign her removal again, and doesn't forget it either: it waits as
  /// unconfirmed until the admin keeps her.
  #[tokio::test]
  async fn the_admin_does_not_sign_again_after_the_user_was_added_back() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut admin = enroll_with_clock(&server, "Admin", &clock, settings()).await;
    let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(admin_id);
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    admin.client.admin_remove_member(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.add_member(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(is_listed(&alice, channel, carol_id).await);
    let before = proposals_by(&alice, &admin, channel).await;

    server.bump_epoch(channel, MAX_CHANGE_AGE_EPOCHS / 2);
    clock.advance(millis(HOUR));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, before);
    assert!(is_listed(&alice, channel, carol_id).await);
    assert_eq!(
      unconfirmed_removals(admin.store.as_ref()).unwrap(),
      [(channel, carol_id)]
    );

    // It waits for the admin, however long.
    clock.advance(millis(2 * DAY));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, before);
    admin.client.discard_removal(channel, carol_id).await.unwrap();
    assert!(unconfirmed_removals(admin.store.as_ref()).unwrap().is_empty());
    assert!(is_listed(&alice, channel, carol_id).await);
  }

  /// A hostile server lists Carol again without anybody adding her (and holds the admin's change
  /// back): the admin isn't fooled into forgetting the removal; "remove again" signs and sends it
  /// afresh.
  #[tokio::test]
  async fn a_server_listing_the_user_again_leaves_the_removal_to_the_admin() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut admin = enroll_with_clock(&server, "Admin", &clock, settings()).await;
    let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(admin_id);
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    admin.client.admin_remove_member(channel, carol_id).await.unwrap();
    // On the server only: no signed addition.
    admin
      .api
      .add_member(
        channel,
        &teamcraft_proto::AddConversationMemberRequest {
          user_id: carol_id,
          maybe_role: None,
        },
      )
      .await
      .unwrap();
    server.bump_epoch(channel, MAX_CHANGE_AGE_EPOCHS / 2);
    clock.advance(millis(HOUR));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(
      unconfirmed_removals(admin.store.as_ref()).unwrap(),
      [(channel, carol_id)]
    );
    let before = proposals_by(&alice, &admin, channel).await;

    admin.client.retry_removal(channel, carol_id).await.unwrap();
    assert_eq!(proposals_by(&alice, &admin, channel).await, before + 1);
    assert!(!is_listed(&alice, channel, carol_id).await);
    assert!(unconfirmed_removals(admin.store.as_ref()).unwrap().is_empty());
  }

  /// The server step of an admin outside the group is resumed later (it was refused): somebody
  /// may have added the user back meanwhile, which the admin can't see. It isn't sent on its own
  /// but waits for the admin.
  #[tokio::test]
  async fn a_resumed_server_step_of_an_admin_outside_the_group_waits_for_the_admin() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let mut admin = enroll_with_clock(&server, "Admin", &clock, settings()).await;
    let (alice_id, admin_id) = (alice.client.user_id(), admin.client.user_id());
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (admin_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(admin_id);
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.refuse_removal_steps_by(admin_id, true);
    admin
      .client
      .admin_remove_member(channel, carol_id)
      .await
      .unwrap_err();
    server.refuse_removal_steps_by(admin_id, false);
    let requests = server.removal_requests();

    clock.advance(millis(2 * DAY));
    admin.client.apply_pending_changes().await.unwrap();
    assert_eq!(server.removal_requests(), requests, "not sent on its own");
    assert!(is_listed(&alice, channel, carol_id).await);
    assert_eq!(
      unconfirmed_removals(admin.store.as_ref()).unwrap(),
      [(channel, carol_id)]
    );

    admin.client.retry_removal(channel, carol_id).await.unwrap();
    assert_eq!(server.removal_requests(), requests + 1);
    assert!(!is_listed(&alice, channel, carol_id).await);
  }
}

mod added_back {
  use super::*;

  /// Alice's removal of Carol waits for its refused server step; Alice adds Carol back. The
  /// pending removal is gone: its retry doesn't drop Carol's new membership.
  #[tokio::test]
  async fn the_removers_own_addition_drops_the_pending_removal() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = enroll_with_clock(&server, "Alice", &clock, settings()).await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.refuse_removal_steps_by(alice.client.user_id(), true);
    alice.client.remove_member(channel, carol_id).await.unwrap_err();
    server.refuse_removal_steps_by(alice.client.user_id(), false);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.add_member(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let requests = server.removal_requests();

    clock.advance(millis(2 * DAY));
    alice.client.apply_pending_changes().await.unwrap();
    assert_eq!(server.removal_requests(), requests);
    assert!(is_listed(&alice, channel, carol_id).await);
    assert!(
      pending_removals(alice.store.as_ref(), channel)
        .unwrap()
        .is_empty()
    );
  }

  /// Bob's removal of Carol waits for its server step; Alice adds Carol back and Bob's device
  /// sees the commit: Bob's retry days later leaves Carol's membership alone.
  #[tokio::test]
  async fn an_observed_addition_drops_another_members_pending_removal() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = enroll_with_clock(&server, "Bob", &clock, settings()).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.refuse_removal_steps_by(bob.client.user_id(), true);
    bob.client.remove_member(channel, carol_id).await.unwrap_err();
    server.refuse_removal_steps_by(bob.client.user_id(), false);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.add_member(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let requests = server.removal_requests();

    clock.advance(millis(2 * DAY));
    bob.client.apply_pending_changes().await.unwrap();
    assert_eq!(server.removal_requests(), requests);
    assert!(is_listed(&alice, channel, carol_id).await);
    assert!(pending_removals(bob.store.as_ref(), channel).unwrap().is_empty());
  }

  /// Even without seeing the addition (the commit came with a welcome it joined from), a member
  /// whose group holds the user again doesn't run the server step.
  #[tokio::test]
  async fn a_retried_server_step_skips_a_user_back_in_the_group() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = enroll_with_clock(&server, "Bob", &clock, settings()).await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.refuse_removal_steps_by(bob.client.user_id(), true);
    bob.client.remove_member(channel, carol_id).await.unwrap_err();
    server.refuse_removal_steps_by(bob.client.user_id(), false);
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // Bob's pending removal comes back as an older version of the app left it.
    let key = format!("removal/{channel}/{carol_id}");
    let stored = bob.store.get(Table::PendingChanges, &key).unwrap();
    alice.client.add_member(channel, carol_id).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    bob
      .store
      .put(Table::PendingChanges, &key, &stored.unwrap())
      .unwrap();
    let requests = server.removal_requests();

    clock.advance(millis(2 * DAY));
    bob.client.apply_pending_changes().await.unwrap();
    assert_eq!(server.removal_requests(), requests);
    assert!(is_listed(&alice, channel, carol_id).await);
  }
}

mod refused {
  use super::*;

  /// The remover lost the right between the check and the server step: the server step is
  /// tried again after 1 h, 2 h, 2 h (the ceiling), then shown as failed until "retry".
  #[tokio::test]
  async fn a_refused_removal_backs_off_and_then_shows_as_failed() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let removal_settings = ClientSettings {
      removal_retry: HOUR,
      removal_retry_max: 2 * HOUR,
      removal_attempts: 4,
      ..settings()
    };
    let mut alice = enroll_with_clock(&server, "Alice", &clock, removal_settings).await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.refuse_removal_steps_by(alice.client.user_id(), true);
    alice.client.remove_member(channel, carol_id).await.unwrap_err();
    assert_eq!(server.removal_requests(), 1);
    assert_eq!(
      pending_removals(alice.store.as_ref(), channel).unwrap(),
      [(carol_id, RemovalStatus::Pending)]
    );

    for (wait, requests) in [
      (HOUR - MINUTE, 1),
      (MINUTE, 2),
      (2 * HOUR - MINUTE, 2),
      (MINUTE, 3),
      (2 * HOUR - MINUTE, 3),
      (MINUTE, 4),
    ] {
      clock.advance(millis(wait));
      alice.client.apply_pending_changes().await.unwrap();
      assert_eq!(server.removal_requests(), requests);
    }
    assert_eq!(
      pending_removals(alice.store.as_ref(), channel).unwrap(),
      [(carol_id, RemovalStatus::Failed)]
    );
    #[cfg(feature = "ui")]
    {
      use teamcraft_client_api::backend::ChatBackend;
      use teamcraft_client_api::model::MembershipIssue;
      assert!(
        common::backend_of(&alice)
          .membership_issues(channel)
          .contains(&MembershipIssue::RemovalFailed { user: carol_id })
      );
    }
    // Failed waits for the user, however long.
    clock.advance(millis(Duration::from_secs(10 * 24 * 60 * 60)));
    alice.client.apply_pending_changes().await.unwrap();
    assert_eq!(server.removal_requests(), 4);

    server.refuse_removal_steps_by(alice.client.user_id(), false);
    alice.client.retry_removal(channel, carol_id).await.unwrap();
    assert_eq!(server.removal_requests(), 5);
    assert!(!is_listed(&alice, channel, carol_id).await);
    assert!(
      pending_removals(alice.store.as_ref(), channel)
        .unwrap()
        .is_empty()
    );
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    // Announced once, with the first round's commit.
    assert_eq!(
      count(
        &bob,
        channel,
        &ConversationEvent::MemberRemoved { user: carol_id }
      ),
      1
    );
  }
}

mod announced_once {
  use super::*;

  #[tokio::test]
  async fn a_removal_finished_on_a_later_round_announces_it_once() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let carol_id = carol.client.user_id();

    server.lose_next_commit_answer(alice.client.user_id());
    alice.client.remove_member(channel, carol_id).await.unwrap_err();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    alice.client.apply_pending_changes().await.unwrap();
    alice.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert!(!is_listed(&alice, channel, carol_id).await);
    assert_eq!(
      count(
        &bob,
        channel,
        &ConversationEvent::MemberRemoved { user: carol_id }
      ),
      1
    );
    assert_eq!(
      count(
        &carol,
        channel,
        &ConversationEvent::MemberRemoved { user: carol_id }
      ),
      0
    );
  }

  /// The goodbye went out, then the leave was cut off and the app restarted: leaving again
  /// finishes the same leave without a second goodbye.
  #[tokio::test]
  async fn leaving_again_after_an_interrupted_leave_says_goodbye_once() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    server.cut_next_proposal(carol.client.user_id(), NetworkCut::BeforeRequest);
    let error = carol.client.leave_channel(channel).await.unwrap_err();
    assert!(error.is_transient(), "{error}");
    let mut carol = restart(carol).await;
    carol.client.leave_channel(channel).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    bob.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    carol.client.apply_pending_changes().await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(count(&bob, channel, &ConversationEvent::Left), 1);
    assert!(!is_listed(&alice, channel, carol.client.user_id()).await);
    assert!(!carol.client.has_group(channel).unwrap());
  }
}

mod refused_queue {
  use super::*;

  /// A server that lists Bob but refuses him the group's queue can't make him stop following it
  /// unnoticed: the group stays, the refusal is shown.
  #[tokio::test]
  async fn a_member_refused_the_queue_sees_it() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    server.refuse_queue_to(bob.client.user_id());
    bob.client.sync_mls(channel).await.unwrap();

    assert!(is_queue_refused(bob.store.as_ref(), channel).unwrap());
    assert!(bob.client.has_group(channel).unwrap());
    assert!(is_cached(&bob, channel));
    #[cfg(feature = "ui")]
    {
      use teamcraft_client_api::backend::ChatBackend;
      use teamcraft_client_api::model::MembershipIssue;
      assert!(
        common::backend_of(&bob)
          .membership_issues(channel)
          .contains(&MembershipIssue::GroupRefused)
      );
    }
  }

  /// Expected while leaving: the server dropped the membership, the commit is still to come.
  #[tokio::test]
  async fn a_leaving_device_refused_the_queue_stays_quiet() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    carol.client.leave_channel(channel).await.unwrap();
    server.refuse_queue_to(carol.client.user_id());
    carol.client.sync_mls(channel).await.unwrap();

    assert!(!is_queue_refused(carol.store.as_ref(), channel).unwrap());
  }

  /// The server says the user isn't a member any more: the conversation goes, as on its
  /// removal event.
  #[tokio::test]
  async fn a_member_the_server_no_longer_lists_loses_the_conversation() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    // Bob's client misses the server's removal event.
    server.set_gateway_silent(bob.client.user_id(), true);
    server.remove_member_as_admin(channel, bob.client.user_id());
    bob.client.sync_mls(channel).await.unwrap();

    assert!(!is_queue_refused(bob.store.as_ref(), channel).unwrap());
    assert!(!bob.client.has_group(channel).unwrap());
    assert!(!is_cached(&bob, channel));
  }
}

mod welcome {
  use super::*;

  /// Dave opens the welcome (which spends his key package), then the network drops while he
  /// pins Carol, whom the server dropped but who is still in the tree. He asks to be added back,
  /// a member does, and he reads what follows.
  #[tokio::test]
  async fn a_cut_off_welcome_join_is_followed_by_a_rejoin_request() {
    let server = FakeServer::new();
    let quick = ClientSettings {
      device_add_stagger: Duration::ZERO,
      ..settings()
    };
    let mut alice = Peer::enroll_with_settings(&server, "Alice", quick.clone()).await;
    let mut bob = Peer::enroll_with_settings(&server, "Bob", quick.clone()).await;
    let mut carol = Peer::enroll_with_settings(&server, "Carol", quick.clone()).await;
    let mut dave = Peer::enroll_with_settings(&server, "Dave", quick).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    server.remove_member_as_admin(channel, carol.client.user_id());
    settle(&mut [&mut alice, &mut bob]).await;

    server.fail_next_identity_lookup(carol.client.user_id());
    alice
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut dave]).await;

    // The rejoin request, and the commit that answered it with a second welcome.
    assert_eq!(proposals_by(&alice, &dave, channel).await, 1);
    assert!(dave.client.has_group(channel).unwrap());
    alice.client.send_text(channel, None, "welcome").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut dave]).await;
    assert_eq!(dave.texts(channel).last().map(String::as_str), Some("welcome"));
  }
}

mod welcome_retry {
  use super::*;

  /// The rejoin request after a cut-off welcome doesn't go out either: the next syncs don't ask
  /// on every round but after `rejoin_retry`, and the request still gets Dave in.
  #[tokio::test]
  async fn a_failed_rejoin_request_is_asked_again_after_a_wait() {
    let server = FakeServer::new();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let quick = ClientSettings {
      device_add_stagger: Duration::ZERO,
      ..settings()
    };
    let mut alice = Peer::enroll_with_settings(&server, "Alice", quick.clone()).await;
    let mut bob = Peer::enroll_with_settings(&server, "Bob", quick.clone()).await;
    let mut carol = Peer::enroll_with_settings(&server, "Carol", quick.clone()).await;
    let mut dave = enroll_with_clock(&server, "Dave", &clock, quick).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    server.remove_member_as_admin(channel, carol.client.user_id());
    settle(&mut [&mut alice, &mut bob]).await;

    server.fail_next_identity_lookup(carol.client.user_id());
    server.cut_next_proposal(dave.client.user_id(), NetworkCut::BeforeRequest);
    alice
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let error = dave.client.sync_welcomes().await.unwrap_err();
    assert!(error.is_transient(), "{error}");

    dave.client.full_sync().await.unwrap();
    assert_eq!(proposals_by(&alice, &dave, channel).await, 0, "not on every sync");

    clock.advance(millis(Duration::from_secs(10 * 60)));
    dave.client.full_sync().await.unwrap();
    assert_eq!(proposals_by(&alice, &dave, channel).await, 1);
    settle(&mut [&mut alice, &mut bob, &mut dave]).await;
    assert!(dave.client.has_group(channel).unwrap());
  }
}

async fn three(server: &FakeServer) -> (Peer, Peer, Peer) {
  (
    Peer::enroll(server, "Alice").await,
    Peer::enroll(server, "Bob").await,
    Peer::enroll(server, "Carol").await,
  )
}

/// A peer whose client runs on `clock`, to move its time.
async fn enroll_with_clock(
  server: &FakeServer,
  name: &str,
  clock: &Arc<ManualClock>,
  client_settings: ClientSettings,
) -> Peer {
  let api = server.add_user(name);
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::new()
    .complete(
      api.clone(),
      store.clone(),
      Box::new(clock.clone()),
      client_settings,
      options(PASSPHRASE),
    )
    .await
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: name.to_string(),
    api,
    store,
    client,
    gateway,
  }
}

/// A channel of `owner` with `others` added and joined.
async fn channel_with(owner: &mut Peer, others: &mut [&mut Peer]) -> ConversationId {
  let space = owner.space().await;
  let channel = owner.client.create_channel(space, "general").await.unwrap();
  for other in others.iter() {
    owner
      .client
      .add_member(channel, other.client.user_id())
      .await
      .unwrap();
  }
  let mut peers: Vec<&mut Peer> = vec![owner];
  peers.extend(others.iter_mut().map(|peer| &mut **peer));
  settle(&mut peers).await;
  channel
}

async fn restart(peer: Peer) -> Peer {
  let Peer { name, api, store, .. } = peer;
  let mut client = Client::open(api.clone(), store.clone(), common::clock(), settings())
    .unwrap()
    .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name,
    api,
    store,
    client,
    gateway,
  }
}

fn epoch(peer: &mut Peer, channel: ConversationId) -> u64 {
  peer.client.mls_epoch(channel).unwrap().unwrap()
}

async fn is_listed(peer: &Peer, channel: ConversationId, user: UserId) -> bool {
  peer
    .api
    .list_members(channel)
    .await
    .unwrap()
    .members
    .iter()
    .any(|member| member.user.id == user)
}

async fn commits(peer: &Peer, channel: ConversationId) -> usize {
  mls_entries(peer, channel)
    .await
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Commit)
    .count()
}

/// Signed changes `sender` posted into the group's queue, as `reader` (a member) sees them.
async fn proposals_by(reader: &Peer, sender: &Peer, channel: ConversationId) -> usize {
  let device = sender.client.device_id();
  mls_entries(reader, channel)
    .await
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal && entry.sender_device_id == device)
    .count()
}

async fn mls_entries(peer: &Peer, channel: ConversationId) -> Vec<teamcraft_proto::MlsMessageResponse> {
  peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
}

/// How often `event` shows in the peer's history of the conversation.
fn count(peer: &Peer, channel: ConversationId, event: &ConversationEvent) -> usize {
  peer
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter(|message| matches!(&message.content, MessageContent::Event { event: shown } if shown == event))
    .count()
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap()
}

/// Whether the conversation is still in the peer's conversation list.
fn is_cached(peer: &Peer, channel: ConversationId) -> bool {
  peer
    .store
    .get(Table::Conversations, &channel.to_string())
    .unwrap()
    .is_some()
}
