//! Meeting guests: a guest's own ephemeral device joins the meeting chat's group when an
//! organizer's device lets it in; the guest only accepts a group from a device of the organizer the
//! link names; nothing from before the admission is readable; guests manage nobody (e2ee.md §4.6,
//! "Meeting guests").
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, settle};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{VERIFIER_KEY, guest_key_code, guest_link_fragment, is_open_by_own_record};
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
  /// `v` of the meeting link: the organizer's fingerprint as Alice's device has it pinned.
  fingerprint: String,
  clock: Arc<ManualClock>,
}

#[tokio::test]
async fn a_guest_let_in_by_the_organizer_reads_only_from_the_admission_on() {
  let mut rig = rig().await;
  rig
    .alice
    .client
    .send_text(rig.chat, None, "before the guest")
    .await
    .unwrap();
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;

  rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();
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
  // The welcome opens the admission's epoch only: what was said before stays unreadable.
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
async fn the_link_carries_the_organizers_fingerprint_for_every_member() {
  let rig = rig().await;
  let mut bob = rig.member("Bob").await;

  bob
    .client
    .prepare_guest_link(rig.chat, rig.meeting)
    .await
    .unwrap();

  let expected = format!("{VERIFIER_KEY}={}", rig.fingerprint);
  assert_eq!(
    guest_link_fragment(&bob.store, rig.meeting),
    Some(expected.clone())
  );
  assert_eq!(guest_link_fragment(&rig.alice.store, rig.meeting), Some(expected));
  // No secret: the fragment is the fingerprint and nothing else.
  assert_eq!(rig.fingerprint.len(), 64);
}

#[tokio::test]
async fn an_admission_by_a_device_the_link_does_not_name_is_refused() {
  let mut rig = rig().await;
  // The link names another key as the organizer's: Alice's welcome is a forged admission to it.
  let other = "ab".repeat(32);
  rig
    .guest
    .client
    .set_guest_organizer_anchor(rig.chat, &other)
    .unwrap();
  rig.server_admits_the_guest().await;
  rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(
    !rig.guest.client.has_group(rig.chat).unwrap(),
    "a welcome from a device the link's key didn't certify is not joined"
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
    .set_guest_organizer_anchor(dm, &rig.fingerprint)
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
  let mut cohost = rig.member("Cohost").await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;

  let refused = cohost
    .client
    .let_guest_into_meeting_chat(
      rig.chat,
      rig.guest_id(),
      &rig.guest.client.own_fingerprint(),
      ACCESS_ENDS_MS,
    )
    .await
    .unwrap_err();

  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::NotOrganizer)),
    "{refused}"
  );
}

#[tokio::test]
async fn a_guest_device_without_a_link_anchor_joins_nothing() {
  let mut rig = rig().await;
  // No organizer key: the guest's device fails closed, whatever group it is offered.
  rig.server_admits_the_guest().await;
  rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(rig.guest.client.is_guest_device().unwrap());
  assert!(!rig.guest.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn a_key_the_server_swaps_after_the_host_saw_its_code_is_refused() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  // The host's lobby entry shows the code of the key the guest's page reported.
  let guest = rig.guest_id();
  rig.alice.client.prepare_guest_key_code(guest).await.unwrap();
  let shown = guest_key_code(&rig.alice.store, guest).unwrap();
  assert_eq!(shown, rig.guest.client.own_key_code());

  // The server swaps in a key of its own before the host clicks "admit".
  rig.server.replace_identity_key(guest, vec![9; 32]);
  let refused = rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap_err();

  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::KeyChanged)),
    "{refused}"
  );
  rig.guest.client.full_sync().await.unwrap();
  assert!(!rig.guest.client.has_group(rig.chat).unwrap());
}

#[tokio::test]
async fn a_key_that_is_not_the_one_the_guest_reported_is_refused() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  let other = "cd".repeat(32);

  let refused = rig
    .alice
    .client
    .let_guest_into_meeting_chat(rig.chat, rig.guest_id(), &other, ACCESS_ENDS_MS)
    .await
    .unwrap_err();

  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::NotTheKnockingKey)),
    "{refused}"
  );
}

#[tokio::test]
async fn only_the_organizers_own_record_opens_a_meeting() {
  let rig = rig().await;
  // Nothing recorded (the server's `has_lobby` is no record): no admission by itself.
  assert!(!is_open_by_own_record(&rig.alice.store, rig.meeting));
  rig.alice.client.note_meeting_lobby(rig.meeting, true).unwrap();
  assert!(!is_open_by_own_record(&rig.alice.store, rig.meeting));
  rig.alice.client.note_meeting_lobby(rig.meeting, false).unwrap();
  assert!(is_open_by_own_record(&rig.alice.store, rig.meeting));
}

#[tokio::test]
async fn a_guest_whose_access_ended_is_removed_even_if_the_server_says_nothing() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  rig.organizer_lets_the_guest_in(NOW_MS + 1_000).await.unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  assert_eq!(rig.call_devices().await, 2);

  // The meeting is over by Alice's clock; the server listed the guest all along.
  rig.clock.advance(5_000);
  rig.alice.client.full_sync().await.unwrap();

  assert_eq!(rig.call_devices().await, 1);
}

#[tokio::test]
async fn a_removed_guest_reads_nothing_sent_after_the_removal() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  rig.guest.client.full_sync().await.unwrap();
  rig
    .alice
    .client
    .send_text(rig.chat, None, "while the guest is in")
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  assert_eq!(rig.call_devices().await, 2);

  // The server ends the guest's session (they left, or the meeting's access ended).
  rig.server.remove_member_as_admin(rig.chat, rig.guest_id());
  settle(&mut [&mut rig.alice]).await;
  rig.alice.client.full_sync().await.unwrap();
  rig
    .alice
    .client
    .send_text(rig.chat, None, "after the removal")
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;

  assert_eq!(
    rig.call_devices().await,
    1,
    "the guest's leaf is out of the group"
  );
  assert!(
    !rig
      .guest
      .texts(rig.chat)
      .contains(&"after the removal".to_owned()),
    "the removal started an epoch the guest has no keys for"
  );
}

impl Rig {
  fn guest_id(&self) -> UserId {
    self.guest.client.user_id()
  }

  /// The guest read the organizer's fingerprint from the link.
  fn guest_knows_the_link(&self) {
    self
      .guest
      .client
      .set_guest_organizer_anchor(self.chat, &self.fingerprint)
      .unwrap();
  }

  /// The server lets the guest into the chat (a host's "admit"), without any encryption group.
  async fn server_admits_the_guest(&self) {
    self.add_to_chat(self.guest_id()).await;
  }

  /// Alice's device, the organizer's, lets the guest's device into the group.
  async fn organizer_lets_the_guest_in(&mut self, access_ends_at_ms: i64) -> Result<(), CoreError> {
    self
      .alice
      .client
      .let_guest_into_meeting_chat(
        self.chat,
        self.guest_id(),
        &self.guest.client.own_fingerprint(),
        access_ends_at_ms,
      )
      .await
  }

  /// A member of the meeting chat on the server, who isn't the organizer.
  async fn member(&self, name: &str) -> Peer {
    let peer = Peer::enroll(&self.server, name).await;
    self.add_to_chat(peer.client.user_id()).await;
    peer
  }

  async fn add_to_chat(&self, user: UserId) {
    self
      .alice
      .api
      .add_member(
        self.chat,
        &AddConversationMemberRequest {
          user_id: user,
          maybe_role: None,
        },
      )
      .await
      .unwrap();
  }

  async fn call_devices(&mut self) -> usize {
    self
      .alice
      .client
      .verified_call_devices(self.chat)
      .await
      .unwrap()
      .len()
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
  alice.client.prepare_guest_link(chat, meeting).await.unwrap();
  let fingerprint = guest_link_fragment(&alice.store, meeting)
    .unwrap()
    .strip_prefix(&format!("{VERIFIER_KEY}="))
    .unwrap()
    .to_owned();
  Rig {
    server,
    alice,
    guest,
    chat,
    meeting,
    fingerprint,
    clock,
  }
}
