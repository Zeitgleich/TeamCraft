//! Admins letting admins into private channels (admin.md §2.4, issue #816): an admin who isn't a
//! member gets an admin in, themselves or another one, only as the signed `admin_self_join` says.
//! Members add admins like anybody else, and admins outside still add everybody else (§2.2).
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_core::store::{ConversationEvent, LocalStore, MessageContent, MessageFilter};
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{
  AdminSelfJoin, ConversationId, ListMlsMessagesQuery, MlsMessageKind, UserId, WorkspacePolicy,
};

use crate::common::{Peer, settle};

/// A line as members read it: who joined, and who let them in ("approved by" or "added by",
/// tests/suite/admin_joins.rs has the wording).
type Line = (UserId, Option<UserId>);

/// The values members decide on, the default first.
fn policies() -> [AdminSelfJoin; 3] {
  [
    AdminSelfJoin::Visible,
    AdminSelfJoin::OwnerApproval,
    AdminSelfJoin::Disabled,
  ]
}

/// The issue: Bob, an admin outside the channel, signs Erin's addition. The (fake) server adds
/// her, as the real one does (it checks only an admin's own join); the members' apps decide.
mod an_admin_outside_adds_an_admin {
  use super::*;

  #[tokio::test]
  async fn nobody_commits_it_when_admins_may_not_join() {
    for policy in [AdminSelfJoin::Disabled, AdminSelfJoin::OwnerApproval] {
      let mut team = Team::new(policy.clone()).await;
      let epoch = team.carol.client.mls_epoch(team.channel).unwrap();
      let erin = team.erin.client.user_id();

      team
        .bob
        .client
        .admin_add_member(team.channel, erin)
        .await
        .unwrap();
      team.settle().await;

      assert!(
        !team.erin.client.has_group(team.channel).unwrap(),
        "{policy:?}: let in"
      );
      for peer in [&mut team.carol, &mut team.dave] {
        assert_eq!(
          peer.client.mls_epoch(team.channel).unwrap(),
          epoch,
          "{policy:?}: {} committed it",
          peer.name
        );
        assert!(lines(peer, team.channel).is_empty(), "{policy:?}: {}", peer.name);
      }
    }
  }

  #[tokio::test]
  async fn the_members_commit_it_when_admins_may_join() {
    let mut team = Team::new(AdminSelfJoin::Visible).await;
    let (bob, erin) = (team.bob.client.user_id(), team.erin.client.user_id());

    team
      .bob
      .client
      .admin_add_member(team.channel, erin)
      .await
      .unwrap();
    team.settle().await;

    assert!(team.erin.client.has_group(team.channel).unwrap());
    for peer in [&team.carol, &team.dave] {
      assert_eq!(
        lines(peer, team.channel),
        vec![(erin, Some(bob))],
        "{}",
        peer.name
      );
    }
  }

  /// An open channel is anybody's to join: the policy is about private ones.
  #[tokio::test]
  async fn an_open_channel_takes_it_whatever_the_policy() {
    let mut team = Team::new(AdminSelfJoin::Disabled).await;
    let (dave, erin) = (team.dave.client.user_id(), team.erin.client.user_id());
    let space = team.carol.space().await;
    let public = team
      .carol
      .client
      .create_channel_with(
        space,
        ChannelOptions {
          name: "open".into(),
          is_private: false,
          maybe_topic: None,
          maybe_category: None,
          maybe_history: None,
          is_integration: false,
          maybe_idempotency_key: None,
        },
      )
      .await
      .unwrap();
    team.carol.client.add_member(public, dave).await.unwrap();
    team.settle().await;

    team.bob.client.admin_add_member(public, erin).await.unwrap();
    team.settle().await;

    assert!(team.erin.client.has_group(public).unwrap());
  }
}

/// A member's addition of an admin is an ordinary one under every policy but `owner_approval`: a
/// member may add people. Under `owner_approval` it takes a signed channel admin (issue #380,
/// e2ee.md §4.6.1): Carol, who made the channel, is one; Alice and Dave are plain members.
mod a_member_adds_an_admin {
  use super::*;

  /// Alice, an owner of the workspace, was added by Carol and signs Erin's addition as a member.
  #[tokio::test]
  async fn an_admin_who_is_a_member() {
    for policy in policies() {
      let mut team = Team::new(policy.clone()).await;
      let (alice, carol, erin) = (
        team.alice.client.user_id(),
        team.carol.client.user_id(),
        team.erin.client.user_id(),
      );
      team.carol.client.add_member(team.channel, alice).await.unwrap();
      team.settle().await;
      assert!(
        team.alice.client.has_group(team.channel).unwrap(),
        "{policy:?}: Carol let Alice in"
      );

      let outcome = team.alice.client.admin_add_member(team.channel, erin).await;
      team.settle().await;

      if policy == AdminSelfJoin::OwnerApproval {
        // Alice is no signed channel admin: nobody commits the addition of the admin Erin.
        let _ = outcome;
        assert!(
          !team.erin.client.has_group(team.channel).unwrap(),
          "{policy:?}: members refuse an unsigned owner's addition"
        );
        assert_eq!(lines(&team.dave, team.channel), vec![(alice, Some(carol))]);
        continue;
      }
      outcome.unwrap();
      assert!(
        team.erin.client.has_group(team.channel).unwrap(),
        "{policy:?}: Alice let Erin in"
      );
      assert_eq!(
        lines(&team.dave, team.channel),
        vec![(alice, Some(carol)), (erin, Some(alice))],
        "{policy:?}"
      );
    }
  }

  /// Dave isn't an admin: his signed addition of Erin, committed by whichever member comes first.
  #[tokio::test]
  async fn a_member_who_is_no_admin() {
    for policy in policies() {
      let mut team = Team::new(policy.clone()).await;
      let (dave, erin) = (team.dave.client.user_id(), team.erin.client.user_id());

      let outcome = team.dave.client.admin_add_member(team.channel, erin).await;
      team.settle().await;

      if policy == AdminSelfJoin::OwnerApproval {
        // Dave is no signed channel admin (issue #380).
        let _ = outcome;
        assert!(
          !team.erin.client.has_group(team.channel).unwrap(),
          "{policy:?}: members refuse a plain member's addition of an admin"
        );
        continue;
      }
      outcome.unwrap();
      assert!(
        team.erin.client.has_group(team.channel).unwrap(),
        "{policy:?}: Dave let Erin in"
      );
      assert_eq!(
        lines(&team.carol, team.channel),
        vec![(erin, Some(dave))],
        "{policy:?}"
      );
    }
  }

  /// The way in under `owner_approval` (admin.md §2.4.1): Erin asks, Carol approves with her own
  /// commit.
  #[tokio::test]
  async fn the_owner_who_approves() {
    let mut team = Team::new(AdminSelfJoin::OwnerApproval).await;
    let (carol, erin) = (team.carol.client.user_id(), team.erin.client.user_id());
    team.erin.client.request_admin_join(team.channel).await.unwrap();
    team.settle().await;

    team
      .carol
      .client
      .approve_admin_join(team.channel, erin)
      .await
      .unwrap();
    team.settle().await;

    assert!(team.erin.client.has_group(team.channel).unwrap());
    assert_eq!(lines(&team.dave, team.channel), vec![(erin, Some(carol))]);
  }
}

/// What the fix leaves as it was.
mod unchanged {
  use super::*;

  /// Admin changes about people who aren't admins (admin.md §2.2) go through under every policy.
  #[tokio::test]
  async fn an_admin_outside_adds_somebody_else() {
    for policy in policies() {
      let mut team = Team::new(policy.clone()).await;
      let mut fay = Peer::enroll(&team.server, "Fay").await;
      let fay_id = fay.client.user_id();

      team
        .bob
        .client
        .admin_add_member(team.channel, fay_id)
        .await
        .unwrap();
      team.settle_with(&mut fay).await;

      assert!(fay.client.has_group(team.channel).unwrap(), "{policy:?}");
      assert!(
        lines(&team.carol, team.channel).is_empty(),
        "{policy:?}: no admin"
      );
    }
  }

  /// An admin's own self-join follows the policy as before.
  #[tokio::test]
  async fn an_admins_own_self_join() {
    for (policy, may_join) in [
      (AdminSelfJoin::Visible, true),
      (AdminSelfJoin::OwnerApproval, false),
      (AdminSelfJoin::Disabled, false),
    ] {
      let mut team = Team::new(policy.clone()).await;
      let erin = team.erin.client.user_id();

      team
        .erin
        .client
        .admin_add_member(team.channel, erin)
        .await
        .unwrap();
      team.settle().await;

      assert_eq!(
        team.erin.client.has_group(team.channel).unwrap(),
        may_join,
        "{policy:?}"
      );
      let expected: Vec<Line> = if may_join { vec![(erin, None)] } else { Vec::new() };
      assert_eq!(lines(&team.carol, team.channel), expected, "{policy:?}");
    }
  }
}

/// Members on clients before v1.0.9 still commit an admin's addition of another admin, whatever
/// the policy. This version refuses that commit like any it can't verify: the member falls out of
/// the group and asks to be added back (e2ee.md §4.6, "Falling out and coming back"), and a member
/// adds it back into the group that has the admin in it. The group doesn't split; the policy holds
/// only once no member runs an older version (admin.md §2.4).
mod with_members_on_older_versions {
  use super::*;

  #[tokio::test]
  async fn an_older_member_lets_the_admin_in_and_this_version_is_added_back() {
    let mut team = Team::new(AdminSelfJoin::Disabled).await;
    let (bob, erin) = (team.bob.client.user_id(), team.erin.client.user_id());
    let dave_device = team.dave.client.device_id();
    team.carol.client.check_membership_as_before_v1_0_9();

    team
      .bob
      .client
      .admin_add_member(team.channel, erin)
      .await
      .unwrap();
    team.settle().await;

    // Carol's client committed it and Erin is in. Dave's refused to merge it: the next entry of
    // the later epoch showed him that he is behind, he asked to be added back, and a member did.
    assert!(team.erin.client.has_group(team.channel).unwrap());
    assert_eq!(rejoin_requests(&team.carol, &team.dave, team.channel).await, 1);
    assert_eq!(lines(&team.carol, team.channel), vec![(erin, Some(bob))]);
    let epoch = team.carol.client.mls_epoch(team.channel).unwrap();
    for peer in [&mut team.dave, &mut team.erin] {
      assert_eq!(
        peer.client.mls_epoch(team.channel).unwrap(),
        epoch,
        "{}: one group",
        peer.name
      );
    }
    assert!(
      team
        .carol
        .client
        .group_device_ids(team.channel)
        .unwrap()
        .contains(&dave_device)
    );

    team
      .erin
      .client
      .send_text(team.channel, None, "hello")
      .await
      .unwrap();
    team
      .dave
      .client
      .send_text(team.channel, None, "hi")
      .await
      .unwrap();
    team.settle().await;
    assert_eq!(team.dave.texts(team.channel), ["hello", "hi"]);
    assert_eq!(team.erin.texts(team.channel), ["hello", "hi"]);
    // Dave never merged the commit that let Erin in, so his device has no line about it: the
    // welcome that added him back shows no join.
    assert!(lines(&team.dave, team.channel).is_empty());
  }

  /// The other way round nothing changes: this version commits nothing an older one refuses.
  #[tokio::test]
  async fn an_older_member_takes_what_this_version_commits() {
    let mut team = Team::new(AdminSelfJoin::OwnerApproval).await;
    let (carol, erin) = (team.carol.client.user_id(), team.erin.client.user_id());
    team.dave.client.check_membership_as_before_v1_0_9();
    let dave_device = team.dave.client.device_id();
    team.erin.client.request_admin_join(team.channel).await.unwrap();
    team.settle().await;

    team
      .carol
      .client
      .approve_admin_join(team.channel, erin)
      .await
      .unwrap();
    team.settle().await;

    assert!(team.erin.client.has_group(team.channel).unwrap());
    assert_eq!(
      team.dave.client.mls_epoch(team.channel).unwrap(),
      team.carol.client.mls_epoch(team.channel).unwrap()
    );
    assert!(
      team
        .carol
        .client
        .group_device_ids(team.channel)
        .unwrap()
        .contains(&dave_device)
    );
    assert_eq!(lines(&team.dave, team.channel), vec![(erin, Some(carol))]);
  }
}

/// The lines `peer` lists in `channel`, oldest first. Each one stems from a commit.
fn lines(peer: &Peer, channel: ConversationId) -> Vec<Line> {
  peer
    .store
    .messages(channel, MessageFilter::TopLevel)
    .unwrap()
    .into_iter()
    .filter_map(|message| match message.content {
      MessageContent::Event {
        event: ConversationEvent::AdminJoined { user, is_approved },
      } if message.maybe_verified_commit.is_some() => {
        // The line's author is who let the admin in; the admin's own for a self-join.
        let by = message.sender_user_id;
        Some((user, (is_approved || by != user).then_some(by)))
      },
      _ => None,
    })
    .collect()
}

/// How many requests to be added back `sender`'s device put into the channel's queue: its only
/// proposals here.
async fn rejoin_requests(reader: &Peer, sender: &Peer, channel: ConversationId) -> usize {
  let device = sender.client.device_id();
  reader
    .api
    .list_mls_messages(channel, &ListMlsMessagesQuery::default())
    .await
    .unwrap()
    .messages
    .iter()
    .filter(|entry| entry.kind == MlsMessageKind::Proposal && entry.sender_device_id == device)
    .count()
}

/// Alice owns the workspace and signs Bob and Erin as admins; Carol founds (owns) a private
/// channel and adds Dave, who is no admin. Neither Alice, Bob nor Erin is in the channel.
struct Team {
  server: FakeServer,
  alice: Peer,
  bob: Peer,
  carol: Peer,
  dave: Peer,
  erin: Peer,
  channel: ConversationId,
}

impl Team {
  async fn new(admin_self_join: AdminSelfJoin) -> Self {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut dave = Peer::enroll(&server, "Dave").await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    let (alice_id, bob_id, erin_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      erin.client.user_id(),
    );
    alice
      .client
      .publish_roster(&[
        (alice_id, RosterRole::Owner),
        (bob_id, RosterRole::Admin),
        (erin_id, RosterRole::Admin),
      ])
      .await
      .unwrap();
    alice
      .client
      .publish_policy(WorkspacePolicy {
        admin_self_join,
        ..WorkspacePolicy::default()
      })
      .await
      .unwrap();
    server.set_admin(bob_id);
    server.set_admin(erin_id);
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin]).await;
    let channel = carol
      .client
      .create_channel(carol.space().await, "private")
      .await
      .unwrap();
    carol
      .client
      .add_member(channel, dave.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin]).await;
    Self {
      server,
      alice,
      bob,
      carol,
      dave,
      erin,
      channel,
    }
  }

  async fn settle(&mut self) {
    settle(&mut [
      &mut self.alice,
      &mut self.bob,
      &mut self.carol,
      &mut self.dave,
      &mut self.erin,
    ])
    .await;
  }

  /// Like [`Self::settle`], with somebody from outside the team.
  async fn settle_with(&mut self, other: &mut Peer) {
    settle(&mut [
      &mut self.alice,
      &mut self.bob,
      &mut self.carol,
      &mut self.dave,
      &mut self.erin,
      other,
    ])
    .await;
  }
}
