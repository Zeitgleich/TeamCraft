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
  Base64Bytes, ConversationId, ListMlsMessagesQuery, MlsMessageKind, PostMlsCommitRequest,
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

/// Alice owns the workspace (a signed roster), so she may turn channels strict (D3).
async fn three_with_owner(server: &FakeServer) -> (Peer, Peer, Peer) {
  let mut alice = Peer::enroll(server, "Alice").await;
  let mut bob = Peer::enroll(server, "Bob").await;
  let mut carol = Peer::enroll(server, "Carol").await;
  let alice_id = alice.client.user_id();
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner)])
    .await
    .unwrap();
  bob.client.full_sync().await.unwrap();
  carol.client.full_sync().await.unwrap();
  (alice, bob, carol)
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
