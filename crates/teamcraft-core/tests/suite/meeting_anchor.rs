//! Meeting chats anchored like channels (D10, #1747, e2ee.md §4.6.5, "Meetings (D10)"): the
//! organizer's device founds the chat's group with an anchor of kind `meeting_chat` naming the
//! meeting, and a member checks it against the meeting and organizer the server names. The
//! verdicts case by case (a copied anchor, an invitee's own signature) are in
//! `teamcraft_core::anchoring`.
#![allow(clippy::unwrap_used)]

use crate::common;

use std::sync::Arc;

use common::{Peer, link_device, link_device_with_store, settings, settle};
use teamcraft_core::anchoring::ANCHORING_RELEASE;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::HostsChange;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::meeting_invitees::InviteeList;
use teamcraft_core::workspace::RosterRole;
use teamcraft_proto::{ConversationId, MeetingId};

const ACCESS_ENDS_MS: i64 = 4_000_000_000_000;
const NOW_MS: i64 = 1_800_000_000_000;

#[tokio::test]
async fn an_invitee_pins_the_organizers_anchor() {
  let (_server, mut alice, mut bob, _carol, chat) = anchored_meeting().await;
  bob.client.full_sync().await.unwrap();

  assert!(bob.client.has_group(chat).unwrap());
  let alices = alice.client.account_data().anchor_floor(chat).unwrap();
  let bobs = bob.client.account_data().anchor_floor(chat).unwrap();
  assert_eq!(alices.founding_hash, bobs.founding_hash);
  assert!(bobs.grants());
  // The organizer's other device joins on the same founding.
  let mut laptop = link_device(&mut alice).await;
  laptop.full_sync().await.unwrap();
  laptop.prepare_call(chat).await.unwrap();
  assert_eq!(
    laptop.account_data().anchor_floor(chat).unwrap().founding_hash,
    alices.founding_hash
  );
}

#[tokio::test]
async fn a_meeting_chat_is_founded_unanchored_below_the_release() {
  let server = FakeServer::new();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
  let meeting = server.meeting_of_chat(chat);
  alice
    .client
    .set_up_meeting_chat_from(chat, meeting, &chosen(&bob))
    .await
    .unwrap();
  assert!(alice.client.has_group(chat).unwrap());
  assert_eq!(alice.client.account_data().anchor_floor(chat), None);
}

#[tokio::test]
async fn an_invitee_refuses_the_chat_when_the_server_names_another_meeting() {
  let (server, mut alice, _bob, mut carol, chat) = anchored_meeting().await;
  let carol_id = carol.client.user_id();
  // The server relabels the chat as another meeting's: the anchor names the first one.
  server.set_meeting_of_chat(chat, other_meeting());
  alice
    .client
    .update_meeting_chat_members(chat, &[carol_id], &[])
    .await
    .unwrap();

  carol.client.full_sync().await.unwrap();

  assert!(!carol.client.has_group(chat).unwrap(), "the welcome was refused");
  assert_eq!(carol.client.account_data().anchor_floor(chat), None);
}

#[tokio::test]
async fn after_a_handover_a_new_member_accepts_the_former_organizers_anchor() {
  let (server, mut alice, mut bob, mut carol, chat) = anchored_meeting().await;
  bob.client.full_sync().await.unwrap();
  let meeting = server.meeting_of_chat(chat).unwrap();
  // Alice hands the meeting to Bob: the server names Bob, the anchor still Alice as founder.
  alice
    .client
    .change_meeting_hosts(&HostsChange {
      meeting,
      conversation: chat,
      co_hosts: Vec::new(),
      maybe_new_organizer: Some(bob.client.user_id()),
    })
    .await
    .unwrap();
  bob.client.full_sync().await.unwrap();

  bob
    .client
    .let_into_meeting_chat(chat, carol.client.user_id())
    .await
    .unwrap();
  settle(&mut [&mut bob, &mut carol]).await;
  carol.client.full_sync().await.unwrap();

  assert!(carol.client.has_group(chat).unwrap());
  assert_eq!(
    carol
      .client
      .account_data()
      .anchor_floor(chat)
      .unwrap()
      .founding_hash,
    alice
      .client
      .account_data()
      .anchor_floor(chat)
      .unwrap()
      .founding_hash
  );
}

/// A server that names an insider as organizer gets the insider's statement nowhere: devices that
/// pinned the chat's anchor hold to its founder, absent a signed handover.
#[tokio::test]
async fn a_statement_of_an_organizer_the_server_made_up_counts_nowhere() {
  let server = anchoring_server_signing_invitees();
  let mut alice = Peer::enroll(&server, "Alice").await;
  let mut bob = Peer::enroll(&server, "Bob").await;
  let mut mallory = Peer::enroll(&server, "Mallory").await;
  // Dave has no device yet: whoever lists him can't get him in right away.
  let dave_api = server.add_user("Dave");
  let dave = dave_api.user_id();
  let (bob_id, mallory_id) = (bob.client.user_id(), mallory.client.user_id());
  let chat = server.create_meeting_chat(alice.client.user_id(), &[bob_id, mallory_id, dave]);
  let meeting = server.meeting_of_chat(chat).unwrap();
  alice
    .client
    .set_up_meeting_chat_from(
      chat,
      Some(meeting),
      &InviteeList::Chosen {
        added: vec![bob_id, mallory_id],
        kept: Vec::new(),
      },
    )
    .await
    .unwrap();
  assert!(
    alice
      .client
      .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
      .await
      .unwrap()
  );
  bob.client.full_sync().await.unwrap();
  mallory.client.full_sync().await.unwrap();
  bob.client.check_meeting_admission(chat).await.unwrap();
  // Mallory saw Alice's statement: hers goes above it.
  mallory.client.check_meeting_admission(chat).await.unwrap();

  // The server makes Mallory, an invitee, the organizer; she invites Dave and signs.
  server.set_organizer_of_chat(chat, mallory_id);
  mallory
    .client
    .update_meeting_chat_members(chat, &[bob_id, dave], &[])
    .await
    .unwrap();
  assert!(
    mallory
      .client
      .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
      .await
      .unwrap()
  );
  let mut dave_peer = Peer::enroll_as(dave_api, "Dave").await;
  let listed = InviteeList::Listed(vec![bob_id, mallory_id, dave]);

  alice
    .client
    .set_up_meeting_chat_from(chat, Some(meeting), &listed)
    .await
    .unwrap();
  bob
    .client
    .set_up_meeting_chat_from(chat, Some(meeting), &listed)
    .await
    .unwrap();

  dave_peer.client.full_sync().await.unwrap();
  assert!(!alice.client.group_users(chat).unwrap().contains(&dave));
  assert!(!bob.client.group_users(chat).unwrap().contains(&dave));
}

mod servers_word {
  use super::*;

  /// An insider founds the chat herself and the server names no organizer or meeting: her anchor
  /// can't vouch for itself, so a new member pins nothing.
  #[tokio::test]
  async fn a_self_founded_anchor_without_the_servers_organizer_pins_nothing() {
    let (server, mut bob, chat) = insider_founded(false).await;
    bob.client.full_sync().await.unwrap();
    assert_eq!(bob.client.account_data().anchor_floor(chat), None);
    let _ = server;
  }

  /// The server names the organizer but not the meeting: the insider's anchor is held to the
  /// organizer, and refused.
  #[tokio::test]
  async fn a_self_founded_anchor_without_the_servers_meeting_is_refused() {
    let (_server, mut bob, chat) = insider_founded(true).await;
    bob.client.full_sync().await.unwrap();
    assert!(!bob.client.has_group(chat).unwrap());
    assert_eq!(bob.client.account_data().anchor_floor(chat), None);
  }

  /// Mallory, invited to Alice's meeting, founds its chat anchored by herself and adds Bob; the
  /// server leaves out the meeting (and, unless `keeps_organizer`, the organizer).
  async fn insider_founded(keeps_organizer: bool) -> (FakeServer, Peer, ConversationId) {
    let server = anchoring_server();
    let alice = Peer::enroll(&server, "Alice").await;
    let bob = Peer::enroll(&server, "Bob").await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    let chat = server.create_meeting_chat(
      alice.client.user_id(),
      &[bob.client.user_id(), mallory.client.user_id()],
    );
    let meeting = server.meeting_of_chat(chat);
    server.hide_meeting_of_chat(chat, keeps_organizer);
    let _ = mallory
      .client
      .set_up_meeting_chat_from(chat, meeting, &chosen(&bob))
      .await;
    (server, bob, chat)
  }
}

mod entering {
  use super::*;

  /// A meeting chat founded before anchoring stays unanchored when a member enters it after the
  /// release: nothing is refused and nothing pinned.
  #[tokio::test]
  async fn a_legacy_unanchored_chat_entered_after_the_release_stays_unanchored() {
    let server = FakeServer::new();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let chat = server.create_meeting_chat(alice.client.user_id(), &[bob.client.user_id()]);
    let meeting = server.meeting_of_chat(chat);
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        meeting,
        &InviteeList::Chosen {
          added: Vec::new(),
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    let release = ANCHORING_RELEASE;
    server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
    alice
      .client
      .set_up_meeting_chat_from(chat, meeting, &chosen(&bob))
      .await
      .unwrap();

    bob.client.full_sync().await.unwrap();

    assert!(bob.client.has_group(chat).unwrap());
    assert_eq!(bob.client.account_data().anchor_floor(chat), None);
  }

  /// A further device whose join answer was lost is added back into an anchored meeting chat and
  /// pins the same founding.
  #[tokio::test]
  async fn a_device_added_back_pins_the_meeting_chats_founding() {
    let server = anchoring_server();
    let clock = Arc::new(ManualClock::new(NOW_MS));
    let mut alice = Peer::enroll(&server, "Alice").await;
    let (mut laptop, _) = link_device_with_store(&mut alice, settings(), Box::new(clock.clone())).await;
    let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
    let meeting = server.meeting_of_chat(chat);
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        meeting,
        &InviteeList::Chosen {
          added: Vec::new(),
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    server.lose_next_commit_answer(alice.client.user_id());
    assert!(laptop.prepare_call(chat).await.is_err(), "the answer was lost");
    clock.advance(i64::try_from(settings().rejoin_retry.as_millis()).unwrap());
    assert!(laptop.prepare_call(chat).await.is_err(), "asks to be added back");
    alice.client.full_sync().await.unwrap();

    laptop.prepare_call(chat).await.unwrap();

    assert_eq!(
      laptop.account_data().anchor_floor(chat).unwrap().founding_hash,
      alice
        .client
        .account_data()
        .anchor_floor(chat)
        .unwrap()
        .founding_hash
    );
  }
}

mod handovers {
  use super::*;

  /// After a handover from Alice to Bob to Carol, the server names Bob again and serves only the
  /// first list: a member who saw the newest holds to it, so Bob's statement counts for nothing.
  #[tokio::test]
  async fn a_replayed_host_list_doesnt_make_a_former_organizer_count() {
    let server = anchoring_server_signing_invitees();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    let dave_api = server.add_user("Dave");
    let dave = dave_api.user_id();
    let (bob_id, carol_id, erin_id) = (
      bob.client.user_id(),
      carol.client.user_id(),
      erin.client.user_id(),
    );
    let chat = server.create_meeting_chat(alice.client.user_id(), &[bob_id, carol_id, erin_id, dave]);
    let meeting = server.meeting_of_chat(chat).unwrap();
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        Some(meeting),
        &InviteeList::Chosen {
          added: vec![bob_id, carol_id, erin_id],
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    assert!(
      alice
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    for peer in [&mut bob, &mut carol, &mut erin] {
      peer.client.full_sync().await.unwrap();
      peer.client.check_meeting_admission(chat).await.unwrap();
    }
    hand_over(&mut alice, meeting, chat, bob_id).await;
    bob.client.full_sync().await.unwrap();
    hand_over(&mut bob, meeting, chat, carol_id).await;
    // Erin follows the whole chain to Carol.
    erin.client.full_sync().await.unwrap();
    erin.client.check_meeting_admission(chat).await.unwrap();

    // The server names Bob again and serves only the first handover; Bob invites Dave and signs.
    let lists = server.meeting_hosts(chat);
    server.set_meeting_hosts(chat, lists.into_iter().take(1).collect());
    server.set_organizer_of_chat(chat, bob_id);
    bob.client.check_meeting_admission(chat).await.unwrap();
    bob
      .client
      .update_meeting_chat_members(chat, &[carol_id, erin_id, dave], &[])
      .await
      .unwrap();
    let _ = bob
      .client
      .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
      .await;
    let mut dave_peer = Peer::enroll_as(dave_api, "Dave").await;
    erin.client.full_sync().await.unwrap();
    assert!(
      !erin.client.group_users(chat).unwrap().contains(&dave),
      "not in the group before the Join"
    );

    erin
      .client
      .set_up_meeting_chat_from(chat, Some(meeting), &InviteeList::Listed(vec![dave]))
      .await
      .unwrap();

    dave_peer.client.full_sync().await.unwrap();
    assert!(!erin.client.group_users(chat).unwrap().contains(&dave));
  }

  /// #1864: after a handover from Alice to Bob to Carol, the server names Bob again and withholds
  /// his handover to Carol, and Bob signs a list of his own above Carol's (a fork from Alice's
  /// handover to him): a member who saw the handover to Carol refuses the fork, so Bob's
  /// statement counts for nothing, though its sequence is the newest.
  #[tokio::test]
  async fn a_former_organizers_fork_of_the_host_lists_doesnt_count() {
    let server = anchoring_server_signing_invitees();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    let dave_api = server.add_user("Dave");
    let dave = dave_api.user_id();
    let (bob_id, carol_id, erin_id) = (
      bob.client.user_id(),
      carol.client.user_id(),
      erin.client.user_id(),
    );
    let chat = server.create_meeting_chat(alice.client.user_id(), &[bob_id, carol_id, erin_id, dave]);
    let meeting = server.meeting_of_chat(chat).unwrap();
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        Some(meeting),
        &InviteeList::Chosen {
          added: vec![bob_id, carol_id, erin_id],
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    assert!(
      alice
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    for peer in [&mut bob, &mut carol, &mut erin] {
      peer.client.full_sync().await.unwrap();
      peer.client.check_meeting_admission(chat).await.unwrap();
    }
    hand_over(&mut alice, meeting, chat, bob_id).await;
    bob.client.full_sync().await.unwrap();
    hand_over(&mut bob, meeting, chat, carol_id).await;
    erin.client.full_sync().await.unwrap();
    erin.client.check_meeting_admission(chat).await.unwrap();

    // The fork: the first handover only, Bob named again, and Bob's own list above Carol's.
    let lists = server.meeting_hosts(chat);
    server.set_meeting_hosts(chat, lists.into_iter().take(1).collect());
    server.set_organizer_of_chat(chat, bob_id);
    bob
      .client
      .change_meeting_hosts(&HostsChange {
        meeting,
        conversation: chat,
        co_hosts: Vec::new(),
        maybe_new_organizer: None,
      })
      .await
      .unwrap();
    assert_eq!(
      server.meeting_hosts(chat).len(),
      2,
      "Alice's handover and Bob's list"
    );
    bob.client.check_meeting_admission(chat).await.unwrap();
    bob
      .client
      .update_meeting_chat_members(chat, &[carol_id, erin_id, dave], &[])
      .await
      .unwrap();
    let _ = bob
      .client
      .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
      .await;
    let mut dave_peer = Peer::enroll_as(dave_api, "Dave").await;
    erin.client.full_sync().await.unwrap();
    assert!(
      !erin.client.group_users(chat).unwrap().contains(&dave),
      "not in the group before the Join"
    );

    erin
      .client
      .set_up_meeting_chat_from(chat, Some(meeting), &InviteeList::Listed(vec![dave]))
      .await
      .unwrap();

    dave_peer.client.full_sync().await.unwrap();
    assert!(!erin.client.group_users(chat).unwrap().contains(&dave));
  }

  /// Honest handovers across devices (#1864): Alice hands the meeting to Bob, Bob names Carol
  /// co-host and hands it on to her, each step read by Erin's device in between; the server keeps
  /// only the handovers and the newest list. Erin follows every step, and a member added after the
  /// last joins the group Alice founded.
  #[tokio::test]
  async fn honest_handovers_keep_verifying_where_a_handover_was_kept() {
    let server = anchoring_server_signing_invitees();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut carol = Peer::enroll(&server, "Carol").await;
    let mut erin = Peer::enroll(&server, "Erin").await;
    let dave_api = server.add_user("Dave");
    let dave = dave_api.user_id();
    let (bob_id, carol_id, erin_id) = (
      bob.client.user_id(),
      carol.client.user_id(),
      erin.client.user_id(),
    );
    // Colleagues on the roster Alice signs: each organizer's device vouches for them, so Erin
    // is on Carol's admitters later (#1873).
    alice
      .client
      .publish_roster(&[
        (alice.client.user_id(), RosterRole::Owner),
        (bob_id, RosterRole::Admin),
        (carol_id, RosterRole::Admin),
        (erin_id, RosterRole::Admin),
      ])
      .await
      .unwrap();
    let chat = server.create_meeting_chat(alice.client.user_id(), &[bob_id, carol_id, erin_id, dave]);
    let meeting = server.meeting_of_chat(chat).unwrap();
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        Some(meeting),
        &InviteeList::Chosen {
          added: vec![bob_id, carol_id, erin_id],
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    assert!(
      alice
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    for peer in [&mut bob, &mut carol, &mut erin] {
      peer.client.full_sync().await.unwrap();
      peer.client.check_meeting_admission(chat).await.unwrap();
    }
    hand_over(&mut alice, meeting, chat, bob_id).await;
    bob.client.full_sync().await.unwrap();
    erin.client.full_sync().await.unwrap();
    erin.client.check_meeting_admission(chat).await.unwrap();
    // Bob signs his own lists: the hosts again, then Carol as co-host; then he hands over.
    assert!(bob.client.sync_meeting_hosts(meeting, chat).await.unwrap());
    bob
      .client
      .change_meeting_hosts(&HostsChange {
        meeting,
        conversation: chat,
        co_hosts: vec![carol_id],
        maybe_new_organizer: None,
      })
      .await
      .unwrap();
    erin.client.full_sync().await.unwrap();
    erin.client.check_meeting_admission(chat).await.unwrap();
    hand_over(&mut bob, meeting, chat, carol_id).await;
    carol.client.full_sync().await.unwrap();
    assert!(carol.client.sync_meeting_hosts(meeting, chat).await.unwrap());
    assert_eq!(
      server.meeting_hosts(chat).len(),
      3,
      "the two handovers and Carol's list"
    );
    erin.client.full_sync().await.unwrap();
    erin.client.check_meeting_admission(chat).await.unwrap();

    // Carol invites Dave; Erin's device, which kept Bob's handover, follows the chain to her.
    carol
      .client
      .update_meeting_chat_members(chat, &[bob_id, erin_id, dave], &[])
      .await
      .unwrap();
    assert!(
      carol
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    let mut dave_peer = Peer::enroll_as(dave_api, "Dave").await;
    erin
      .client
      .set_up_meeting_chat_from(chat, Some(meeting), &InviteeList::Listed(vec![dave]))
      .await
      .unwrap();
    settle(&mut [&mut erin, &mut dave_peer]).await;
    dave_peer.client.full_sync().await.unwrap();

    assert!(erin.client.group_users(chat).unwrap().contains(&dave));
  }

  /// Before Bob's device is in the group, the server names Mallory and serves a host list she
  /// signed from her own key; Bob's device verifies it from her key. Once Bob pinned Alice's
  /// anchor, that list counts for nothing: Mallory, named again, doesn't become the organizer.
  #[tokio::test]
  async fn host_lists_kept_from_another_key_before_entering_dont_count() {
    let server = anchoring_server_signing_invitees();
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mut mallory = Peer::enroll(&server, "Mallory").await;
    let dave_api = server.add_user("Dave");
    let dave = dave_api.user_id();
    let (alice_id, bob_id, mallory_id) = (
      alice.client.user_id(),
      bob.client.user_id(),
      mallory.client.user_id(),
    );
    let chat = server.create_meeting_chat(alice_id, &[bob_id, mallory_id, dave]);
    let meeting = server.meeting_of_chat(chat).unwrap();
    // The poisoning: Mallory named, her own list, Bob's device reads it with no group yet.
    server.set_organizer_of_chat(chat, mallory_id);
    mallory
      .client
      .change_meeting_hosts(&HostsChange {
        meeting,
        conversation: chat,
        co_hosts: Vec::new(),
        maybe_new_organizer: None,
      })
      .await
      .unwrap();
    let poisoned = server.meeting_hosts(chat);
    assert!(!poisoned.is_empty());
    let _ = bob
      .client
      .let_guest_into_meeting_chat(chat, dave, "", ACCESS_ENDS_MS)
      .await;
    // Alice, named again, sets the chat up; Bob enters and pins her anchor.
    server.set_organizer_of_chat(chat, alice_id);
    alice
      .client
      .set_up_meeting_chat_from(
        chat,
        Some(meeting),
        &InviteeList::Chosen {
          added: vec![bob_id, mallory_id],
          kept: Vec::new(),
        },
      )
      .await
      .unwrap();
    assert!(
      alice
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    bob.client.full_sync().await.unwrap();
    assert!(bob.client.account_data().anchor_floor(chat).is_some());
    mallory.client.full_sync().await.unwrap();
    mallory.client.check_meeting_admission(chat).await.unwrap();

    // Mallory named again, with her own lists; she invites Dave and signs.
    server.set_organizer_of_chat(chat, mallory_id);
    server.set_meeting_hosts(chat, poisoned);
    mallory
      .client
      .update_meeting_chat_members(chat, &[bob_id, dave], &[])
      .await
      .unwrap();
    assert!(
      mallory
        .client
        .sync_meeting_admitters(meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap(),
      "Mallory signed"
    );
    let mut dave_peer = Peer::enroll_as(dave_api, "Dave").await;

    bob
      .client
      .set_up_meeting_chat_from(chat, Some(meeting), &InviteeList::Listed(vec![dave]))
      .await
      .unwrap();

    dave_peer.client.full_sync().await.unwrap();
    assert!(!bob.client.group_users(chat).unwrap().contains(&dave));
  }

  async fn hand_over(from: &mut Peer, meeting: MeetingId, chat: ConversationId, to: teamcraft_proto::UserId) {
    from
      .client
      .change_meeting_hosts(&HostsChange {
        meeting,
        conversation: chat,
        co_hosts: Vec::new(),
        maybe_new_organizer: Some(to),
      })
      .await
      .unwrap();
  }
}

/// Alice organizes a meeting inviting Bob, on an instance whose minimum client version is the
/// anchoring release; her device set the chat up with the meeting's ID. Carol is a member of the
/// chat on the server, not invited yet.
async fn anchored_meeting() -> (FakeServer, Peer, Peer, Peer, ConversationId) {
  let server = FakeServer::new();
  let release = ANCHORING_RELEASE;
  server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
  let mut alice = Peer::enroll(&server, "Alice").await;
  let bob = Peer::enroll(&server, "Bob").await;
  let carol = Peer::enroll(&server, "Carol").await;
  let chat = server.create_meeting_chat(
    alice.client.user_id(),
    &[bob.client.user_id(), carol.client.user_id()],
  );
  let meeting = server.meeting_of_chat(chat);
  assert!(meeting.is_some());
  alice
    .client
    .set_up_meeting_chat_from(chat, meeting, &chosen(&bob))
    .await
    .unwrap();
  assert!(
    alice.client.account_data().anchor_floor(chat).is_some(),
    "the organizer pinned its own founding"
  );
  (server, alice, bob, carol, chat)
}

fn chosen(peer: &Peer) -> InviteeList {
  InviteeList::Chosen {
    added: vec![peer.client.user_id()],
    kept: Vec::new(),
  }
}

fn other_meeting() -> MeetingId {
  "4242".parse().unwrap()
}

fn anchoring_server_signing_invitees() -> FakeServer {
  let server = FakeServer::new();
  let release = teamcraft_core::meeting_invitees::SIGNED_INVITEES_RELEASE;
  server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
  server
}

fn anchoring_server() -> FakeServer {
  let server = FakeServer::new();
  let release = ANCHORING_RELEASE;
  server.set_minimum_client_version(&format!("{}.{}.{}", release.major, release.minor, release.patch));
  server
}
