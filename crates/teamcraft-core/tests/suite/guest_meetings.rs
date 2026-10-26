//! Meeting guests: an ephemeral device joins the meeting chat's group after the host's device
//! checked the link's proof; the guest only joins the organizer's group; guests manage nobody
//! (e2ee.md §4.6, "Meeting guests").
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, link_device, settle};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{GuestAdmission, GuestLinkMaterial};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::error::{CoreError, GuestRefusal};
use teamcraft_core::store::MemoryStore;
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_proto::{AddConversationMemberRequest, ConversationId, MeetingId, UserId};

const GUEST_NAME: &str = "Gus";
const NOW_MS: i64 = 1_800_000_000_000;
/// Far later than the test clock.
const ACCESS_ENDS_MS: i64 = 4_000_000_000_000;

struct Rig {
  server: FakeServer,
  alice: Peer,
  guest: Peer,
  chat: ConversationId,
  meeting: MeetingId,
  link: GuestLinkMaterial,
  clock: Arc<ManualClock>,
}

#[tokio::test]
async fn a_guest_with_the_links_proof_is_let_in_and_reads_from_the_join_on() {
  let mut rig = rig().await;
  rig
    .alice
    .client
    .send_text(rig.chat, None, "before the guest")
    .await
    .unwrap();
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  let proof = rig.proof(&rig.link.secret);

  rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap();
  rig
    .alice
    .client
    .send_text(rig.chat, None, "after the guest")
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  rig.guest.client.full_sync().await.unwrap();
  rig
    .guest
    .client
    .send_text(rig.chat, None, "hello from the guest")
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;

  assert!(rig.guest.client.has_group(rig.chat).unwrap());
  assert_eq!(
    rig.guest.texts(rig.chat),
    vec!["after the guest", "hello from the guest"]
  );
  assert_eq!(
    rig.alice.texts(rig.chat),
    vec!["before the guest", "after the guest", "hello from the guest"]
  );
}

#[tokio::test]
async fn a_guest_the_server_made_up_is_not_let_in() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  // The server doesn't have the link's secret: its proof is made with a guess.
  let guess = base64_url_of(&[7u8; 32]);
  let forged = rig.proof(&guess);

  for admission in [GuestAdmission::Proof(forged), GuestAdmission::Proof(Vec::new())] {
    let refused = rig
      .alice
      .client
      .let_guest_into_meeting_chat(
        rig.chat,
        rig.meeting,
        rig.guest_id(),
        GUEST_NAME,
        &admission,
        ACCESS_ENDS_MS,
      )
      .await
      .unwrap_err();
    assert!(matches!(refused, CoreError::GuestRefused(_)), "{refused}");
  }
  // A guest whose name was changed on the way is not the one who proved.
  let proof = rig.proof(&rig.link.secret);
  let renamed = rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      "Somebody else",
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap_err();
  assert!(
    matches!(renamed, CoreError::GuestRefused(GuestRefusal::BadProof)),
    "{renamed}"
  );

  rig.guest.client.full_sync().await.unwrap();
  assert!(!rig.guest.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn a_device_that_did_not_make_the_link_needs_the_hosts_key_code() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  let mut laptop = link_device(&mut rig.alice).await;
  laptop.full_sync().await.unwrap();
  let proof = rig.proof(&rig.link.secret);

  let no_secret = laptop
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap_err();
  assert!(
    matches!(no_secret, CoreError::GuestRefused(GuestRefusal::NoLinkSecret)),
    "{no_secret}"
  );

  let wrong = laptop
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::KeyCodeConfirmed("0000-0000-0000-0000".into()),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap_err();
  assert!(
    matches!(wrong, CoreError::GuestRefused(GuestRefusal::KeyCodeMismatch)),
    "{wrong}"
  );

  let code = rig.guest.client.own_key_code();
  laptop
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::KeyCodeConfirmed(code),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap();
  rig.guest.client.full_sync().await.unwrap();
  assert!(rig.guest.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn a_guest_device_without_a_link_anchor_joins_nothing() {
  let mut rig = rig().await;
  // No organizer key from a link: the guest's device fails closed, whatever group it is offered.
  rig.server_admits_the_guest().await;
  let proof = rig.proof(&rig.link.secret);
  rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(rig.guest.client.is_guest_device().unwrap());
  assert!(!rig.guest.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn a_guest_whose_access_ended_is_removed_even_if_the_server_says_nothing() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  let proof = rig.proof(&rig.link.secret);
  let now = NOW_MS;
  rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      now + 1_000,
    )
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  assert_eq!(
    rig
      .alice
      .client
      .verified_call_devices(rig.chat)
      .await
      .unwrap()
      .len(),
    2
  );

  // The meeting is over by Alice's clock; the server listed the guest all along.
  rig.clock.advance(5_000);
  rig.alice.client.full_sync().await.unwrap();

  assert_eq!(
    rig
      .alice
      .client
      .verified_call_devices(rig.chat)
      .await
      .unwrap()
      .len(),
    1
  );
}

#[tokio::test]
async fn a_group_the_server_made_holding_the_organizer_is_not_joined() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  // A device of the server's own founds a group and adds the organizer's published key package.
  let mut mallory = Peer::enroll(&rig.server, "Mallory").await;
  let (alice_id, guest_id) = (rig.alice.client.user_id(), rig.guest_id());
  let dm = mallory.client.open_group_dm(&[alice_id, guest_id]).await.unwrap();
  // The guest's link names the chat Mallory's group claims to be.
  rig
    .guest
    .client
    .set_guest_organizer_anchor(dm, &rig.link.organizer_fingerprint)
    .unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(
    !rig.guest.client.has_group(dm).unwrap(),
    "the organizer is in the group, but the server's device added the guest"
  );
}

#[tokio::test]
async fn only_the_organizers_devices_let_guests_in() {
  let rig = rig().await;
  let mut cohost = Peer::enroll(&rig.server, "Cohost").await;
  rig
    .alice
    .api
    .add_member(
      rig.chat,
      &AddConversationMemberRequest {
        user_id: cohost.client.user_id(),
        maybe_role: None,
      },
    )
    .await
    .unwrap();
  let refused = cohost
    .client
    .guest_link_material(rig.chat, rig.meeting)
    .await
    .unwrap_err();
  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::NotOrganizer)),
    "{refused}"
  );
}

#[tokio::test]
async fn a_guest_only_joins_a_group_the_links_organizer_founded() {
  let mut rig = rig().await;
  // The link names another key as the organizer's: the group Alice founded is not the one.
  let other = "ab".repeat(32);
  rig
    .guest
    .client
    .set_guest_organizer_anchor(rig.chat, &other)
    .unwrap();
  rig.server_admits_the_guest().await;
  let proof = rig.proof(&rig.link.secret);
  rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(
    !rig.guest.client.has_group(rig.chat).unwrap(),
    "a group founded by another key is not joined"
  );
}

#[tokio::test]
async fn a_guest_whose_session_ended_is_removed_from_the_group() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  let proof = rig.proof(&rig.link.secret);
  rig
    .alice
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.meeting,
      rig.guest_id(),
      GUEST_NAME,
      &GuestAdmission::Proof(proof),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  assert_eq!(
    rig
      .alice
      .client
      .verified_call_devices(rig.chat)
      .await
      .unwrap()
      .len(),
    2
  );

  // The server ends the guest's session (they left, or the meeting's access ended).
  rig.server.remove_member_as_admin(rig.chat, rig.guest_id());
  settle(&mut [&mut rig.alice]).await;
  rig.alice.client.full_sync().await.unwrap();

  assert_eq!(
    rig
      .alice
      .client
      .verified_call_devices(rig.chat)
      .await
      .unwrap()
      .len(),
    1,
    "the guest's leaf is out of the group"
  );
}

impl Rig {
  fn guest_id(&self) -> UserId {
    self.guest.client.user_id()
  }

  /// The guest read the organizer's key from the link.
  fn guest_knows_the_link(&self) {
    self
      .guest
      .client
      .set_guest_organizer_anchor(self.chat, &self.link.organizer_fingerprint)
      .unwrap();
  }

  /// The server lets the guest into the chat (a host's "admit"), without any encryption group.
  async fn server_admits_the_guest(&self) {
    self
      .alice
      .api
      .add_member(
        self.chat,
        &AddConversationMemberRequest {
          user_id: self.guest_id(),
          maybe_role: None,
        },
      )
      .await
      .unwrap();
  }

  fn proof(&self, secret: &str) -> Vec<u8> {
    self
      .guest
      .client
      .guest_link_proof(secret, self.meeting, GUEST_NAME)
      .unwrap()
  }
}

async fn rig() -> Rig {
  let server = FakeServer::new();
  let clock = Arc::new(ManualClock::new(NOW_MS));
  let mut alice = Peer::enroll_on_clock(
    server.add_user("Alice"),
    "Alice",
    common::settings(),
    Box::new(clock.clone()),
  )
  .await;
  let guest_api = server.add_guest(GUEST_NAME);
  let store = Arc::new(MemoryStore::new());
  let mut client = PendingEnrollment::complete_as_guest(
    guest_api.clone(),
    store.clone(),
    common::clock(),
    common::settings(),
    "Browser".into(),
    Argon2Settings {
      memory_kib: 64,
      iterations: 1,
      parallelism: 1,
    },
  )
  .await
  .unwrap();
  let (gateway, _) = client.connect_gateway().await.unwrap();
  let guest = Peer {
    name: GUEST_NAME.into(),
    api: guest_api,
    store,
    client,
    gateway,
  };
  let chat = server.create_meeting_chat(alice.client.user_id(), &[]);
  alice.client.set_up_meeting_chat(chat, &[]).await.unwrap();
  let meeting: MeetingId = "7".parse().unwrap();
  let link = alice.client.guest_link_material(chat, meeting).await.unwrap();
  Rig {
    server,
    alice,
    guest,
    chat,
    meeting,
    link,
    clock,
  }
}

fn base64_url_of(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
