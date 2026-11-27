//! Strict channels between clients and the fake server (e2ee.md §4.6.2, issue #1896): turning
//! strict on and changing the roles, stopping at a commit that can't be taken (D4), passing over
//! what was built on it, the outbox while stopped, and an admin continuing from the last good
//! epoch through the server's rewind. The verdicts on each signer and action are
//! `teamcraft_crypto`'s tests (`strict_roles`, `mls::group::tests::strict`).
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, settle};
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::error::CoreError;
use teamcraft_core::workspace::RosterRole;
use teamcraft_crypto::strict_roles::StrictRoleKind;
use teamcraft_proto::Id as _;
use teamcraft_proto::{
  Base64Bytes, ConversationId, ListMlsMessagesQuery, MlsMessageKind, MlsStrictHintQuery, PostMlsCommitRequest,
};

mod turning_on {
  use super::*;

  #[tokio::test]
  async fn a_workspace_owner_turns_a_private_channel_strict_for_every_member() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let alice_id = alice.client.user_id();

    alice
      .client
      .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      let roles = peer.client.strict_roles(channel).unwrap().unwrap();
      assert_eq!(roles.admins().count(), 1);
      // Kept in the account data for a welcome back after falling out.
      let known = peer.client.account_data().known_strict_roles(channel).cloned();
      assert!(known.unwrap().maybe_roles.is_some());
    }
    assert_eq!(epoch(&mut alice, channel), epoch(&mut bob, channel));
    assert_eq!(epoch(&mut alice, channel), epoch(&mut carol, channel));
  }

  #[tokio::test]
  async fn the_roles_change_and_strict_turns_off_again() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let alice_id = alice.client.user_id();
    let bob_id = bob.client.user_id();
    strict(&mut alice, channel).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    // Alice now signs as an admin of the context.
    alice
      .client
      .set_strict_roles(
        channel,
        &[
          (alice_id, StrictRoleKind::Admin),
          (bob_id, StrictRoleKind::Moderator),
        ],
      )
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let roles = carol.client.strict_roles(channel).unwrap().unwrap();
    assert_eq!(roles.version(), 2);
    assert_eq!(
      roles.role_of(crypto_user(bob_id)).map(|role| role.role),
      Some(StrictRoleKind::Moderator)
    );

    alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    for peer in [&mut alice, &mut bob, &mut carol] {
      assert!(!peer.client.is_strict(channel).unwrap());
      let known = peer.client.account_data().known_strict_roles(channel).cloned();
      assert_eq!(known.unwrap().maybe_roles, None, "turned off, as the device saw");
    }
  }

  /// Off and on again: the turning on signs the version after the "off", and every member's
  /// account data takes the new roles.
  #[tokio::test]
  async fn strict_on_again_after_off_reaches_every_members_account_data() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let alice_id = alice.client.user_id();
    alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    alice
      .client
      .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      assert_eq!(peer.client.strict_roles(channel).unwrap().unwrap().version(), 3);
      let known = peer
        .client
        .account_data()
        .known_strict_roles(channel)
        .cloned()
        .unwrap();
      assert!(known.maybe_roles.is_some(), "{}", peer.name);
      assert_eq!(known.version(), Some(3), "{}", peer.name);
    }
  }

  #[tokio::test]
  async fn a_member_without_a_workspace_role_cannot_turn_it_on() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let bob_id = bob.client.user_id();
    let epoch_before = epoch(&mut bob, channel);

    let result = bob
      .client
      .set_strict_roles(channel, &[(bob_id, StrictRoleKind::Admin)])
      .await;

    assert!(result.is_err(), "{result:?}");
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(!alice.client.is_strict(channel).unwrap());
    assert_eq!(epoch(&mut alice, channel), epoch_before, "nothing was committed");
  }

  /// Only private channels: never a DM (e2ee.md §4.6.2).
  #[tokio::test]
  async fn a_dm_never_turns_strict() {
    let server = FakeServer::new();
    let (mut alice, mut bob, _carol) = three_with_owner(&server).await;
    let dm = alice.client.open_dm(bob.client.user_id()).await.unwrap();
    settle(&mut [&mut alice, &mut bob]).await;
    let alice_id = alice.client.user_id();

    let result = alice
      .client
      .set_strict_roles(dm, &[(alice_id, StrictRoleKind::Admin)])
      .await;

    assert!(
      matches!(result, Err(CoreError::NotStrictCapable(id)) if id == dm),
      "{result:?}"
    );
  }
}

/// Converting to public needs strict off first (e2ee.md §4.6.2, "No self-add").
mod converting {
  use super::*;
  use teamcraft_proto::{ConversationVisibility, UpdateConversationRequest};

  #[tokio::test]
  async fn a_strict_channel_is_made_public_only_after_strict_is_off() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let public = UpdateConversationRequest {
      maybe_visibility: Some(ConversationVisibility::Public),
      ..Default::default()
    };

    let result = alice.client.update_channel(channel, &public).await;
    assert!(
      matches!(result, Err(CoreError::NotStrictCapable(id)) if id == channel),
      "{result:?}"
    );

    alice.client.set_strict_roles(channel, &[]).await.unwrap();
    alice.client.update_channel(channel, &public).await.unwrap();
  }
}

/// D4: a commit a strict channel can't take stops every honest device at the last good epoch,
/// and an admin continues from there.
mod stopping {
  use super::*;

  #[tokio::test]
  async fn an_unreadable_commit_stops_every_member_alike() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);

    // Bob's modified client takes the epoch with a commit nobody can read (a group founded
    // alone, or garbage: finding 4).
    let refused = post_garbage_commit(&bob, channel, good_epoch).await;
    settle(&mut [&mut alice, &mut carol]).await;

    for peer in [&mut alice, &mut carol] {
      let stop = peer.client.strict_stop(channel).unwrap().unwrap();
      assert_eq!(stop.epoch, good_epoch);
      assert_eq!(stop.commit_id, refused);
      assert_eq!(epoch(peer, channel), good_epoch, "stays at the last good epoch");
      assert!(peer.client.has_group(channel).unwrap());
    }
    assert_eq!(
      rejoin_requests(&alice, channel).await,
      0,
      "a stopped device doesn't ask to be added back"
    );
  }

  #[tokio::test]
  async fn later_entries_are_passed_over_and_messages_still_flow() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    post_garbage_commit(&bob, channel, good_epoch + 1).await;

    settle(&mut [&mut alice, &mut carol]).await;
    // Both stopped, not behind: the outbox counts as caught up and sends with the current key.
    carol.client.send_text(channel, None, "still here").await.unwrap();
    settle(&mut [&mut alice, &mut carol]).await;

    assert_eq!(epoch(&mut alice, channel), good_epoch);
    assert_eq!(
      alice.texts(channel).last().map(String::as_str),
      Some("still here")
    );
    assert_eq!(rejoin_requests(&carol, channel).await, 0);
  }

  #[tokio::test]
  async fn no_commit_of_its_own_goes_out_while_stopped() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    settle(&mut [&mut alice, &mut carol]).await;

    let result = alice.client.set_strict_roles(channel, &[]).await;

    assert!(
      matches!(result, Err(CoreError::StrictStopped(id)) if id == channel),
      "{result:?}"
    );
    assert_eq!(epoch(&mut alice, channel), good_epoch);
  }

  #[tokio::test]
  async fn an_admin_removes_the_committer_and_continues() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    post_garbage_commit(&bob, channel, good_epoch + 1).await;
    settle(&mut [&mut alice, &mut carol]).await;
    let bob_id = bob.client.user_id();

    alice
      .client
      .continue_strict_channel(channel, Some(bob_id))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut carol]).await;

    assert_eq!(server.rewinds(), vec![(channel, good_epoch)]);
    for peer in [&mut alice, &mut carol] {
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None);
      assert_eq!(epoch(peer, channel), good_epoch + 1);
      assert!(!peer.client.group_users(channel).unwrap().contains(&bob_id));
    }
    // The group goes on: Carol's message reaches Alice under the new epoch's key.
    carol.client.send_text(channel, None, "after").await.unwrap();
    settle(&mut [&mut alice, &mut carol]).await;
    assert_eq!(alice.texts(channel).last().map(String::as_str), Some("after"));
  }

  #[tokio::test]
  async fn an_admin_continues_without_removing_anybody() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    settle(&mut [&mut alice, &mut carol]).await;

    alice.client.continue_strict_channel(channel, None).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None);
      assert_eq!(epoch(peer, channel), good_epoch + 1);
    }
  }

  /// The rewind is under `manage_channel_roles`: a plain member's continuation is refused by
  /// the server and changes nothing.
  #[tokio::test]
  async fn a_plain_member_cannot_continue() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    settle(&mut [&mut alice, &mut carol]).await;

    let result = carol.client.continue_strict_channel(channel, None).await;

    assert!(result.is_err(), "{result:?}");
    assert!(server.rewinds().is_empty());
    assert!(carol.client.strict_stop(channel).unwrap().is_some());
    assert_eq!(epoch(&mut carol, channel), good_epoch);
  }

  #[tokio::test]
  async fn a_device_that_did_not_stop_has_nothing_to_continue() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;

    let result = alice.client.continue_strict_channel(channel, None).await;

    assert!(
      matches!(result, Err(CoreError::StrictNotNow(id, _)) if id == channel),
      "{result:?}"
    );
  }

  /// A channel that isn't strict keeps §4.6: an unreadable commit is skipped, and the device
  /// asks to be added back once it is behind.
  #[tokio::test]
  async fn a_channel_that_is_not_strict_does_not_stop() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;

    settle(&mut [&mut alice, &mut carol]).await;

    assert_eq!(alice.client.strict_stop(channel).unwrap(), None);
  }
}

/// Opting a channel in (e2ee.md §4.6.2, "Turning it on", steps 2 and 3, issue #1924): the
/// channel's devices start on apps from before (`test_hooks::set_legacy_leaves`), update, answer
/// a workspace owner's ask over the real queue and refresh their leaves; a device that never
/// answers blocks it until it is removed.
mod opting_in {
  use super::*;
  use teamcraft_crypto::mls::test_hooks;

  #[tokio::test]
  async fn devices_answer_refresh_their_leaves_and_the_channel_turns_strict() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, channel) = channel_on_old_apps(&server).await;
    let readiness = alice.client.strict_readiness(channel).unwrap();
    assert_eq!(readiness.waiting.len(), 3);
    assert!(readiness.maybe_ask.is_none() && !readiness.is_ready);
    let alice_id = alice.client.user_id();
    // Not before every device refreshed: a leaf without the capability refuses the switch.
    assert!(
      alice
        .client
        .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
        .await
        .is_err()
    );

    alice.client.ask_strict_ready(channel).await.unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob, &mut carol]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      let readiness = peer.client.strict_readiness(channel).unwrap();
      assert!(readiness.is_ready, "{}: {readiness:?}", peer.name);
      assert_eq!(readiness.maybe_ask.unwrap().asker, alice_id);
    }
    let epoch_before = epoch(&mut alice, channel);
    alice
      .client
      .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    for peer in [&mut alice, &mut bob, &mut carol] {
      assert!(peer.client.is_strict(channel).unwrap(), "{}", peer.name);
    }
    assert_eq!(epoch(&mut bob, channel), epoch_before + 1);
  }

  /// D6: Carol's device is offline (or on an older app) and never answers. Nobody refreshes; the
  /// asker sees her device waiting and removes it, and then the others go on.
  #[tokio::test]
  async fn a_device_that_never_answers_blocks_until_it_is_removed() {
    let server = FakeServer::new();
    let (mut alice, mut bob, carol, channel) = channel_on_old_apps(&server).await;
    let carol_device = (carol.client.user_id(), carol.client.device_id());
    let epoch_before = epoch(&mut alice, channel);

    alice.client.ask_strict_ready(channel).await.unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob]).await;

    let readiness = alice.client.strict_readiness(channel).unwrap();
    assert_eq!(readiness.waiting, vec![carol_device]);
    assert_eq!(readiness.refreshing.len(), 2, "{readiness:?}");
    assert_eq!(epoch(&mut alice, channel), epoch_before, "nobody refreshed yet");

    let (carol_user, carol_device_id) = carol_device;
    alice
      .client
      .remove_strict_blocker(channel, carol_user, carol_device_id)
      .await
      .unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob]).await;

    for peer in [&mut alice, &mut bob] {
      let readiness = peer.client.strict_readiness(channel).unwrap();
      assert!(readiness.is_ready, "{}: {readiness:?}", peer.name);
    }
  }

  /// Only a workspace owner or admin asks (D3); a member's ask, posted by a modified client, is
  /// answered by nobody.
  #[tokio::test]
  async fn only_a_workspace_admins_ask_is_answered() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, channel) = channel_on_old_apps(&server).await;
    let result = bob.client.ask_strict_ready(channel).await;
    assert!(
      matches!(result, Err(CoreError::StrictNotNow(id, _)) if id == channel),
      "{result:?}"
    );
    let ask = teamcraft_crypto::mls::StrictAsk::new(
      crypto_conversation(channel),
      crypto_user(bob.client.user_id()),
      &mut rand_core::OsRng,
    );
    let mut payload = b"TCSA".to_vec();
    payload.extend_from_slice(&ask.to_bytes().unwrap());
    post_proposal(&bob, channel, payload).await;
    run_until_quiet(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert!(
      alice
        .client
        .strict_readiness(channel)
        .unwrap()
        .maybe_ask
        .is_none()
    );
    assert_eq!(answers(&alice, channel).await, 0);
  }

  /// A device that doesn't implement strict channels (an app from before) never says it is ready.
  #[tokio::test]
  async fn an_app_that_does_not_announce_strict_channels_never_answers() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, channel) = channel_on_old_apps(&server).await;
    alice.client.ask_strict_ready(channel).await.unwrap();
    test_hooks::set_legacy_leaves(true);
    run_until_quiet(&mut [&mut bob, &mut carol]).await;
    test_hooks::set_legacy_leaves(false);
    assert_eq!(answers(&alice, channel).await, 1, "only Alice's own answer");
    settle(&mut [&mut alice]).await;
    assert_eq!(alice.client.strict_readiness(channel).unwrap().waiting.len(), 2);
  }

  /// Three peers whose apps didn't announce strict channels when the channel was made, all
  /// updated now; Alice owns the workspace.
  pub(super) async fn channel_on_old_apps(server: &FakeServer) -> (Peer, Peer, Peer, ConversationId) {
    test_hooks::set_legacy_leaves(true);
    let (mut alice, mut bob, mut carol) = three_with_owner_settings(server, quick()).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    test_hooks::set_legacy_leaves(false);
    (alice, bob, carol, channel)
  }

  async fn answers(peer: &Peer, channel: ConversationId) -> usize {
    peer
      .api
      .list_mls_messages(channel, &ListMlsMessagesQuery::default())
      .await
      .unwrap()
      .messages
      .iter()
      .filter(|entry| entry.payload.as_slice().starts_with(b"TCSY"))
      .count()
  }
}

/// What the UI shows (issue #1897): the core's stored view of a channel's strict mode.
mod view {
  use super::*;
  use teamcraft_core::client::StrictStep;

  #[tokio::test]
  async fn the_ui_walks_a_channel_from_asking_to_strict() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, channel) = super::opting_in::channel_on_old_apps(&server).await;
    let alice_id = alice.client.user_id();

    alice
      .client
      .run_strict_step(channel, StrictStep::Refresh)
      .await
      .unwrap();
    let view = alice.client.strict_view(channel).unwrap().unwrap();
    assert!(view.has_group && !view.is_strict);
    assert_eq!(view.workspace_signers, vec![alice_id]);
    let readiness = view.maybe_readiness.unwrap();
    assert_eq!(readiness.waiting.len(), 3);
    assert!(!readiness.is_ready && readiness.maybe_asked_by.is_none());

    alice
      .client
      .run_strict_step(channel, StrictStep::AskReady)
      .await
      .unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob, &mut carol]).await;
    let readiness = alice
      .client
      .strict_view(channel)
      .unwrap()
      .unwrap()
      .maybe_readiness
      .unwrap();
    assert!(readiness.is_ready, "{readiness:?}");
    assert_eq!(readiness.maybe_asked_by, Some(alice_id));

    alice
      .client
      .run_strict_step(channel, StrictStep::TurnOn)
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let view = alice.client.strict_view(channel).unwrap().unwrap();
    assert!(view.is_strict && view.maybe_readiness.is_none());
    assert!(
      view
        .roles
        .iter()
        .any(|role| role.user == alice_id && role.is_admin),
      "{view:?}"
    );
    // Bob never opened the settings: his app shows the channel strict all the same.
    assert!(bob.client.strict_view(channel).unwrap().unwrap().is_strict);
  }

  #[tokio::test]
  async fn a_stop_shows_on_every_member_and_ends_with_the_admins_continue() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let good_epoch = epoch(&mut alice, channel);
    post_garbage_commit(&bob, channel, good_epoch).await;
    settle(&mut [&mut alice, &mut carol]).await;

    for peer in [&mut alice, &mut carol] {
      let view = peer.client.strict_view(channel).unwrap().unwrap();
      assert!(view.maybe_stop.is_some(), "{}: {view:?}", peer.name);
    }

    alice
      .client
      .run_strict_step(channel, StrictStep::Continue { maybe_remove: None })
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut carol]).await;
    for peer in [&mut alice, &mut carol] {
      let view = peer.client.strict_view(channel).unwrap().unwrap();
      assert_eq!(view.maybe_stop, None, "{}", peer.name);
    }
  }

  #[tokio::test]
  async fn a_plain_members_step_fails_and_the_view_stays() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;

    let result = bob.client.run_strict_step(channel, StrictStep::AskReady).await;

    assert!(result.is_err(), "only a workspace owner or admin asks (D3)");
    let view = bob.client.strict_view(channel).unwrap().unwrap();
    assert!(!view.is_strict);
    assert!(view.maybe_readiness.unwrap().maybe_asked_by.is_none());
  }

  #[tokio::test]
  async fn a_channel_that_was_never_strict_gets_no_view_unasked() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    carol.client.send_text(channel, None, "hello").await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    assert_eq!(bob.client.strict_view(channel).unwrap(), None);
  }
}

/// A workspace owner or admin outside the group signs a strict channel's roles (e2ee.md §4.6.2,
/// "Role changes while strict"): from the server's hint, posted for a member to commit.
mod outside_signer {
  use super::*;

  #[tokio::test]
  async fn a_workspace_admin_outside_the_group_changes_the_roles() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut dave) = three_and_an_outside_admin(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    assert!(!dave.client.has_group(channel).unwrap());
    let alice_id = alice.client.user_id();
    let bob_id = bob.client.user_id();
    let hint = dave
      .api
      .mls_strict_hint(channel, &MlsStrictHintQuery::default())
      .await
      .unwrap();
    assert_eq!(
      hint
        .maybe_strict
        .as_ref()
        .map(|hint| (hint.is_strict, hint.version)),
      Some((true, 1))
    );

    dave
      .client
      .set_strict_roles(
        channel,
        &[
          (alice_id, StrictRoleKind::Admin),
          (bob_id, StrictRoleKind::Moderator),
        ],
      )
      .await
      .unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      let roles = peer.client.strict_roles(channel).unwrap().unwrap();
      assert_eq!(roles.version(), 2, "{}", peer.name);
      assert_eq!(
        roles.role_of(crypto_user(bob_id)).map(|role| role.role),
        Some(StrictRoleKind::Moderator)
      );
    }
    let hint = dave
      .api
      .mls_strict_hint(channel, &MlsStrictHintQuery::default())
      .await
      .unwrap();
    assert_eq!(hint.maybe_strict.map(|hint| hint.version), Some(2));
  }

  /// A member lowered the hint (any member's commit sets it): the outside admin's turning on again
  /// signs version 1, below the "off" of version 2. Every member still takes the new roles into
  /// its account data: they merge by the epoch they were seen in, not by version.
  #[tokio::test]
  async fn turning_on_again_on_a_lowered_hint_reaches_every_members_account_data() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut dave) = three_and_an_outside_admin(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let alice_id = alice.client.user_id();
    alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    server.set_strict_hint(
      channel,
      Some(teamcraft_proto::StrictChannelHint {
        is_strict: false,
        version: 0,
        hash: Base64Bytes::new(vec![0; 32]),
        maybe_change: None,
      }),
    );

    dave
      .client
      .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    run_until_quiet(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    for peer in [&mut alice, &mut bob, &mut carol] {
      assert_eq!(
        peer
          .client
          .strict_roles(channel)
          .unwrap()
          .map(|roles| roles.version()),
        Some(1),
        "{}",
        peer.name
      );
      let known = peer
        .client
        .account_data()
        .known_strict_roles(channel)
        .cloned()
        .unwrap();
      assert!(
        known.maybe_roles.is_some(),
        "{}: the \"off\" of version 2 hides nothing",
        peer.name
      );
    }
  }

  /// A member outside the workspace roles signs nothing from outside (D3).
  #[tokio::test]
  async fn a_non_admin_outside_the_group_signs_nothing() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, _dave) = three_and_an_outside_admin(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    erin.client.full_sync().await.unwrap();
    let erin_id = erin.client.user_id();
    let result = erin
      .client
      .set_strict_roles(channel, &[(erin_id, StrictRoleKind::Admin)])
      .await;
    assert!(result.is_err(), "{result:?}");
  }

  /// Alice owns the workspace, Dave is a workspace admin with no device in the channel.
  async fn three_and_an_outside_admin(server: &FakeServer) -> (Peer, Peer, Peer, Peer) {
    let mut alice = Peer::enroll_with_settings(server, "Alice", quick()).await;
    let mut bob = Peer::enroll_with_settings(server, "Bob", quick()).await;
    let mut carol = Peer::enroll_with_settings(server, "Carol", quick()).await;
    let mut dave = Peer::enroll_with_settings(server, "Dave", quick()).await;
    let alice_id = alice.client.user_id();
    let dave_id = dave.client.user_id();
    alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (dave_id, RosterRole::Admin)])
      .await
      .unwrap();
    server.set_admin(dave_id);
    for peer in [&mut bob, &mut carol, &mut dave] {
      peer.client.full_sync().await.unwrap();
    }
    (alice, bob, carol, dave)
  }
}

/// The server's hint (e2ee.md §4.6.2): told with the roles commit, never decided on, and the
/// server keeps a strict channel private.
mod server_hint {
  use super::*;
  use teamcraft_proto::{ConversationVisibility, UpdateConversationRequest};

  #[tokio::test]
  async fn the_committer_tells_the_server_and_it_keeps_the_channel_private() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = strict_channel(&mut alice, &mut bob, &mut carol).await;
    let listed = bob.api.conversation(channel).await.unwrap();
    let hint = listed.maybe_strict.unwrap();
    assert!(hint.is_strict);
    assert_eq!(hint.version, 1);
    let roles = bob.client.strict_roles(channel).unwrap().unwrap();
    assert_eq!(hint.hash.as_slice(), roles.hash().unwrap().as_bytes());

    // Past the app's own refusal: the server refuses too.
    let public = UpdateConversationRequest {
      maybe_visibility: Some(ConversationVisibility::Public),
      ..UpdateConversationRequest::default()
    };
    assert!(alice.api.update_conversation(channel, &public).await.is_err());

    alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let hint = bob.api.conversation(channel).await.unwrap().maybe_strict.unwrap();
    assert!(!hint.is_strict);
    assert_eq!(hint.version, 2);
    assert!(alice.api.update_conversation(channel, &public).await.is_ok());
  }
}

/// Key package stock replacement (e2ee.md §4.6.2, "Older clients"): a device whose packages start
/// announcing strict channels replaces its whole stock once.
mod key_packages {
  use super::*;
  use teamcraft_crypto::mls::test_hooks;

  #[tokio::test]
  async fn a_device_replaces_its_stock_once_its_packages_announce_strict_channels() {
    let server = FakeServer::new();
    test_hooks::set_legacy_leaves(true);
    let mut alice = Peer::enroll(&server, "Alice").await;
    test_hooks::set_legacy_leaves(false);
    let device = alice.client.device_id();
    assert!(server.stock_replacements().is_empty());

    alice.client.top_up_key_packages().await.unwrap();
    assert_eq!(server.stock_replacements(), vec![device]);
    assert_eq!(server.key_package_count(device), 3);

    alice.client.top_up_key_packages().await.unwrap();
    assert_eq!(server.stock_replacements(), vec![device], "only once");
  }

  /// A build that doesn't announce strict channels keeps its stock as before.
  #[tokio::test]
  async fn an_app_that_does_not_announce_them_keeps_its_stock() {
    let server = FakeServer::new();
    test_hooks::set_legacy_leaves(true);
    let mut alice = Peer::enroll(&server, "Alice").await;
    alice.client.top_up_key_packages().await.unwrap();
    test_hooks::set_legacy_leaves(false);
    assert!(server.stock_replacements().is_empty());
  }
}

/// A strict channel's renames and moves (e2ee.md §4.6.5, D5): its admins sign them as
/// `channel_admin`; its plain members don't sign them into the anchor at all.
mod renames {
  use super::*;
  use teamcraft_core::anchoring::{ANCHORING_RELEASE, NameRole};
  use teamcraft_core::client::channel_anchor_marks;
  use teamcraft_proto::UpdateConversationRequest;

  #[tokio::test]
  async fn a_strict_admins_rename_is_named_as_the_channels_admin() {
    let server = FakeServer::new();
    let release = ANCHORING_RELEASE;
    server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
    let (mut alice, mut bob, mut carol) = three_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    let bob_id = bob.client.user_id();
    alice
      .client
      .set_strict_roles(channel, &[(bob_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;

    rename(&mut bob, channel, "announcements").await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let marks = channel_anchor_marks(&*carol.store, channel).unwrap().unwrap();
    assert_eq!(marks.maybe_name_set_by, Some((bob_id, NameRole::ChannelAdmin)));

    // Carol is a plain member of the strict channel: the server takes her rename, the anchor
    // doesn't, and the name is stale.
    let epoch_before = epoch(&mut carol, channel);
    rename(&mut carol, channel, "old-test").await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    assert_eq!(epoch(&mut alice, channel), epoch_before, "nothing was committed");
    let marks = channel_anchor_marks(&*alice.store, channel).unwrap().unwrap();
    assert_eq!(marks.maybe_name_set_by, Some((bob_id, NameRole::ChannelAdmin)));
  }

  async fn rename(peer: &mut Peer, channel: ConversationId, name: &str) {
    peer
      .client
      .update_channel(
        channel,
        &UpdateConversationRequest {
          maybe_name: Some(name.to_owned()),
          ..UpdateConversationRequest::default()
        },
      )
      .await
      .unwrap();
  }
}

/// A modified client's commits (e2ee.md §4.6.2, finding 3): Bob's client skips the rule before it
/// commits (`Client::commit_unchecked_change`, a test hook), and the server takes a commit from any
/// member. Every honest member refuses it alike and stops; an admin continues.
mod modified_client {
  use super::*;
  use teamcraft_crypto::membership::MembershipAction;
  use teamcraft_proto::ChannelRole;

  #[tokio::test]
  async fn a_plain_members_signed_removal_is_refused_alike_and_an_admin_removes_them_and_continues() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut dave) = four_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol, &mut dave]).await;
    strict(&mut alice, channel).await;
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    let good_epoch = epoch(&mut alice, channel);
    let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());

    // Bob, a plain member, signs Carol's removal and commits it past his own app's refusal.
    remove_unchecked(&mut bob, channel, carol_id).await;
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;

    let mut stops = Vec::new();
    for peer in [&mut alice, &mut carol, &mut dave] {
      let stop = peer.client.strict_stop(channel).unwrap();
      let stop = stop.unwrap_or_else(|| panic!("{} didn't stop", peer.name));
      assert_eq!(stop.epoch, good_epoch, "{}", peer.name);
      assert_eq!(stop.maybe_signer, Some(bob_id), "{}", peer.name);
      assert_eq!(epoch(peer, channel), good_epoch, "{}", peer.name);
      assert!(
        peer.client.group_users(channel).unwrap().contains(&carol_id),
        "{}: Carol stays in the group",
        peer.name
      );
      stops.push(stop.commit_id);
    }
    stops.dedup();
    assert_eq!(stops.len(), 1, "every member stopped at the same commit");
    assert_eq!(rejoin_requests(&alice, channel).await, 0);
    assert_eq!(
      epoch(&mut bob, channel),
      good_epoch + 1,
      "Bob's client follows its own commit"
    );

    // "Remove Bob and continue", through the server's rewind.
    alice
      .client
      .continue_strict_channel(channel, Some(bob_id))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    assert_eq!(server.rewinds(), vec![(channel, good_epoch)]);
    for peer in [&mut alice, &mut carol, &mut dave] {
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None, "{}", peer.name);
      assert_eq!(epoch(peer, channel), good_epoch + 1, "{}", peer.name);
      let users = peer.client.group_users(channel).unwrap();
      assert!(
        !users.contains(&bob_id) && users.contains(&carol_id),
        "{}: {users:?}",
        peer.name
      );
    }
    carol.client.send_text(channel, None, "after").await.unwrap();
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;
    for peer in [&alice, &dave] {
      assert_eq!(peer.texts(channel).last().map(String::as_str), Some("after"));
    }
  }

  /// A plain member's signed addition of an outsider: refused alike, so nobody shares keys with
  /// the person Bob let in, and "Continue" without removing anybody ends the stop.
  #[tokio::test]
  async fn a_plain_members_signed_addition_is_refused_alike() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut erin) = four_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol]).await;
    strict(&mut alice, channel).await;
    settle(&mut [&mut alice, &mut bob, &mut carol]).await;
    let good_epoch = epoch(&mut alice, channel);
    let erin_id = erin.client.user_id();
    // The server lists Erin (it takes a member's word, finding 3); nobody signed her in.
    server.list_member_unsigned(channel, erin_id, ChannelRole::Member);

    bob
      .client
      .commit_unchecked_change(
        channel,
        MembershipAction::AddUser {
          user_id: crypto_user(erin_id),
          history: teamcraft_crypto::membership::HistoryShare::FromJoin,
        },
      )
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut carol, &mut erin]).await;

    for peer in [&mut alice, &mut carol] {
      assert!(
        peer.client.strict_stop(channel).unwrap().is_some(),
        "{}",
        peer.name
      );
      assert!(!peer.client.group_users(channel).unwrap().contains(&erin_id));
    }

    alice.client.continue_strict_channel(channel, None).await.unwrap();
    settle(&mut [&mut alice, &mut carol, &mut erin]).await;
    for peer in [&mut alice, &mut carol] {
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None, "{}", peer.name);
      assert_eq!(epoch(peer, channel), good_epoch + 1, "{}", peer.name);
      assert!(!peer.client.group_users(channel).unwrap().contains(&erin_id));
    }
  }

  /// Race and rewind convergence (§4.6.2, "Epoch conflicts and convergence"): two admins of the
  /// context continue from the same stop at once, one with Bob's removal and one without. The
  /// first rewind wins; the second must not drop it, or the two admins' groups part.
  #[tokio::test]
  async fn two_admins_continuing_at_once_end_in_one_group() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut dave) = four_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol, &mut dave]).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    alice
      .client
      .set_strict_roles(
        channel,
        &[
          (alice_id, StrictRoleKind::Admin),
          (carol_id, StrictRoleKind::Admin),
        ],
      )
      .await
      .unwrap();
    server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    let good_epoch = epoch(&mut alice, channel);
    remove_unchecked(&mut bob, channel, dave.client.user_id()).await;
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;
    assert!(alice.client.strict_stop(channel).unwrap().is_some());
    assert!(carol.client.strict_stop(channel).unwrap().is_some());

    // Alice continues first; Carol, who hasn't seen it yet, a moment later.
    alice
      .client
      .continue_strict_channel(channel, Some(bob_id))
      .await
      .unwrap();
    let result = carol.client.continue_strict_channel(channel, None).await;
    assert!(result.is_ok(), "the second admin catches up: {result:?}");
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    assert_eq!(server.rewinds(), vec![(channel, good_epoch)], "one rewind");
    let alice_epoch = epoch(&mut alice, channel);
    for peer in [&mut alice, &mut carol, &mut dave] {
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None, "{}", peer.name);
      assert_eq!(epoch(peer, channel), alice_epoch, "{}", peer.name);
      assert!(
        !peer.client.group_users(channel).unwrap().contains(&bob_id),
        "{}",
        peer.name
      );
    }
    // One group: each reads the other's message.
    alice.client.send_text(channel, None, "from alice").await.unwrap();
    carol.client.send_text(channel, None, "from carol").await.unwrap();
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;
    assert!(carol.texts(channel).contains(&"from alice".to_owned()));
    assert!(alice.texts(channel).contains(&"from carol".to_owned()));
    assert!(dave.texts(channel).contains(&"from carol".to_owned()));
  }

  /// Both admins remove Bob at once: the second's removal is already done once it catches up.
  #[tokio::test]
  async fn two_admins_removing_the_committer_at_once_end_in_one_group() {
    let server = FakeServer::new();
    let (mut alice, mut bob, mut carol, mut dave) = four_with_owner(&server).await;
    let channel = channel_with(&mut alice, &mut [&mut bob, &mut carol, &mut dave]).await;
    let (alice_id, bob_id, carol_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      carol.client.user_id(),
    );
    alice
      .client
      .set_strict_roles(
        channel,
        &[
          (alice_id, StrictRoleKind::Admin),
          (carol_id, StrictRoleKind::Admin),
        ],
      )
      .await
      .unwrap();
    server.set_channel_role_unsigned(channel, carol_id, ChannelRole::Admin);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
    remove_unchecked(&mut bob, channel, dave.client.user_id()).await;
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;

    alice
      .client
      .continue_strict_channel(channel, Some(bob_id))
      .await
      .unwrap();
    carol
      .client
      .continue_strict_channel(channel, Some(bob_id))
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

    assert_eq!(server.rewinds().len(), 1, "{:?}", server.rewinds());
    let alice_epoch = epoch(&mut alice, channel);
    for peer in [&mut carol, &mut dave] {
      assert_eq!(epoch(peer, channel), alice_epoch, "{}", peer.name);
      assert_eq!(peer.client.strict_stop(channel).unwrap(), None, "{}", peer.name);
    }
    carol.client.send_text(channel, None, "one group").await.unwrap();
    settle(&mut [&mut alice, &mut carol, &mut dave]).await;
    assert_eq!(alice.texts(channel).last().map(String::as_str), Some("one group"));
  }

  async fn remove_unchecked(peer: &mut Peer, channel: ConversationId, user: teamcraft_proto::UserId) {
    peer
      .client
      .commit_unchecked_change(
        channel,
        MembershipAction::RemoveUser {
          user_id: crypto_user(user),
        },
      )
      .await
      .unwrap();
  }
}

/// Falling out of a strict channel and coming back (e2ee.md §4.6.2, "Falling out in a strict
/// channel", and the table's withheld roster): Carol's server withholds the roster that makes Dave
/// a workspace admin, so she can't judge his roles change, reads the roster, still can't and asks
/// to be added back. An admin's welcome is opened at once; a plain member's is held until the
/// user decides after `strict_rejoin_wait`. Before judging a welcome she carries the roles she
/// knew forward along the server's chain of signed changes (issue #2251): a verified "off" or an
/// admin appointed meanwhile brings her back at once; a chain she can't verify changes nothing.
mod coming_back {
  use super::*;
  use std::sync::Arc;
  use teamcraft_core::clock::ManualClock;
  use teamcraft_core::store::{
    AwayChange, ConversationEvent, LocalStore as _, MessageContent, MessageFilter,
  };
  use teamcraft_crypto::strict_roles::{
    SignedStrictRolesChange, StrictHash, StrictRolesChange, StrictSignerRole,
  };
  use teamcraft_proto::StrictChainChange;

  /// Strict was turned off, honestly, while Carol's only device was out: the chain ends in a
  /// verified "off", so a plain member's welcome without the roles opens at once instead of
  /// waiting `strict_rejoin_wait` for a decision she can't judge.
  #[tokio::test]
  async fn an_honest_off_while_the_device_was_away_opens_a_members_welcome() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    // Alice, the channel's admin, turns strict off while Carol is out.
    team.alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();

    // Only Bob, a plain member now, answers.
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
    assert!(!team.carol.client.is_strict(channel).unwrap());
    team.bob.client.send_text(channel, None, "back").await.unwrap();
    settle(&mut [&mut team.bob, &mut team.carol]).await;
    assert_eq!(team.carol.texts(channel).last().map(String::as_str), Some("back"));
  }

  /// Two role changes Carol missed hand the channel to Bob: his welcome is judged by the chain's
  /// end, where he is the admin, not by the roles she knew, where he was nobody.
  #[tokio::test]
  async fn after_missed_role_changes_the_new_admins_welcome_opens() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice_hands_the_channel_to_bob(channel).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
    let roles = team.carol.client.strict_roles(channel).unwrap().unwrap();
    assert_eq!(roles.version(), 4);
    team
      .bob
      .client
      .send_text(channel, None, "from the new admin")
      .await
      .unwrap();
    settle(&mut [&mut team.bob, &mut team.carol]).await;
    assert_eq!(
      team.carol.texts(channel).last().map(String::as_str),
      Some("from the new admin")
    );
  }

  /// Dave's link of the chain is signed as a workspace admin of a roster Carol's server still
  /// withholds: "unknown", so nothing is carried forward and Bob's welcome is held, as without a
  /// chain. Nothing is refused for it: the known roles stay, and the next welcome reads the chain
  /// again (`strict_roles::tests::chain` has the verdict).
  #[tokio::test]
  async fn a_workspace_signer_the_device_cannot_verify_keeps_the_hold() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice_hands_the_channel_to_bob(channel).await;
    team.carol_comes_online(channel).await;

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert_eq!(prompt.hold.sender, team.bob.client.user_id());
    assert!(!prompt.is_due);
    assert_eq!(
      known_roles(&team.carol, channel).version(),
      Some(1),
      "nothing carried forward"
    );
  }

  /// A hostile server leaves out a link: the chain has a gap and is refused as a whole.
  #[tokio::test]
  async fn a_gapped_chain_keeps_the_hold() {
    held_on_a_hostile_chain(|_, honest| honest.into_iter().filter(|link| link.version != 3).collect()).await;
  }

  /// A hostile server shows a link Bob signed himself, as if he were the admin: he is a moderator
  /// of the roles before it, so the link is refused.
  #[tokio::test]
  async fn a_forged_link_keeps_the_hold() {
    held_on_a_hostile_chain(|team, honest| {
      let forged = team.change_signed_by_bob_after(&honest[1]);
      vec![honest[1].clone(), forged]
    })
    .await;
  }

  /// A hostile server shows another version 3, signed by Alice but never committed, before the
  /// real version 4: version 4 doesn't continue it, so the fork is refused.
  #[tokio::test]
  async fn a_forked_chain_keeps_the_hold() {
    held_on_a_hostile_chain(|team, honest| {
      let fork = team.change_signed_by_alice_after(&honest[1], &honest[3]);
      vec![honest[1].clone(), fork, honest[3].clone()]
    })
    .await;
  }

  /// Another version 3, signed by Alice but never committed, beside the real one at the chain's
  /// end: nothing later tells which one the group took, so the chain is refused.
  #[tokio::test]
  async fn a_chain_forked_at_its_end_keeps_the_hold() {
    held_on_a_hostile_chain(|team, honest| {
      let fork = team.change_signed_by_alice_after(&honest[1], &honest[3]);
      vec![honest[1].clone(), honest[2].clone(), fork]
    })
    .await;
  }

  /// Bob was an admin and is demoted at version 4. He stays a member, and his own commits carry
  /// changes he signed on from version 3, where he still was one: a branch one version longer than
  /// the real chain, which an honest server keeps like any (shown here as the server would). The
  /// fork at version 4 refuses the chain: his welcome is judged by the roles Carol knew, and held.
  #[tokio::test]
  async fn a_demoted_admins_longer_branch_keeps_the_hold() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    let (alice_id, bob_id) = (team.alice.client.user_id(), team.bob.client.user_id());
    team
      .alice
      .client
      .set_strict_roles(
        channel,
        &[(alice_id, StrictRoleKind::Admin), (bob_id, StrictRoleKind::Admin)],
      )
      .await
      .unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    team
      .alice
      .client
      .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();
    let mut shown = server.strict_chain(channel);
    let both_admins = parse(&shown[2]).change().roles.clone();
    let branch_fourth = sign_after(&team.bob, &shown[2], both_admins.clone());
    let branch_fifth = sign_after(&team.bob, &branch_fourth, both_admins);
    shown.extend([branch_fourth, branch_fifth]);
    shown.sort_by_key(|link| (link.version, link.commit));
    server.show_strict_chain(channel, Some(shown));

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert_eq!(prompt.hold.sender, bob_id);
    assert_ne!(
      epoch_or_zero(&mut team.carol, channel),
      epoch(&mut team.bob, channel)
    );
  }

  /// Links naming more signers than `max_strict_chain_signers` leave the chain unused: the hold
  /// stays, and the device read nobody's device list beyond that.
  #[tokio::test]
  async fn a_chain_naming_too_many_signers_keeps_the_hold() {
    let settings = teamcraft_core::client::ClientSettings {
      max_strict_chain_signers: 1,
      ..quick()
    };
    let (_server, mut team, channel) = Team::on_a_long_chain(settings).await;
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;
    assert!(team.carol.client.strict_rejoin_prompt(channel).unwrap().is_some());
  }

  /// A member's junk under the next version, beside the real change (the server keeps every
  /// commit's): passed over, and the new admin's welcome still opens.
  #[tokio::test]
  async fn junk_beside_the_real_change_does_not_hold_the_welcome() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice_hands_the_channel_to_bob(channel).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();
    let mut shown = server.strict_chain(channel);
    for link in shown.clone() {
      shown.push(StrictChainChange {
        change: Base64Bytes::new(b"junk".to_vec()),
        commit: commit_after(link.commit, 1),
        ..link
      });
    }
    shown.sort_by_key(|link| (link.version, link.commit));
    server.show_strict_chain(channel, Some(shown));

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
  }

  /// Strict off and on again while Carol is out: Alice's turning on signs the version after her
  /// "off" (`next_strict_chain_version`), so the chain runs on through it, and the new admin's
  /// welcome opens.
  #[tokio::test]
  async fn off_and_on_again_while_away_and_the_new_admins_welcome_opens() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    let bob_id = team.bob.client.user_id();
    team.alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    team
      .alice
      .client
      .set_strict_roles(channel, &[(bob_id, StrictRoleKind::Admin)])
      .await
      .unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    assert_eq!(
      team.bob.client.strict_roles(channel).unwrap().unwrap().version(),
      4,
      "turned on again after the \"off\" of version 3"
    );
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
    assert_eq!(
      team
        .carol
        .client
        .strict_roles(channel)
        .unwrap()
        .unwrap()
        .version(),
      4
    );
  }

  /// The server cuts the chain right after an "off" while its hint says strict is on: Carol's
  /// welcome without the roles is held, not opened.
  #[tokio::test]
  async fn an_off_the_hint_contradicts_keeps_the_hold() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice.client.set_strict_roles(channel, &[]).await.unwrap();
    settle(&mut [&mut team.alice, &mut team.bob, &mut team.dave]).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();
    let mut hint = team
      .bob
      .api
      .conversation(channel)
      .await
      .unwrap()
      .maybe_strict
      .unwrap();
    hint.is_strict = true;
    hint.version += 1;
    server.set_strict_hint(channel, Some(hint));

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert_eq!(prompt.hold.sender, team.bob.client.user_id());
  }

  /// A chain longer than a page is read page by page (Alice's changes, signed but shown by the
  /// server only, hand the channel to Bob at the 70th); one longer than
  /// `max_strict_chain_changes`, or a server that never finishes or cuts it, leaves the hold.
  #[tokio::test]
  async fn a_long_chain_is_read_page_by_page() {
    let (_server, mut team, channel) = Team::on_a_long_chain(quick_answers()).await;
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;
    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
  }

  #[tokio::test]
  async fn a_chain_longer_than_the_setting_keeps_the_hold() {
    let settings = teamcraft_core::client::ClientSettings {
      max_strict_chain_changes: 10,
      ..quick()
    };
    let (_server, mut team, channel) = Team::on_a_long_chain(settings).await;
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;
    assert!(team.carol.client.strict_rejoin_prompt(channel).unwrap().is_some());
  }

  #[tokio::test]
  async fn a_chain_whose_pages_never_end_or_end_early_keeps_the_hold() {
    use teamcraft_core::api::fake::StrictChainPaging;
    for paging in [StrictChainPaging::Stuck, StrictChainPaging::EndsEarly] {
      let (server, mut team, channel) = Team::on_a_long_chain(quick_answers()).await;
      server.page_strict_chain(paging);
      run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;
      assert!(
        team.carol.client.strict_rejoin_prompt(channel).unwrap().is_some(),
        "{paging:?}"
      );
    }
  }

  /// The chain's read is lost on the way: no verdict, so no hold; the device asks to be added back
  /// again, as after any failed read while opening a welcome.
  #[tokio::test]
  async fn a_lost_chain_read_asks_again_instead_of_holding() {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice_hands_the_channel_to_bob(channel).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();
    let requests_before = rejoin_requests(&team.bob, channel).await;
    server.fail_strict_chain_reads(1);

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert!(!team.carol.client.has_group(channel).unwrap());
    assert!(rejoin_requests(&team.bob, channel).await > requests_before);
  }

  /// Carol misses Dave's change and Alice handing the channel to Bob, and only Bob answers her
  /// request; her roster is honest again,
  /// but the server shows her `tamper` of the chain (versions 1 to 4) instead: Bob's welcome is
  /// held, and the roles she knew stay as they were.
  async fn held_on_a_hostile_chain(
    tamper: impl FnOnce(&Team, Vec<StrictChainChange>) -> Vec<StrictChainChange>,
  ) {
    let server = FakeServer::new();
    let mut team = Team::with_carol_clock(&server, Arc::new(ManualClock::new(1_800_000_000_000))).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_while_carol_is_away(channel).await;
    team.alice_hands_the_channel_to_bob(channel).await;
    team.carol_comes_online(channel).await;
    team.show_carol_the_roster();
    let honest = server.strict_chain(channel);
    assert_eq!(
      honest.iter().map(|link| link.version).collect::<Vec<_>>(),
      vec![1, 2, 3, 4]
    );
    let mut shown = tamper(&team, honest);
    shown.sort_by_key(|link| (link.version, link.commit));
    server.show_strict_chain(channel, Some(shown));

    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert_eq!(prompt.hold.sender, team.bob.client.user_id());
    assert_eq!(known_roles(&team.carol, channel).version(), Some(1));
    assert_ne!(
      epoch_or_zero(&mut team.carol, channel),
      epoch(&mut team.bob, channel)
    );
  }

  #[tokio::test]
  async fn a_withheld_roster_makes_a_device_fall_out_and_an_admins_welcome_brings_it_back() {
    let server = FakeServer::new();
    let mut team = Team::new(&server, quick()).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_carol_cannot_judge(channel).await;

    // Carol is behind, not stopped, and asked to be added back.
    assert_eq!(team.carol.client.strict_stop(channel).unwrap(), None);
    assert_ne!(
      epoch_or_zero(&mut team.carol, channel),
      epoch(&mut team.alice, channel)
    );
    assert!(rejoin_requests(&team.alice, channel).await >= 1);

    // While Carol is out, Alice removes Bob; then she answers. Her welcome, an admin's of the roles
    // Carol knew, is opened at once.
    let bob_id = team.bob.client.user_id();
    team.alice.client.remove_member(channel, bob_id).await.unwrap();
    run_until_quiet(&mut [&mut team.alice, &mut team.carol]).await;
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.alice, channel));
    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    // A line tells Carol who is gone since her last tree; nobody is new.
    assert_eq!(away_lines(&team.carol, channel), vec![(bob_id, AwayChange::Gone)]);
    let roles = team.carol.client.strict_roles(channel).unwrap().unwrap();
    assert_eq!(
      roles.version(),
      2,
      "the roles she couldn't judge come with the welcome"
    );
    team
      .alice
      .client
      .send_text(channel, None, "welcome back")
      .await
      .unwrap();
    settle(&mut [&mut team.alice, &mut team.carol]).await;
    assert_eq!(
      team.carol.texts(channel).last().map(String::as_str),
      Some("welcome back")
    );
  }

  #[tokio::test]
  async fn a_plain_members_welcome_is_held_until_the_user_takes_it_after_the_wait() {
    let server = FakeServer::new();
    let carol_clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let mut team = Team::with_carol_clock(&server, carol_clock.clone()).await;
    let channel = team.strict_channel().await;
    team.dave_changes_the_roles_carol_cannot_judge(channel).await;
    let bob_id = team.bob.client.user_id();

    // Only Bob is online; his app waited `strict_rejoin_wait` already (his settings).
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;
    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert_eq!(prompt.hold.sender, bob_id);
    assert!(!prompt.is_due, "the wait for an admin isn't over");
    assert!(
      prompt.hold.joined.is_empty() && prompt.hold.gone.is_empty(),
      "{prompt:?}"
    );
    assert!(team.carol.client.accept_strict_rejoin(channel).await.is_err());

    carol_clock.advance(i64::try_from(quick().strict_rejoin_wait.as_millis()).unwrap());
    let prompt = team.carol.client.strict_rejoin_prompt(channel).unwrap().unwrap();
    assert!(prompt.is_due);
    // Members answer a device again only after `rejoin_cooldown` (issue #149), by the server's time.
    server.advance_clock(quick().rejoin_cooldown + std::time::Duration::from_secs(60));
    team.carol.client.accept_strict_rejoin(channel).await.unwrap();
    run_until_quiet(&mut [&mut team.bob, &mut team.carol]).await;

    assert_eq!(team.carol.client.strict_rejoin_prompt(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.bob, channel));
    assert!(
      away_lines(&team.carol, channel).is_empty(),
      "the same people as before"
    );
    team
      .bob
      .client
      .send_text(channel, None, "on your word")
      .await
      .unwrap();
    settle(&mut [&mut team.bob, &mut team.carol]).await;
    assert_eq!(
      team.carol.texts(channel).last().map(String::as_str),
      Some("on your word")
    );
  }

  /// The roster that made Dave an admin reached Carol's server but not yet her device, and Dave
  /// removes Bob as a workspace admin: Carol reads the roster before she judges the commit, not
  /// after (processing it spends the epoch's secret), and follows the group.
  #[tokio::test]
  async fn a_device_whose_roster_lags_reads_it_before_judging_a_workspace_admins_removal() {
    let server = FakeServer::new();
    let mut team = Team::new(&server, quick()).await;
    let channel = team.strict_channel().await;
    let server_roster = team.alice.client.workspace().unwrap().roster.version;
    let (alice_id, bob_id, carol_id, dave_id) = (
      team.alice.client.user_id(),
      team.bob.client.user_id(),
      team.carol.client.user_id(),
      team.dave.client.user_id(),
    );
    server.withhold_roster(carol_id, Some(server_roster));
    team
      .alice
      .client
      .publish_roster(&[(alice_id, RosterRole::Owner), (dave_id, RosterRole::Admin)])
      .await
      .unwrap();
    team.settle().await;
    // Honest again, but Carol's device hasn't read the roster since.
    server.withhold_roster(carol_id, None);
    team.dave.client.full_sync().await.unwrap();

    team
      .dave
      .client
      .admin_remove_member(channel, bob_id)
      .await
      .unwrap();
    run_until_quiet(&mut [&mut team.alice, &mut team.carol, &mut team.dave]).await;

    assert_eq!(team.carol.client.strict_stop(channel).unwrap(), None);
    assert_eq!(epoch(&mut team.carol, channel), epoch(&mut team.alice, channel));
    assert!(!team.carol.client.group_users(channel).unwrap().contains(&bob_id));
  }

  /// Alice owns the workspace and is the channel's admin; Dave becomes a workspace admin in a
  /// roster Carol's server never shows her. Bob is a plain member.
  struct Team {
    alice: Peer,
    bob: Peer,
    carol: Peer,
    dave: Peer,
  }

  impl Team {
    async fn new(server: &FakeServer, settings: teamcraft_core::client::ClientSettings) -> Self {
      let carol = Peer::enroll_with_settings(server, "Carol", settings.clone()).await;
      Self::with_carol(server, settings, carol).await
    }

    async fn with_carol_clock(server: &FakeServer, clock: Arc<ManualClock>) -> Self {
      let carol = Peer::enroll_on_clock(server.add_user("Carol"), "Carol", quick(), Box::new(clock)).await;
      // Bob's app has waited `strict_rejoin_wait` already: he answers a request at once.
      let settings = teamcraft_core::client::ClientSettings {
        strict_rejoin_wait: std::time::Duration::ZERO,
        ..quick()
      };
      Self::with_carol(server, settings, carol).await
    }

    async fn with_carol(
      server: &FakeServer,
      settings: teamcraft_core::client::ClientSettings,
      carol: Peer,
    ) -> Self {
      let alice = Peer::enroll_with_settings(server, "Alice", quick()).await;
      let bob = Peer::enroll_with_settings(server, "Bob", settings).await;
      let dave = Peer::enroll_with_settings(server, "Dave", quick()).await;
      let mut team = Self {
        alice,
        bob,
        carol,
        dave,
      };
      let alice_id = team.alice.client.user_id();
      team
        .alice
        .client
        .publish_roster(&[(alice_id, RosterRole::Owner)])
        .await
        .unwrap();
      for peer in [&mut team.bob, &mut team.carol, &mut team.dave] {
        peer.client.full_sync().await.unwrap();
      }
      team
    }

    async fn strict_channel(&mut self) -> ConversationId {
      let channel = channel_with(
        &mut self.alice,
        &mut [&mut self.bob, &mut self.carol, &mut self.dave],
      )
      .await;
      strict(&mut self.alice, channel).await;
      self.settle().await;
      channel
    }

    /// The roster making Dave an admin is withheld from Carol; Dave signs the roles as a workspace
    /// admin of it (Bob a moderator), and everybody but Carol takes it.
    async fn dave_changes_the_roles_carol_cannot_judge(&mut self, channel: ConversationId) {
      self.dave_changes_the_roles_while_carol_is_away(channel).await;
      settle(&mut [&mut self.carol]).await;
    }

    /// Like [`Self::dave_changes_the_roles_carol_cannot_judge`], but Carol's device doesn't look
    /// yet: it falls out only at the next [`Self::carol_comes_online`], once whatever else happens
    /// meanwhile is in the queue, so nobody but who runs then has seen her rejoin request.
    async fn dave_changes_the_roles_while_carol_is_away(&mut self, channel: ConversationId) {
      let server_roster = self.alice.client.workspace().unwrap().roster.version;
      let (alice_id, bob_id, dave_id) = (
        self.alice.client.user_id(),
        self.bob.client.user_id(),
        self.dave.client.user_id(),
      );
      self
        .carol
        .api
        .server()
        .withhold_roster(self.carol.client.user_id(), Some(server_roster));
      self
        .alice
        .client
        .publish_roster(&[(alice_id, RosterRole::Owner), (dave_id, RosterRole::Admin)])
        .await
        .unwrap();
      self.settle().await;
      self.dave.client.full_sync().await.unwrap();
      self
        .dave
        .client
        .set_strict_roles(
          channel,
          &[
            (alice_id, StrictRoleKind::Admin),
            (bob_id, StrictRoleKind::Moderator),
          ],
        )
        .await
        .unwrap();
      settle(&mut [&mut self.alice, &mut self.bob, &mut self.dave]).await;
    }

    /// Carol's device reads the queue: it can't judge Dave's change, so it falls out and asks to
    /// be added back.
    async fn carol_comes_online(&mut self, channel: ConversationId) {
      settle(&mut [&mut self.carol]).await;
      assert!(
        rejoin_requests(&self.bob, channel).await >= 1,
        "Carol asked to be added back"
      );
      assert_eq!(self.carol.client.strict_stop(channel).unwrap(), None);
    }

    /// While Carol is out, Alice (the admin) makes Bob an admin too, then hands the channel to him:
    /// roles versions 3 and 4, signed by her as the channel's admin.
    async fn alice_hands_the_channel_to_bob(&mut self, channel: ConversationId) {
      let (alice_id, bob_id) = (self.alice.client.user_id(), self.bob.client.user_id());
      self
        .alice
        .client
        .set_strict_roles(
          channel,
          &[(alice_id, StrictRoleKind::Admin), (bob_id, StrictRoleKind::Admin)],
        )
        .await
        .unwrap();
      settle(&mut [&mut self.alice, &mut self.bob, &mut self.dave]).await;
      self
        .alice
        .client
        .set_strict_roles(channel, &[(bob_id, StrictRoleKind::Admin)])
        .await
        .unwrap();
      settle(&mut [&mut self.alice, &mut self.bob, &mut self.dave]).await;
    }

    /// Carol (with `carol_settings`) knows the channel strict at version 1 and falls out at Dave's
    /// change; the server shows her a chain of 70 changes after it, the real version 2 and Alice's
    /// signed (never committed) changes up to version 71, the last making Bob an admin. Only Bob
    /// answers her request.
    async fn on_a_long_chain(
      carol_settings: teamcraft_core::client::ClientSettings,
    ) -> (FakeServer, Self, ConversationId) {
      let server = FakeServer::new();
      let clock = Arc::new(ManualClock::new(1_800_000_000_000));
      let carol =
        Peer::enroll_on_clock(server.add_user("Carol"), "Carol", carol_settings, Box::new(clock)).await;
      let mut team = Self::with_carol(&server, quick_answers(), carol).await;
      let channel = team.strict_channel().await;
      team.dave_changes_the_roles_while_carol_is_away(channel).await;
      team.carol_comes_online(channel).await;
      team.show_carol_the_roster();
      let mut shown = server.strict_chain(channel);
      let alice_admin = vec![role_of(&team.alice, StrictRoleKind::Admin)];
      let mut both = vec![
        role_of(&team.alice, StrictRoleKind::Admin),
        role_of(&team.bob, StrictRoleKind::Admin),
      ];
      both.sort_by_key(|role| role.user_id);
      for step in 0..69 {
        let roles = if step == 68 {
          both.clone()
        } else {
          alice_admin.clone()
        };
        let previous = shown.last().unwrap().clone();
        shown.push(sign_after(&team.alice, &previous, roles));
      }
      assert_eq!(shown.last().unwrap().version, 71);
      server.show_strict_chain(channel, Some(shown));
      (server, team, channel)
    }

    /// Carol's server shows her the roster again: she could judge Dave's change now.
    fn show_carol_the_roster(&self) {
      self
        .carol
        .api
        .server()
        .withhold_roster(self.carol.client.user_id(), None);
    }

    /// A version 3 after `previous` that makes Bob the admin, signed by Bob as if he were one.
    fn change_signed_by_bob_after(&self, previous: &StrictChainChange) -> StrictChainChange {
      let roles = vec![role_of(&self.bob, StrictRoleKind::Admin)];
      sign_after(&self.bob, previous, roles)
    }

    /// A version 3 after `previous`, signed by Alice, its admin, that was never committed: the
    /// roles of `other` (Bob alone the admin), so it differs from the real version 3.
    fn change_signed_by_alice_after(
      &self,
      previous: &StrictChainChange,
      other: &StrictChainChange,
    ) -> StrictChainChange {
      let roles = parse(other).change().roles.clone();
      sign_after(&self.alice, previous, roles)
    }

    async fn settle(&mut self) {
      settle(&mut [&mut self.alice, &mut self.bob, &mut self.carol, &mut self.dave]).await;
    }
  }

  /// A roles change signed by `signer` as the channel's admin, continuing `previous`, unchecked.
  fn sign_after(
    signer: &Peer,
    previous: &StrictChainChange,
    roles: Vec<teamcraft_crypto::strict_roles::StrictRole>,
  ) -> StrictChainChange {
    let previous_commit = previous.commit;
    let previous = parse(previous);
    let change = StrictRolesChange {
      conversation_id: previous.change().conversation_id,
      version: previous.change().version + 1,
      prev_hash: previous.change().hash().unwrap(),
      signed_for_epoch: previous.change().signed_for_epoch,
      signer_role: StrictSignerRole::ChannelAdmin,
      roster_version: 0,
      role_log_version: 0,
      role_log_hash: StrictHash::ZERO,
      roles,
    };
    let signed = signer.client.sign_strict_roles_unchecked(change).unwrap();
    StrictChainChange {
      version: signed.change().version,
      commit: commit_after(previous_commit, 1),
      change: Base64Bytes::new(signed.to_bytes().unwrap()),
    }
  }

  /// A commit ID `by` after `commit`, for links a hostile server makes up.
  fn commit_after(commit: teamcraft_proto::MlsMessageId, by: i64) -> teamcraft_proto::MlsMessageId {
    teamcraft_proto::MlsMessageId::from_snowflake(
      teamcraft_proto::Snowflake::new(commit.snowflake().get() + by).unwrap(),
    )
  }

  /// Settings of members who answer a rejoin request at once (their app waited already).
  fn quick_answers() -> teamcraft_core::client::ClientSettings {
    teamcraft_core::client::ClientSettings {
      strict_rejoin_wait: std::time::Duration::ZERO,
      ..quick()
    }
  }

  fn parse(link: &StrictChainChange) -> SignedStrictRolesChange {
    SignedStrictRolesChange::from_bytes(link.change.as_slice()).unwrap()
  }

  /// `peer`'s role with the identity key its device is certified by.
  fn role_of(peer: &Peer, role: StrictRoleKind) -> teamcraft_crypto::strict_roles::StrictRole {
    teamcraft_crypto::strict_roles::StrictRole {
      user_id: crypto_user(peer.client.user_id()),
      role,
      identity_key: teamcraft_crypto::identity::IdentityPublicKey::from_bytes(
        peer.client.identity_public_key_bytes(),
      ),
    }
  }

  fn known_roles(peer: &Peer, channel: ConversationId) -> teamcraft_core::account_data::KnownStrictRoles {
    peer
      .client
      .account_data()
      .known_strict_roles(channel)
      .cloned()
      .unwrap()
  }

  /// The lines the device shows for who is new or gone since its last tree.
  fn away_lines(peer: &Peer, channel: ConversationId) -> Vec<(teamcraft_proto::UserId, AwayChange)> {
    peer
      .store
      .messages(channel, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter(|message| !message.is_hidden())
      .filter_map(|message| match message.content {
        MessageContent::Event {
          event: ConversationEvent::ChangedWhileAway { user, change },
        } => Some((user, change)),
        _ => None,
      })
      .collect()
  }

  fn epoch_or_zero(peer: &mut Peer, channel: ConversationId) -> u64 {
    peer.client.mls_epoch(channel).unwrap().unwrap_or(0)
  }
}

/// Lets every client process its gateway frames and run its pending steps (a strict channel's
/// answers, leaf refreshes and delegated commits retry after an epoch was taken) until nothing
/// moves any more.
async fn run_until_quiet(peers: &mut [&mut Peer]) {
  for _ in 0..30 {
    settle(peers).await;
    for peer in peers.iter_mut() {
      peer.client.apply_pending_changes().await.unwrap();
    }
    let mut handled = 0;
    for peer in peers.iter_mut() {
      handled += peer.client.pump(&mut peer.gateway).await.unwrap();
    }
    if handled == 0 {
      return;
    }
  }
  panic!("clients did not get quiet");
}

/// Client settings for flows ranked by `device_add_stagger`: nobody waits on the manual clock.
fn quick() -> teamcraft_core::client::ClientSettings {
  teamcraft_core::client::ClientSettings {
    device_add_stagger: std::time::Duration::ZERO,
    ..common::settings()
  }
}

/// Posts a raw proposal into the group's queue, as a modified client could.
async fn post_proposal(peer: &Peer, channel: ConversationId, payload: Vec<u8>) {
  let epoch = peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .current_epoch;
  let request = teamcraft_proto::PostMlsMessageRequest {
    device_id: peer.client.device_id(),
    epoch,
    kind: MlsMessageKind::Proposal,
    payload: Base64Bytes::new(payload),
  };
  peer.api.post_mls_message(channel, &request).await.unwrap();
}

fn crypto_conversation(id: ConversationId) -> teamcraft_crypto::ids::ConversationId {
  teamcraft_crypto::ids::ConversationId::new(u64::try_from(id.snowflake().get()).unwrap())
}

/// Alice owns the workspace (a signed roster), so she may turn channels strict (D3).
async fn three_with_owner(server: &FakeServer) -> (Peer, Peer, Peer) {
  three_with_owner_settings(server, common::settings()).await
}

/// [`three_with_owner`] and Dave, a fourth member without a workspace role.
async fn four_with_owner(server: &FakeServer) -> (Peer, Peer, Peer, Peer) {
  let (alice, bob, carol) = three_with_owner(server).await;
  let mut dave = Peer::enroll(server, "Dave").await;
  dave.client.full_sync().await.unwrap();
  (alice, bob, carol, dave)
}

/// [`three_with_owner`] with other client settings.
async fn three_with_owner_settings(
  server: &FakeServer,
  settings: teamcraft_core::client::ClientSettings,
) -> (Peer, Peer, Peer) {
  let mut alice = Peer::enroll_with_settings(server, "Alice", settings.clone()).await;
  let mut bob = Peer::enroll_with_settings(server, "Bob", settings.clone()).await;
  let mut carol = Peer::enroll_with_settings(server, "Carol", settings).await;
  finish_three_with_owner(&mut alice, &mut bob, &mut carol).await;
  (alice, bob, carol)
}

async fn finish_three_with_owner(alice: &mut Peer, bob: &mut Peer, carol: &mut Peer) {
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  bob.client.full_sync().await.unwrap();
  carol.client.full_sync().await.unwrap();
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

/// A channel of Alice's with Bob and Carol, strict with Alice as its admin.
async fn strict_channel(alice: &mut Peer, bob: &mut Peer, carol: &mut Peer) -> ConversationId {
  let channel = channel_with(alice, &mut [&mut *bob, &mut *carol]).await;
  strict(alice, channel).await;
  settle(&mut [alice, bob, carol]).await;
  channel
}

async fn strict(alice: &mut Peer, channel: ConversationId) {
  let alice_id = alice.client.user_id();
  alice
    .client
    .set_strict_roles(channel, &[(alice_id, StrictRoleKind::Admin)])
    .await
    .unwrap();
}

/// A commit for `epoch` that no member can read, posted the way a modified client would.
async fn post_garbage_commit(
  peer: &Peer,
  channel: ConversationId,
  epoch: u64,
) -> teamcraft_proto::MlsMessageId {
  let request = PostMlsCommitRequest {
    maybe_strict: None,
    maybe_replaces: None,
    device_id: peer.client.device_id(),
    epoch,
    commit: Base64Bytes::new(vec![0x00, 0x01, 0x00, 0x03, 0xde, 0xad, 0xbe, 0xef]),
    welcomes: Vec::new(),
    maybe_group_info: None,
  };
  peer
    .api
    .post_mls_commit(channel, &request)
    .await
    .unwrap()
    .commit
    .id
}

async fn rejoin_requests(peer: &Peer, channel: ConversationId) -> usize {
  peer
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal)
    .count()
}

fn crypto_user(id: teamcraft_proto::UserId) -> teamcraft_crypto::ids::UserId {
  teamcraft_crypto::ids::UserId::new(u64::try_from(id.snowflake().get()).unwrap())
}

fn epoch(peer: &mut Peer, channel: ConversationId) -> u64 {
  peer.client.mls_epoch(channel).unwrap().unwrap()
}
