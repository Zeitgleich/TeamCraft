//! The organizer-signed invitee list (#1726, e2ee.md §4.6.5, "Meetings (D10)"): a host's Join
//! starts from the server's attendees, and adds only users the organizer's newest signed list
//! names. Anybody else knocks at the lobby.
#![allow(clippy::unwrap_used)]

use crate::common;

use common::{Peer, TestClient, link_device};
use rand_core::OsRng;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::meeting_invitees::InviteeList;
use teamcraft_core::workspace::RosterRole;
use teamcraft_crypto::identity::{IdentityKeyPair, IdentityPublicKey};
use teamcraft_crypto::ids::{ConversationId as SignedChatId, MeetingId as SignedMeetingId};
use teamcraft_crypto::meeting_admitters::{AdmittersDraft, LinkAdmission, SignedMeetingAdmitters};
use teamcraft_proto::{ConversationId, MeetingId, UserId};

/// The release whose apps read the invitees: the instance's minimum here.
const SIGNING_RELEASE: &str = "1.0.32";
const ACCESS_ENDS_MS: i64 = 4_000_000_000_000;

#[tokio::test]
async fn the_statement_names_the_invitees_once_every_app_reads_them() {
  let rig = Rig::new(SIGNING_RELEASE).await;
  let statement = rig.statement();
  assert_eq!(
    statement.maybe_invitees(),
    Some(&[signed(rig.bob), signed(rig.carol)][..])
  );
}

#[tokio::test]
async fn before_that_release_the_statement_stays_version_one() {
  let rig = Rig::new("1.0.31").await;
  assert_eq!(rig.statement().maybe_invitees(), None);
}

#[tokio::test]
async fn an_attendee_the_server_slips_in_knocks_while_a_signed_invitee_gets_in() {
  let mut rig = Rig::new(SIGNING_RELEASE).await;
  let mut laptop = rig.organizers_laptop().await;
  let mut carol = Peer::enroll_as(rig.carol_api.take().unwrap(), "Carol").await;

  // The Join on the organizer's laptop, which never chose a list itself: the server lists
  // Mallory, whom the organizer never invited.
  laptop
    .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
    .await
    .unwrap();

  carol.client.full_sync().await.unwrap();
  rig.mallory.client.full_sync().await.unwrap();
  assert!(
    carol.client.has_group(rig.chat).unwrap(),
    "Carol, signed and now with a device, was added"
  );
  assert!(
    !rig.mallory.client.has_group(rig.chat).unwrap(),
    "Mallory got no welcome"
  );
  assert!(!laptop.group_users(rig.chat).unwrap().contains(&rig.mallory_id()));
}

#[tokio::test]
async fn the_organizers_own_device_holds_to_its_own_choice() {
  let mut rig = Rig::new(SIGNING_RELEASE).await;
  let listed = rig.listed();
  rig
    .alice
    .client
    .set_up_meeting_chat_from(rig.chat, None, &listed)
    .await
    .unwrap();
  rig.mallory.client.full_sync().await.unwrap();
  assert!(!rig.mallory.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn an_edit_carries_over_only_signed_invitees_from_the_servers_list() {
  let mut rig = Rig::new(SIGNING_RELEASE).await;
  let mut laptop = rig.organizers_laptop().await;
  let dave_id = rig.dave;
  // The organizer adds Dave on the laptop; the edit sends the whole list as the server had it,
  // Mallory included.
  let edit = InviteeList::Chosen {
    added: vec![dave_id],
    kept: vec![rig.bob, rig.mallory_id()],
  };
  laptop
    .update_meeting_chat_members_from(rig.chat, None, &edit, &[])
    .await
    .unwrap();
  let in_group = laptop.group_users(rig.chat).unwrap();
  assert!(in_group.contains(&dave_id));
  assert!(!in_group.contains(&rig.mallory_id()));

  // The laptop's next statement names Dave, and not Mallory.
  assert!(
    laptop
      .sync_meeting_admitters(rig.meeting, rig.chat, ACCESS_ENDS_MS)
      .await
      .unwrap()
  );
  let invitees = rig.statement().maybe_invitees().unwrap().to_vec();
  assert!(invitees.contains(&signed(dave_id)));
  assert!(!invitees.contains(&signed(rig.mallory_id())));
}

mod organizer_devices {
  use super::*;

  /// One device's edit whose signing failed doesn't override what another device decided since.
  #[tokio::test]
  async fn a_stale_choice_of_one_device_doesnt_bring_back_a_user_the_other_took_out() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let (chat, bob, carol, dave) = (rig.chat, rig.bob, rig.carol, rig.dave);
    let mut laptop = rig.organizers_laptop().await;
    // The phone invites Dave, but its statement never reaches the server.
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol, dave], &[])
      .await
      .unwrap();
    // The laptop takes Dave off the list again and signs.
    laptop.full_sync().await.unwrap();
    laptop
      .update_meeting_chat_members(chat, &[bob, carol], &[dave])
      .await
      .unwrap();
    // The laptop's list is the signed one again: it may have nothing new to sign.
    laptop
      .sync_meeting_admitters(rig.meeting, chat, ACCESS_ENDS_MS)
      .await
      .unwrap();

    // The phone comes back.
    rig.alice.client.full_sync().await.unwrap();
    rig.sign_if_changed().await;
    rig
      .alice
      .client
      .set_up_meeting_chat_from(chat, None, &InviteeList::Listed(vec![bob, carol, dave]))
      .await
      .unwrap();

    assert!(!rig.statement().maybe_invitees().unwrap().contains(&signed(dave)));
    assert!(!rig.alice.client.group_users(chat).unwrap().contains(&dave));
  }

  /// A meeting from before signed invitees: an edit once signing is on carries over the invitees
  /// already in the group, not only the users it adds.
  #[tokio::test]
  async fn a_legacy_meetings_edit_keeps_the_invitees_in_the_group() {
    let mut rig = Rig::new("1.0.31").await;
    assert_eq!(rig.statement().maybe_invitees(), None);
    rig.server.set_minimum_client_version(SIGNING_RELEASE);
    let (chat, bob, carol, dave) = (rig.chat, rig.bob, rig.carol, rig.dave);
    let edit = InviteeList::Chosen {
      added: vec![dave],
      kept: vec![bob, carol],
    };
    rig
      .alice
      .client
      .update_meeting_chat_members_from(chat, None, &edit, &[])
      .await
      .unwrap();
    rig.sign().await;

    let invitees = rig.statement().maybe_invitees().unwrap().to_vec();
    assert!(invitees.contains(&signed(bob)), "in the group: carried over");
    assert!(invitees.contains(&signed(dave)), "added");
    assert!(
      !invitees.contains(&signed(carol)),
      "no device in the group yet: knocks until invited again"
    );
  }

  /// A newer version 1 statement (an organizer device that doesn't sign invitees yet) leaves the
  /// invitees of the version 2 one before it.
  #[tokio::test]
  async fn a_newer_first_version_statement_keeps_the_signed_invitees() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let chat = rig.chat;
    // A device new to the instance's lower minimum signs version 1 statements.
    rig.server.set_minimum_client_version("1.0.31");
    let mut laptop = rig.organizers_laptop().await;
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[rig.bob, rig.carol, rig.dave], &[])
      .await
      .unwrap();
    rig.sign().await;
    laptop.full_sync().await.unwrap();
    assert!(
      laptop
        .sync_meeting_admitters(rig.meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    assert_eq!(
      rig.statement().maybe_invitees(),
      None,
      "the laptop signed version 1"
    );

    let listed = rig.listed();
    rig
      .alice
      .client
      .set_up_meeting_chat_from(chat, None, &listed)
      .await
      .unwrap();

    assert!(
      !rig
        .alice
        .client
        .group_users(chat)
        .unwrap()
        .contains(&rig.mallory_id())
    );
  }
}

mod co_hosts {
  use super::*;
  use teamcraft_core::client::HostsChange;

  #[tokio::test]
  async fn a_co_hosts_join_adds_only_signed_invitees() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mut bob = rig.bob_peer.take().unwrap();
    rig
      .alice
      .client
      .change_meeting_hosts(&HostsChange {
        meeting: rig.meeting,
        conversation: rig.chat,
        co_hosts: vec![rig.bob],
        maybe_new_organizer: None,
      })
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();

    bob
      .client
      .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(
      !bob
        .client
        .group_users(rig.chat)
        .unwrap()
        .contains(&rig.mallory_id())
    );
  }
}

mod hostile_statements {
  use super::*;

  #[tokio::test]
  async fn an_older_statement_the_server_brings_back_adds_nobody_it_named() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    // Mallory was invited once: in the group and in the first statement.
    let mallory_id = rig.mallory_id();
    let (chat, bob, carol) = (rig.chat, rig.bob, rig.carol);
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol, mallory_id], &[])
      .await
      .unwrap();
    rig.sign().await;
    let older = rig.server.meeting_admitters(chat).unwrap();
    // Then taken off the list, out of the group, and out of the newest statement.
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol], &[mallory_id])
      .await
      .unwrap();
    assert!(!rig.alice.client.group_users(chat).unwrap().contains(&mallory_id));
    rig.sign().await;
    let mut laptop = rig.organizers_laptop().await;
    laptop.check_meeting_admission(chat).await.unwrap();
    assert!(
      !laptop.group_users(chat).unwrap().contains(&mallory_id),
      "before the Join"
    );

    rig.server.set_meeting_admitters(chat, older);
    laptop
      .set_up_meeting_chat_from(chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(chat).unwrap().contains(&mallory_id));
  }

  #[tokio::test]
  async fn a_removal_after_the_held_statement_survives_a_withheld_newer_one() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mallory_id = rig.mallory_id();
    let (chat, bob, carol) = (rig.chat, rig.bob, rig.carol);
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol, mallory_id], &[])
      .await
      .unwrap();
    rig.sign().await;
    let mut laptop = rig.organizers_laptop().await;
    laptop.check_meeting_admission(chat).await.unwrap();
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol], &[mallory_id])
      .await
      .unwrap();
    rig.sign().await;
    // The server withholds the newest statement from the laptop, which saw the removal commit.
    rig.server.clear_meeting_admitters(chat);
    laptop.full_sync().await.unwrap();
    assert!(!laptop.group_users(chat).unwrap().contains(&mallory_id));

    laptop
      .set_up_meeting_chat_from(chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(chat).unwrap().contains(&mallory_id));
  }

  /// Statements and commits share no order: an intermediate statement that still names a user
  /// the host saw removed since lifts nothing.
  #[tokio::test]
  async fn an_intermediate_statement_after_a_removal_doesnt_bring_the_user_back() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mut bob = rig.bob_peer.take().unwrap();
    let mallory_id = rig.mallory_id();
    let (chat, bob_id, carol) = (rig.chat, rig.bob, rig.carol);
    bob.client.check_meeting_admission(chat).await.unwrap();
    // Alice invites Mallory and signs (withheld from Bob), then takes her out and signs again.
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob_id, carol, mallory_id], &[])
      .await
      .unwrap();
    rig.sign().await;
    let intermediate = rig.server.meeting_admitters(chat).unwrap();
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob_id, carol], &[mallory_id])
      .await
      .unwrap();
    rig.sign().await;
    bob.client.full_sync().await.unwrap();
    // The server now serves the intermediate statement, newer than any Bob saw.
    rig.server.set_meeting_admitters(chat, intermediate);

    bob
      .client
      .set_up_meeting_chat_from(chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!bob.client.group_users(chat).unwrap().contains(&mallory_id));
  }

  /// The organizer's own removal outlasts another device's statement signed before it.
  #[tokio::test]
  async fn the_organizers_removal_outlasts_another_devices_earlier_statement() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mallory_id = rig.mallory_id();
    let (chat, bob, carol) = (rig.chat, rig.bob, rig.carol);
    // Mallory is a colleague here, whom the laptop invites: on the roster, so its statement names
    // her as an admitter once she is in the group.
    rig
      .alice
      .client
      .publish_roster(&[
        (rig.alice.client.user_id(), RosterRole::Owner),
        (bob, RosterRole::Admin),
        (rig.dave, RosterRole::Admin),
        (mallory_id, RosterRole::Admin),
      ])
      .await
      .unwrap();
    let mut laptop = rig.organizers_laptop().await;
    // The laptop invites Mallory and signs; the phone never fetches that statement.
    laptop
      .update_meeting_chat_members(chat, &[bob, carol, mallory_id], &[])
      .await
      .unwrap();
    assert!(
      laptop
        .sync_meeting_admitters(rig.meeting, chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    // The phone sees the add, then takes Mallory off the list.
    rig.alice.client.full_sync().await.unwrap();
    rig
      .alice
      .client
      .update_meeting_chat_members(chat, &[bob, carol], &[mallory_id])
      .await
      .unwrap();

    rig.sign_if_changed().await;
    rig
      .alice
      .client
      .set_up_meeting_chat_from(chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(
      !rig
        .statement()
        .maybe_invitees()
        .unwrap()
        .contains(&signed(mallory_id))
    );
    assert!(!rig.alice.client.group_users(chat).unwrap().contains(&mallory_id));
  }

  #[tokio::test]
  async fn a_statement_of_another_chat_adds_nobody() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mallory_id = rig.mallory_id();
    // Another meeting of Alice's that invites Mallory, signed by Alice.
    let other_chat = rig
      .server
      .create_meeting_chat(rig.alice.client.user_id(), &[mallory_id]);
    rig
      .alice
      .client
      .set_up_meeting_chat(other_chat, &[mallory_id])
      .await
      .unwrap();
    assert!(
      rig
        .alice
        .client
        .sync_meeting_admitters(rig.meeting, other_chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    let moved = rig.server.meeting_admitters(other_chat).unwrap();
    rig.server.set_meeting_admitters(rig.chat, moved);
    let mut laptop = rig.organizers_laptop().await;

    laptop
      .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(rig.chat).unwrap().contains(&mallory_id));
  }

  #[tokio::test]
  async fn a_statement_signed_by_another_key_adds_nobody() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let forged = AdmittersDraft {
      maybe_invitees: Some(vec![signed(rig.bob), signed(rig.mallory_id())]),
      sequence: u64::MAX / 2,
      ..rig.draft()
    }
    .sign(&IdentityKeyPair::generate(&mut OsRng))
    .unwrap();
    rig
      .server
      .set_meeting_admitters(rig.chat, forged.to_bytes().unwrap());
    let mut laptop = rig.organizers_laptop().await;

    laptop
      .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(rig.chat).unwrap().contains(&rig.mallory_id()));
  }

  #[tokio::test]
  async fn a_tampered_statement_adds_nobody() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    let mut bytes = rig.server.meeting_admitters(rig.chat).unwrap();
    // One bit of the signed content changes: the signature no longer holds.
    bytes[20] ^= 1;
    rig.server.set_meeting_admitters(rig.chat, bytes);
    let mut laptop = rig.organizers_laptop().await;

    laptop
      .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(rig.chat).unwrap().contains(&rig.mallory_id()));
  }

  #[tokio::test]
  async fn without_any_statement_the_servers_list_adds_nobody() {
    let mut rig = Rig::new(SIGNING_RELEASE).await;
    rig.server.clear_meeting_admitters(rig.chat);
    let mut laptop = rig.organizers_laptop().await;

    laptop
      .set_up_meeting_chat_from(rig.chat, None, &rig.listed())
      .await
      .unwrap();

    assert!(!laptop.group_users(rig.chat).unwrap().contains(&rig.mallory_id()));
  }
}

/// Alice organizes a meeting inviting Bob and Carol (who has no device yet); the server also lists
/// Mallory. Alice's device set the chat up from her own request and signed the admitters.
struct Rig {
  server: FakeServer,
  alice: Peer,
  mallory: Peer,
  bob: UserId,
  carol: UserId,
  carol_api: Option<teamcraft_core::api::fake::FakeServerApi>,
  bob_peer: Option<Peer>,
  /// A member of the chat on the server, not invited (yet).
  dave: UserId,
  chat: ConversationId,
  meeting: MeetingId,
}

impl Rig {
  async fn new(minimum_client_version: &str) -> Self {
    let server = FakeServer::new();
    server.set_minimum_client_version(minimum_client_version);
    let mut alice = Peer::enroll(&server, "Alice").await;
    let mut bob = Peer::enroll(&server, "Bob").await;
    let mallory = Peer::enroll(&server, "Mallory").await;
    let dave = Peer::enroll(&server, "Dave").await.client.user_id();
    // Alice's colleagues are on the roster she signs: her device vouches for them as members
    // (#1873). Carol has no device yet; Mallory is the server's slip.
    alice
      .client
      .publish_roster(&[
        (alice.client.user_id(), RosterRole::Owner),
        (bob.client.user_id(), RosterRole::Admin),
        (dave, RosterRole::Admin),
      ])
      .await
      .unwrap();
    let carol_api = server.add_user("Carol");
    let (bob_id, carol) = (bob.client.user_id(), carol_api.user_id());
    let chat = server.create_meeting_chat(
      alice.client.user_id(),
      &[bob_id, carol, mallory.client.user_id(), dave],
    );
    alice
      .client
      .set_up_meeting_chat(chat, &[bob_id, carol])
      .await
      .unwrap();
    bob.client.full_sync().await.unwrap();
    let mut rig = Self {
      server,
      alice,
      mallory,
      bob: bob_id,
      carol,
      carol_api: Some(carol_api),
      bob_peer: None,
      dave,
      chat,
      meeting: "8".parse().unwrap(),
    };
    rig.sign().await;
    rig.bob_peer = Some(bob);
    rig
  }

  fn mallory_id(&self) -> UserId {
    self.mallory.client.user_id()
  }

  /// The meeting's attendees as the server lists them.
  fn listed(&self) -> InviteeList {
    InviteeList::Listed(vec![self.bob, self.carol, self.mallory_id()])
  }

  /// Alice's phone signs anew if anything changed; whether it did doesn't matter here.
  async fn sign_if_changed(&mut self) {
    self
      .alice
      .client
      .sync_meeting_admitters(self.meeting, self.chat, ACCESS_ENDS_MS)
      .await
      .unwrap();
  }

  async fn sign(&mut self) {
    assert!(
      self
        .alice
        .client
        .sync_meeting_admitters(self.meeting, self.chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
  }

  /// The newest statement on the server, checked against Alice's key.
  fn statement(&self) -> teamcraft_crypto::meeting_admitters::VerifiedMeetingAdmitters {
    let bytes = self.server.meeting_admitters(self.chat).unwrap();
    let response = self.server.identity_key_of(self.alice.client.user_id()).unwrap();
    let organizer =
      IdentityPublicKey::from_bytes(response.public_key.as_slice().try_into().unwrap()).fingerprint();
    SignedMeetingAdmitters::from_bytes(&bytes)
      .unwrap()
      .verify(&teamcraft_crypto::meeting_admitters::AdmittersExpectation {
        organizer: &organizer,
        maybe_meeting_id: Some(SignedMeetingId::new(raw(self.meeting))),
        conversation_id: SignedChatId::new(raw(self.chat)),
        now_ms: 0,
        maybe_seen_sequence: None,
      })
      .unwrap()
  }

  /// A statement for this meeting and chat, unsigned yet.
  fn draft(&self) -> AdmittersDraft {
    AdmittersDraft {
      meeting_id: SignedMeetingId::new(raw(self.meeting)),
      conversation_id: SignedChatId::new(raw(self.chat)),
      sequence: 1,
      expires_at_ms: u64::try_from(ACCESS_ENDS_MS).unwrap(),
      link_admission: LinkAdmission::Lobby,
      admitters: Vec::new(),
      maybe_invitees: None,
    }
  }

  /// A second device of Alice's that joined the chat's group but never chose a list itself.
  async fn organizers_laptop(&mut self) -> TestClient {
    let mut laptop = link_device(&mut self.alice).await;
    laptop.full_sync().await.unwrap();
    laptop.prepare_call(self.chat).await.unwrap();
    assert!(laptop.has_group(self.chat).unwrap());
    laptop
  }
}

fn signed(user: UserId) -> teamcraft_crypto::ids::UserId {
  teamcraft_crypto::ids::UserId::new(raw(user))
}

/// A proto ID as the signed statements' raw value.
fn raw(id: impl std::fmt::Display) -> u64 {
  id.to_string().parse().unwrap()
}
