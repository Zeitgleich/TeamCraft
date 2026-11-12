//! Meeting chats anchored like channels (D10, #1747, e2ee.md §4.6.5, "Meetings (D10)"): the
//! organizer's device founds the chat's group with an anchor of kind `meeting_chat` naming the
//! meeting, and a member checks it against the meeting and organizer the server names. The
//! verdicts case by case (a copied anchor, an invitee's own signature) are in
//! `teamcraft_core::anchoring`.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, link_device, settle};
use teamcraft_core::anchoring::ANCHORING_RELEASE;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::HostsChange;
use teamcraft_core::meeting_invitees::InviteeList;
use teamcraft_proto::{ConversationId, MeetingId};

const ACCESS_ENDS_MS: i64 = 4_000_000_000_000;

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
