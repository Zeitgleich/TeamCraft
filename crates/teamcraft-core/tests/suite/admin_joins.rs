//! The "joined this channel as a workspace admin" line (admin.md §2.4, issue #811): every member's
//! device derives it from the verified MLS commit that let the admin in, never from a message.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

#[cfg(feature = "ui")]
use teamcraft_client_api::actions::WorkspaceEvent;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::workspace_admin::ChannelOptions;
use teamcraft_core::records::{self, Draft, OutboxEntry};
use teamcraft_core::store::{
  ConversationEvent, LocalStore, MessageContent, MessageFilter, StoredMessage, Table,
};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{CoreError, JoinRequestRefusal};
use teamcraft_proto::{
  AdminSelfJoin, ClientMessageId, ConversationId, HistoryPolicy, UserId, WorkspacePolicy,
};

use crate::common::{self, Peer, settle};

/// A line as members read it: who joined, and who let them in.
type Line = (UserId, By);

/// Who let the admin in, as the line says it.
#[derive(Debug, PartialEq, Eq)]
enum By {
  /// Their own join: no name.
  Nobody,
  /// "approved by": a member while admins need an owner's approval.
  Approved(UserId),
  /// "added by": anybody else who signed the addition.
  Added(UserId),
}

#[tokio::test]
async fn members_see_an_admins_own_join_once_from_the_commit() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::Visible).await;
  let bob_id = bob.client.user_id();

  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(
    bob.client.has_group(channel).unwrap(),
    "the self-join went through"
  );
  // One of Carol's and Dave's devices committed it, the other one verified the commit.
  for peer in [&carol, &dave] {
    assert_eq!(lines(peer, channel), vec![(bob_id, By::Nobody)], "{}", peer.name);
    // Still sent for older versions; hidden here.
    assert_eq!(announcements(peer, channel), 1, "{}", peer.name);
  }
  // The admin's devices joined by the welcome: there was no commit for them to verify.
  assert!(lines(&bob, channel).is_empty());
  #[cfg(feature = "ui")]
  assert_eq!(
    shown(&dave, channel),
    vec![WorkspaceEvent::AdminJoined {
      user: bob_id,
      maybe_approved_by: None,
      maybe_added_by: None
    }]
  );
}

#[tokio::test]
async fn members_see_which_owner_let_the_admin_in() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(channel).unwrap());
  for peer in [&carol, &dave] {
    assert_eq!(
      lines(peer, channel),
      vec![(bob_id, By::Approved(carol_id))],
      "{}",
      peer.name
    );
  }
  assert!(lines(&bob, channel).is_empty());
  #[cfg(feature = "ui")]
  assert_eq!(
    shown(&dave, channel),
    vec![WorkspaceEvent::AdminJoined {
      user: bob_id,
      maybe_approved_by: Some(carol_id),
      maybe_added_by: None
    }]
  );
}

/// Review L3 of #797: a member who sends the message without any commit tells nobody anything.
#[tokio::test]
async fn a_made_up_admin_join_shows_nothing() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::Visible).await;
  let bob_id = bob.client.user_id();

  forge_admin_join(&mut dave, channel, bob_id).await;
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(!bob.client.has_group(channel).unwrap(), "nobody let him in");
  assert_eq!(announcements(&carol, channel), 1, "the message arrived");
  assert!(lines(&carol, channel).is_empty(), "and shows nothing");
  #[cfg(feature = "ui")]
  assert!(shown(&carol, channel).is_empty());

  // After a real join, another made-up message adds no second line.
  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  forge_admin_join(&mut dave, channel, bob_id).await;
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert_eq!(lines(&carol, channel), vec![(bob_id, By::Nobody)]);
}

/// A server that keeps the message from the members, or a committer that stopped before sending
/// it: the commit still tells them.
#[tokio::test]
async fn an_admin_join_whose_message_never_arrives_is_still_shown() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::Visible).await;
  let bob_id = bob.client.user_id();
  // Whoever of the members commits the self-join, their message goes nowhere.
  server.drop_messages_of(carol.client.user_id(), true);
  server.drop_messages_of(dave.client.user_id(), true);

  bob.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(channel).unwrap());
  for peer in [&carol, &dave] {
    assert_eq!(lines(peer, channel), vec![(bob_id, By::Nobody)], "{}", peer.name);
  }
  assert_eq!(
    announcements(&carol, channel) + announcements(&dave, channel),
    1,
    "only the committer's own copy of the message"
  );
}

/// Another admin's signed change brings an admin in (admin.md §2.2; under `visible` only since
/// #816): the line names the admin who signed it, not the member whose device committed it, and
/// says "added", not "approved".
#[tokio::test]
async fn another_admins_addition_names_who_signed_it() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::Visible).await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  server.set_admin(alice_id);

  alice.client.admin_add_member(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(channel).unwrap());
  assert!(!alice.client.has_group(channel).unwrap(), "she only signed it");
  for peer in [&carol, &dave] {
    assert_eq!(
      lines(peer, channel),
      vec![(bob_id, By::Added(alice_id))],
      "{}",
      peer.name
    );
  }
}

/// Direct messages and group DMs have no admin way in: adding an admin there is no admin's join.
#[tokio::test]
async fn no_line_in_direct_conversations() {
  let (_server, mut alice, mut bob, mut carol, mut dave, _channel) = setup(AdminSelfJoin::Visible).await;
  let (bob_id, dave_id) = (bob.client.user_id(), dave.client.user_id());

  let group = carol.client.open_group_dm(&[dave_id, bob_id]).await.unwrap();
  let direct = carol.client.open_dm(bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(group).unwrap() && bob.client.has_group(direct).unwrap());
  for peer in [&carol, &dave] {
    assert!(lines(peer, group).is_empty(), "{}: group DM", peer.name);
  }
  assert!(lines(&carol, direct).is_empty(), "DM");
}

/// A signer kept for a commit of another epoch (left behind by a crash) names nobody: the line
/// then says only that the admin joined.
#[tokio::test]
async fn a_signer_kept_for_another_commit_names_nobody() {
  #[derive(serde::Serialize)]
  struct Kept {
    epoch: u64,
    signer: UserId,
  }

  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  server.lose_next_commit_answer(carol_id);
  carol
    .client
    .approve_admin_join(channel, bob_id)
    .await
    .unwrap_err();
  let epoch = carol.client.mls_epoch(channel).unwrap().unwrap();

  // What a crashed earlier commit left (the store's own record of a pending commit's signer).
  records::put_json(
    &*carol.store,
    Table::AppliedChanges,
    &format!("pending_signer/{channel}"),
    &Kept {
      epoch: epoch + 7,
      signer: dave.client.user_id(),
    },
  )
  .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(channel).unwrap());
  assert_eq!(lines(&carol, channel), vec![(bob_id, By::Nobody)]);
  assert_eq!(
    lines(&dave, channel),
    vec![(bob_id, By::Approved(carol_id))],
    "verified by the others"
  );
}

/// The owner's commit went through but its answer was lost: the approval's message is never sent,
/// and the owner's device merges its commit from the queue. Everybody still sees who approved.
#[tokio::test]
async fn the_owner_is_named_when_the_commit_answer_was_lost() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  server.lose_next_commit_answer(carol_id);
  let error = carol
    .client
    .approve_admin_join(channel, bob_id)
    .await
    .unwrap_err();
  assert!(error.is_transient(), "{error}");
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert!(bob.client.has_group(channel).unwrap());
  for peer in [&carol, &dave] {
    assert_eq!(
      lines(peer, channel),
      vec![(bob_id, By::Approved(carol_id))],
      "{}",
      peer.name
    );
    assert_eq!(
      announcements(peer, channel),
      0,
      "{}: no message was sent",
      peer.name
    );
  }
}

/// The line's ID is known to everybody who saw the commit; still, no message deletes or changes
/// it, also not one from the person it names.
#[tokio::test]
async fn nobody_deletes_the_line() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  let line = line_message(&dave, channel);

  // Carol, the line's author, deletes it as if it were her message; Bob pins it.
  send(
    &mut carol,
    channel,
    MessageContent::Delete {
      target: line.client_message_id,
    },
  )
  .await;
  send(
    &mut bob,
    channel,
    MessageContent::Pin {
      target: line.client_message_id,
      is_pinned: true,
    },
  )
  .await;
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert_eq!(line_message(&dave, channel), line);
  assert_eq!(lines(&dave, channel), vec![(bob_id, By::Approved(carol_id))]);
}

/// A message sent under the line's ID doesn't take its place.
#[tokio::test]
async fn nobody_replaces_the_line() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  let line = line_message(&dave, channel);

  // Bob has no line (he joined by the welcome) and writes a text under its ID.
  enqueue_as(
    &bob,
    channel,
    line.client_message_id,
    MessageContent::Text {
      body: "nothing to see".into(),
      mentions: Vec::new(),
      meta: Default::default(),
    },
  );
  bob.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert_eq!(line_message(&dave, channel), line);
  assert_eq!(lines(&dave, channel), vec![(bob_id, By::Approved(carol_id))]);
  assert!(dave.texts(channel).is_empty());
}

/// A server that gives a message the commit's ID (which the line's row goes by) can't hide the
/// line behind that message.
#[cfg(feature = "ui")]
#[tokio::test]
async fn a_message_under_the_commits_id_hides_no_line() {
  use teamcraft_proto::{Id, MessageId};

  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  let commit = line_message(&dave, channel).maybe_verified_commit.unwrap();

  // Stored first among equals: the lowest client ID.
  server.force_next_message_id(MessageId::from_snowflake(commit.snowflake()));
  enqueue_as(
    &bob,
    channel,
    ClientMessageId::from_bytes([0; 16]),
    MessageContent::Text {
      body: "same place".into(),
      mentions: Vec::new(),
      meta: Default::default(),
    },
  );
  bob.client.flush_outbox().await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  assert_eq!(dave.texts(channel), vec!["same place"]);
  assert!(shown(&dave, channel).contains(&WorkspaceEvent::AdminJoined {
    user: bob_id,
    maybe_approved_by: Some(carol_id),
    maybe_added_by: None
  }));
}

/// Only admins of the verified roster, and only in channels not open to everybody; how they got in
/// doesn't matter: a member adding an admin is named like an owner who approved.
#[tokio::test]
async fn only_admins_joining_a_closed_channel_are_told() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::Disabled).await;
  let mut erin = Peer::enroll(&server, "Erin").await;
  let (bob_id, carol_id, erin_id) = (
    bob.client.user_id(),
    carol.client.user_id(),
    erin.client.user_id(),
  );
  // The server calls Erin an admin; the signed roster doesn't.
  server.set_admin(erin_id);

  carol.client.add_member(channel, erin_id).await.unwrap();
  carol.client.add_member(channel, bob_id).await.unwrap();
  let public = carol
    .client
    .create_channel_with(
      carol.space().await,
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
  carol
    .client
    .add_member(public, dave.client.user_id())
    .await
    .unwrap();
  carol.client.add_member(public, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave, &mut erin]).await;

  assert!(erin.client.has_group(channel).unwrap() && bob.client.has_group(public).unwrap());
  for peer in [&carol, &dave] {
    assert_eq!(
      lines(peer, channel),
      vec![(bob_id, By::Added(carol_id))],
      "{}",
      peer.name
    );
    assert!(lines(peer, public).is_empty(), "{}: an open channel", peer.name);
  }
}

/// Every device verifies the commit on its own: the approving owner's other device, and members
/// when the admin brings several devices at once, each show the line once.
#[tokio::test]
async fn every_device_shows_the_line_once() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  let mut carol_laptop = second_device(&mut carol).await;
  let mut bob_laptop = second_device(&mut bob).await;
  settle(&mut [
    &mut alice,
    &mut bob,
    &mut bob_laptop,
    &mut carol,
    &mut carol_laptop,
    &mut dave,
  ])
  .await;
  assert!(
    carol_laptop.client.has_group(channel).unwrap(),
    "her laptop is in the channel"
  );

  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [
    &mut alice,
    &mut bob,
    &mut bob_laptop,
    &mut carol,
    &mut carol_laptop,
    &mut dave,
  ])
  .await;
  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [
    &mut alice,
    &mut bob,
    &mut bob_laptop,
    &mut carol,
    &mut carol_laptop,
    &mut dave,
  ])
  .await;

  assert!(bob.client.has_group(channel).unwrap() && bob_laptop.client.has_group(channel).unwrap());
  for peer in [&carol, &carol_laptop, &dave] {
    assert_eq!(
      lines(peer, channel),
      vec![(bob_id, By::Approved(carol_id))],
      "{}",
      peer.name
    );
  }
  for peer in [&bob, &bob_laptop] {
    assert!(lines(peer, channel).is_empty(), "{}", peer.name);
  }
}

/// Approving with or without the earlier history tells the members the same.
#[tokio::test]
async fn the_shared_history_changes_nothing_about_the_line() {
  let (_server, mut alice, mut bob, mut carol, mut dave, channel) =
    setup_with(AdminSelfJoin::OwnerApproval, HistoryPolicy::Full).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  carol.client.send_text(channel, None, "earlier").await.unwrap();
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  carol
    .client
    .approve_admin_join_sharing(channel, bob_id, HistoryPolicy::Full)
    .await
    .unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  bob.client.load_older_messages(channel).await.unwrap();

  assert_eq!(bob.texts(channel), vec!["earlier"], "the owner shared it");
  assert_eq!(lines(&dave, channel), vec![(bob_id, By::Approved(carol_id))]);
}

/// Issue #380: members take in an admin only from a signed channel admin, so an owner the server
/// alone lists can't approve; once a signed admin confirms the role, approving works and every
/// member takes the add in.
#[tokio::test]
async fn an_owner_only_the_server_lists_cannot_approve_until_a_signed_admin_confirms() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) = setup(AdminSelfJoin::OwnerApproval).await;
  let (bob_id, dave_id) = (bob.client.user_id(), dave.client.user_id());
  server.set_channel_role_unsigned(channel, dave_id, teamcraft_proto::ChannelRole::Admin);
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;

  let refused = dave.client.approve_admin_join(channel, bob_id).await;
  assert!(
    matches!(
      refused,
      Err(CoreError::JoinRequestNotApprovable(
        JoinRequestRefusal::OwnerRoleNotSigned
      ))
    ),
    "{refused:?}"
  );
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(!bob.client.has_group(channel).unwrap());

  carol.client.confirm_channel_role(channel, dave_id).await.unwrap();
  dave.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(bob.client.has_group(channel).unwrap());
  assert_eq!(lines(&carol, channel), vec![(bob_id, By::Approved(dave_id))]);
}

/// Migration: a channel from before signed roles has no signed admin, so nobody approves until an
/// admin the server lists signs their own role (founding the log); the other members then take the
/// add in. Nothing was locked out for good, and the channel's other roles stay as they were.
#[tokio::test]
async fn a_channel_from_before_signed_roles_approves_once_its_owner_signed() {
  let (server, mut alice, mut bob, mut carol, mut dave, channel) =
    setup_in(AdminSelfJoin::OwnerApproval, HistoryPolicy::FromJoin, true).await;
  let (bob_id, carol_id) = (bob.client.user_id(), carol.client.user_id());
  server.set_role_log_lacking(false);
  bob.client.request_admin_join(channel).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(!carol.client.channel_role_state(channel).unwrap().is_signed());

  assert!(matches!(
    carol.client.approve_admin_join(channel, bob_id).await,
    Err(CoreError::JoinRequestNotApprovable(
      JoinRequestRefusal::OwnerRoleNotSigned
    ))
  ));

  carol
    .client
    .confirm_channel_role(channel, carol_id)
    .await
    .unwrap();
  carol.client.approve_admin_join(channel, bob_id).await.unwrap();
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  assert!(bob.client.has_group(channel).unwrap());
  assert_eq!(lines(&dave, channel), vec![(bob_id, By::Approved(carol_id))]);
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
      } => {
        assert!(
          message.maybe_verified_commit.is_some(),
          "{}: a listed message",
          peer.name
        );
        let by = match (is_approved, message.sender_user_id) {
          (true, approver) => By::Approved(approver),
          (false, author) if author == user => By::Nobody,
          (false, adder) => By::Added(adder),
        };
        Some((user, by))
      },
      _ => None,
    })
    .collect()
}

/// The only line `peer` lists in `channel`, as stored.
fn line_message(peer: &Peer, channel: ConversationId) -> StoredMessage {
  let mut found: Vec<StoredMessage> = peer
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|message| message.maybe_verified_commit.is_some())
    .collect();
  assert_eq!(found.len(), 1, "{}", peer.name);
  found.remove(0)
}

/// How many "admin joined" messages (not lines) `peer` has in `channel`.
fn announcements(peer: &Peer, channel: ConversationId) -> usize {
  peer
    .store
    .messages(channel, MessageFilter::All)
    .unwrap()
    .into_iter()
    .filter(|message| {
      message.maybe_verified_commit.is_none()
        && matches!(
          message.content,
          MessageContent::Event {
            event: ConversationEvent::AdminJoined { .. }
          }
        )
    })
    .count()
}

/// The admin-join events the UI adapter hands `peer`'s app for `channel`. Their rows are no
/// messages waiting to be sent.
#[cfg(feature = "ui")]
fn shown(peer: &Peer, channel: ConversationId) -> Vec<WorkspaceEvent> {
  use teamcraft_client_api::backend::ChatBackend;
  use teamcraft_client_api::model::is_pending;

  let backend = common::backend_of(peer);
  let count = backend.message_count(channel);
  let ids: Vec<_> = backend
    .messages(channel, 0, count)
    .into_iter()
    .map(|message| message.id)
    .collect();
  assert_eq!(ids.len(), count, "{}: counted as listed", peer.name);
  backend
    .message_extras(channel, &ids)
    .into_iter()
    .filter(|extras| {
      matches!(
        extras.maybe_workspace_event,
        Some(WorkspaceEvent::AdminJoined { .. })
      )
    })
    .map(|extras| {
      assert!(!is_pending(extras.message), "{}: shown as sending", peer.name);
      extras.maybe_workspace_event.unwrap()
    })
    .collect()
}

/// `peer` sends the "admin joined" message about `user`, approved by themselves, without any commit.
async fn forge_admin_join(peer: &mut Peer, channel: ConversationId, user: UserId) {
  send(
    peer,
    channel,
    MessageContent::Event {
      event: ConversationEvent::AdminJoined {
        user,
        is_approved: true,
      },
    },
  )
  .await;
}

/// `peer` sends `content` to `channel` as written.
async fn send(peer: &mut Peer, channel: ConversationId, content: MessageContent) -> ClientMessageId {
  let id = records::enqueue_message(
    &*peer.store,
    peer.client.user_id(),
    1_800_000_000_000,
    Draft {
      conversation_id: channel,
      maybe_thread_id: None,
      content,
    },
  )
  .unwrap();
  peer.client.flush_outbox().await.unwrap();
  id
}

/// Puts `content` into `peer`'s outbox under the ID `id`, as a client that picks its IDs would.
fn enqueue_as(peer: &Peer, channel: ConversationId, id: ClientMessageId, content: MessageContent) {
  peer
    .store
    .upsert_message(&StoredMessage {
      conversation_id: channel,
      client_message_id: id,
      maybe_server_id: None,
      sender_user_id: peer.client.user_id(),
      maybe_thread_id: None,
      sent_at_ms: 1_800_000_000_000,
      content,
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    })
    .unwrap();
  records::put_json(
    &*peer.store,
    Table::Outbox,
    &records::outbox_key(channel, id),
    &OutboxEntry {
      conversation_id: channel,
      client_message_id: id,
      maybe_thread_id: None,
      attempts: 0,
      maybe_last_error: None,
    },
  )
  .unwrap();
}

/// A further device of `owner`'s user, linked and connected.
async fn second_device(owner: &mut Peer) -> Peer {
  let mut client = common::link_device(owner).await;
  let (gateway, _) = client.connect_gateway().await.unwrap();
  Peer {
    name: format!("{} laptop", owner.name),
    api: owner.api.clone(),
    store: client.store().clone(),
    client,
    gateway,
  }
}

/// Alice owns the workspace and signs Bob as an admin; Carol founds (owns) a private channel and
/// adds Dave. Admins join channels by themselves as `admin_self_join` says.
async fn setup(admin_self_join: AdminSelfJoin) -> (FakeServer, Peer, Peer, Peer, Peer, ConversationId) {
  setup_with(admin_self_join, HistoryPolicy::FromJoin).await
}

async fn setup_with(
  admin_self_join: AdminSelfJoin,
  history_default: HistoryPolicy,
) -> (FakeServer, Peer, Peer, Peer, Peer, ConversationId) {
  setup_in(admin_self_join, history_default, false).await
}

/// [`setup_with`]; `is_from_before_signed_roles` makes the channel on an older server, without a
/// role log.
async fn setup_in(
  admin_self_join: AdminSelfJoin,
  history_default: HistoryPolicy,
  is_from_before_signed_roles: bool,
) -> (FakeServer, Peer, Peer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut carol = Peer::enroll(&server, "Carol").await;
  let mut dave = Peer::enroll(&server, "Dave").await;
  let (alice_id, bob_id) = (alice.client.user_id(), bob.client.user_id());
  alice
    .client
    .publish_roster(&[(alice_id, RosterRole::Owner), (bob_id, RosterRole::Admin)])
    .await
    .unwrap();
  alice
    .client
    .publish_policy(WorkspacePolicy {
      admin_self_join,
      history_default,
      ..WorkspacePolicy::default()
    })
    .await
    .unwrap();
  server.set_admin(bob_id);
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  server.set_role_log_lacking(is_from_before_signed_roles);
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
  settle(&mut [&mut alice, &mut bob, &mut carol, &mut dave]).await;
  (server, alice, bob, carol, dave, channel)
}
