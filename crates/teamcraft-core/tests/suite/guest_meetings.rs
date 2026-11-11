//! Meeting guests: a guest's own ephemeral device joins the meeting chat's group when an
//! organizer's device, or an admitter's the organizer signed (#1103), lets it in; the guest only
//! accepts a group from a device of the organizer the link names or of such an admitter; nothing
//! from before the admission is readable; guests manage nobody (e2ee.md §4.6, "Meeting guests").
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use crate::common;

use common::{Peer, settle};
use teamcraft_core::PendingEnrollment;
use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::FakeServer;
use teamcraft_core::client::{
  VERIFIER_KEY, admits_link_arrivals, guest_key_code, guest_link_fragment, is_open_by_own_record,
};
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
  guest_clock: Arc<ManualClock>,
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

/// A guest the server admitted, who starts the call before the organizer's device let them in
/// (#796): told at once, and a retry gets in afterwards.
#[tokio::test]
async fn a_guest_is_told_clearly_until_the_organizer_let_them_in() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  assert!(matches!(
    rig.guest.client.prepare_call(rig.chat).await,
    Err(CoreError::CallEncryptionPending(id)) if id == rig.chat
  ));
  assert!(
    !rig.guest.client.has_group(rig.chat).unwrap(),
    "a guest never founds"
  );

  rig.organizer_lets_the_guest_in(NOW_MS + 3_600_000).await.unwrap();
  let prepared = rig.guest.client.prepare_call(rig.chat).await.unwrap();
  assert_eq!(prepared.devices.len(), 2, "the retry gets in");
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
    .set_guest_organizer_anchor(rig.chat, rig.meeting, &other)
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
    .set_guest_organizer_anchor(dm, rig.meeting, &rig.fingerprint)
    .unwrap();

  rig.guest.client.full_sync().await.unwrap();

  assert!(
    !rig.guest.client.has_group(dm).unwrap(),
    "the organizer is in the group, but the server's device added the guest"
  );
}

#[tokio::test]
async fn a_member_the_organizer_named_no_admitter_lets_no_guest_in() {
  let mut rig = rig().await;
  let mut cohost = rig.member("Cohost").await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;

  // Nothing published: only the organizer's devices let guests in.
  let refused = rig.member_lets_the_guest_in(&mut cohost).await.unwrap_err();
  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::NotAnAdmitter)),
    "{refused}"
  );

  // Published while the cohost wasn't in the group yet: still not named.
  rig.publish_admitters().await;
  rig.let_into_group(&mut cohost).await;
  let refused = rig.member_lets_the_guest_in(&mut cohost).await.unwrap_err();
  assert!(
    matches!(refused, CoreError::GuestRefused(GuestRefusal::NotAnAdmitter)),
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

/// #1077: a guest's device leaves the organizer's new device to the members, who would refuse its
/// commit and split the group; the guest still follows the group the members' commit makes.
#[tokio::test]
async fn a_guests_device_adds_no_other_users_new_device() {
  let mut rig = rig().await;
  rig.guest_knows_the_link();
  rig.server_admits_the_guest().await;
  rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  rig.guest.client.full_sync().await.unwrap();
  assert!(rig.guest.client.has_group(rig.chat).unwrap());

  let _laptop = common::link_device(&mut rig.alice).await;
  let added_by_guest = rig.guest.client.add_missing_devices(rig.chat).await.unwrap();
  let added_by_alice = rig.alice.client.add_missing_devices(rig.chat).await.unwrap();
  rig
    .alice
    .client
    .send_text(rig.chat, None, "with the laptop in")
    .await
    .unwrap();
  settle(&mut [&mut rig.alice, &mut rig.guest]).await;
  rig.guest.client.full_sync().await.unwrap();

  assert_eq!((added_by_guest, added_by_alice), (0, 1));
  assert_eq!(rig.guest.texts(rig.chat), vec!["with the laptop in"]);
}

/// Admitters the organizer signs (#1103): other members' devices let guests in, and the guest
/// checks the list against the organizer's key from the link.
mod admitters {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::{IdentityKeyPair, IdentityPublicKey};
  use teamcraft_crypto::ids::{
    ConversationId as SignedChatId, MeetingId as SignedMeetingId, UserId as SignedUserId,
  };
  use teamcraft_crypto::meeting_admitters::{Admitter, AdmittersDraft, LinkAdmission};

  use super::*;

  #[tokio::test]
  async fn an_admitter_the_organizer_signed_lets_a_guest_in_who_reads_from_then_on() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    assert!(rig.publish_admitters().await);
    bob
      .client
      .send_text(rig.chat, None, "before the guest")
      .await
      .unwrap();
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    // The organizer is away: Bob's device lets the guest in.
    rig.member_lets_the_guest_in(&mut bob).await.unwrap();
    bob
      .client
      .send_text(rig.chat, None, "after the guest")
      .await
      .unwrap();
    settle(&mut [&mut bob, &mut rig.guest]).await;
    rig.guest.client.full_sync().await.unwrap();

    assert!(rig.guest.client.has_group(rig.chat).unwrap());
    assert_eq!(rig.guest.texts(rig.chat), vec!["after the guest"]);
    // The organizer's device takes the guest's add from Bob.
    settle(&mut [&mut rig.alice]).await;
    rig.alice.client.full_sync().await.unwrap();
    assert_eq!(rig.call_devices().await, 3);
  }

  #[tokio::test]
  async fn the_admitters_device_takes_its_guest_out_when_the_access_ends() {
    let mut rig = rig().await;
    let bob_clock = Arc::new(ManualClock::new(NOW_MS));
    let mut bob = Peer::enroll_on_clock(
      rig.server.add_user("Bob"),
      "Bob",
      common::settings(),
      Box::new(bob_clock.clone()),
    )
    .await;
    rig.add_to_chat(bob.client.user_id()).await;
    rig.let_into_group(&mut bob).await;
    rig.publish_admitters().await;
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;
    bob
      .client
      .let_guest_into_meeting_chat(
        rig.chat,
        rig.guest_id(),
        &rig.guest.client.own_fingerprint(),
        NOW_MS + 1_000,
      )
      .await
      .unwrap();
    settle(&mut [&mut rig.alice, &mut bob, &mut rig.guest]).await;
    rig.guest.client.full_sync().await.unwrap();
    assert!(rig.guest.client.has_group(rig.chat).unwrap());
    assert_eq!(rig.call_devices().await, 3);

    // The meeting is over by Bob's clock; the server listed the guest all along. Bob is no roster
    // member, but the organizer named him: his removal of the guest counts.
    bob_clock.advance(5_000);
    bob.client.full_sync().await.unwrap();
    settle(&mut [&mut rig.alice, &mut bob]).await;
    rig.alice.client.full_sync().await.unwrap();

    assert_eq!(rig.call_devices().await, 2);
  }

  #[tokio::test]
  async fn a_member_off_the_list_adds_nobody_even_past_the_guests_check() {
    let mut rig = rig().await;
    rig.publish_admitters().await;
    // Carol joined after the organizer signed, and tries the plain add, which skips the
    // admitters check of `let_guest_into_meeting_chat`.
    let mut carol = rig.member("Carol").await;
    rig.let_into_group(&mut carol).await;
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    let refused = carol
      .client
      .let_into_meeting_chat(rig.chat, rig.guest_id())
      .await
      .unwrap_err();
    rig.guest.client.full_sync().await.unwrap();

    // The members' rule refuses the change (her own device first, every other one alike), and
    // the guest's check would refuse her welcome too.
    assert!(
      matches!(
        refused,
        CoreError::Mls(teamcraft_crypto::mls::MlsError::Membership(_))
      ),
      "{refused}"
    );
    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn a_list_signed_by_another_key_lets_no_admitter_in() {
    let mut rig = rig().await;
    let mut bob = rig.admitter_lets_the_guest_in().await;
    // After Bob's device checked the organizer's list, the server serves one it signed itself,
    // naming Bob for this very meeting and chat.
    let bob_key = rig.identity_key_of(&bob).await;
    let forged = AdmittersDraft {
      sequence: u64::MAX / 2,
      ..rig.draft(vec![Admitter {
        user_id: signed_user(bob.client.user_id()),
        identity_key: bob_key,
      }])
    }
    .sign(&IdentityKeyPair::generate(&mut OsRng))
    .unwrap();
    rig
      .server
      .set_meeting_admitters(rig.chat, forged.to_bytes().unwrap());

    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
    let _ = &mut bob;
  }

  #[tokio::test]
  async fn the_organizers_list_for_another_meeting_lets_no_admitter_in() {
    let mut rig = rig().await;
    let bob = rig.admitter_lets_the_guest_in().await;
    // The organizer also signed Bob as an admitter of another meeting, with a chat of its own; the
    // server serves that list for this chat.
    let other_chat = rig
      .server
      .create_meeting_chat(rig.alice.client.user_id(), &[bob.client.user_id()]);
    rig
      .alice
      .client
      .set_up_meeting_chat(other_chat, &[bob.client.user_id()])
      .await
      .unwrap();
    assert!(
      rig
        .alice
        .client
        .sync_meeting_admitters("8".parse().unwrap(), other_chat, ACCESS_ENDS_MS)
        .await
        .unwrap()
    );
    let moved = rig.server.meeting_admitters(other_chat).unwrap();
    rig.server.set_meeting_admitters(rig.chat, moved);

    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn the_organizers_list_naming_another_meeting_for_this_chat_lets_no_admitter_in() {
    let mut rig = rig().await;
    let _bob = rig.admitter_lets_the_guest_in().await;
    // Signed for this chat but under another meeting than the one the guest joined.
    rig
      .alice
      .client
      .sync_meeting_admitters("8".parse().unwrap(), rig.chat, ACCESS_ENDS_MS)
      .await
      .unwrap();

    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn an_older_list_than_the_guest_saw_lets_no_admitter_in() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    rig.publish_admitters().await;
    let older = rig.server.meeting_admitters(rig.chat).unwrap();
    // The organizer opens the meeting: a newer list, which the admitted guest sees.
    rig.alice.client.note_meeting_lobby(rig.meeting, false).unwrap();
    assert!(rig.publish_admitters().await);
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;
    rig.guest.client.check_meeting_admission(rig.chat).await.unwrap();
    rig.member_lets_the_guest_in(&mut bob).await.unwrap();

    // The server brings the older list back (it names Bob as well, but is older).
    rig.server.set_meeting_admitters(rig.chat, older);
    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn an_expired_list_lets_no_admitter_in() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    rig
      .alice
      .client
      .sync_meeting_admitters(rig.meeting, rig.chat, NOW_MS + 1_000)
      .await
      .unwrap();
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;
    rig.member_lets_the_guest_in(&mut bob).await.unwrap();

    // By the guest's clock the list ended before the welcome is opened.
    rig.guest_clock.advance(5_000);
    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn the_organizer_signs_anew_only_when_the_admitters_change() {
    let mut rig = rig().await;
    assert!(rig.publish_admitters().await, "the first list");
    assert!(!rig.publish_admitters().await, "nothing changed");
    let first = rig.server.meeting_admitters(rig.chat).unwrap();

    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    assert!(rig.publish_admitters().await, "Bob joined the group");
    // A guest in the group is never an admitter.
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;
    rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();
    assert!(!rig.publish_admitters().await, "the guest changes nothing");
    rig.alice.client.note_meeting_lobby(rig.meeting, false).unwrap();
    assert!(rig.publish_admitters().await, "no lobby any more");
    // A longer meeting renews the list, a shorter one keeps it.
    assert!(
      rig
        .alice
        .client
        .sync_meeting_admitters(rig.meeting, rig.chat, ACCESS_ENDS_MS + 1)
        .await
        .unwrap()
    );
    assert!(
      !rig
        .alice
        .client
        .sync_meeting_admitters(rig.meeting, rig.chat, NOW_MS)
        .await
        .unwrap()
    );
    // The server refuses the first list again.
    let refused = rig
      .alice
      .api
      .put_meeting_admitters(
        rig.chat,
        &teamcraft_proto::PutMeetingAdmittersRequest {
          statement: teamcraft_proto::Base64Bytes::new(first),
        },
      )
      .await
      .unwrap_err();
    assert!(refused.is_conflict(), "{refused}");
  }

  #[tokio::test]
  async fn members_let_link_arrivals_in_only_where_the_organizer_signed_no_lobby() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    let mut carol = rig.member("Carol").await;
    rig.publish_admitters().await;
    check(&mut bob, rig.chat).await;
    assert!(
      !admits_link_arrivals(&bob.store, rig.chat, NOW_MS),
      "the organizer kept the lobby"
    );

    rig.alice.client.note_meeting_lobby(rig.meeting, false).unwrap();
    rig.publish_admitters().await;
    check(&mut bob, rig.chat).await;
    check(&mut carol, rig.chat).await;

    assert!(admits_link_arrivals(&bob.store, rig.chat, NOW_MS));
    assert!(
      !admits_link_arrivals(&bob.store, rig.chat, ACCESS_ENDS_MS + 1),
      "not once the list ended"
    );
    assert!(
      !admits_link_arrivals(&carol.store, rig.chat, NOW_MS),
      "Carol is not on the list"
    );
  }

  impl Rig {
    /// Bob, in the group and on the organizer's list, lets the guest in; the guest has not opened
    /// the welcome yet.
    async fn admitter_lets_the_guest_in(&mut self) -> Peer {
      let mut bob = self.member("Bob").await;
      self.let_into_group(&mut bob).await;
      self.publish_admitters().await;
      self.guest_knows_the_link();
      self.server_admits_the_guest().await;
      self.member_lets_the_guest_in(&mut bob).await.unwrap();
      bob
    }

    /// A list for this meeting and chat, unsigned yet.
    fn draft(&self, admitters: Vec<Admitter>) -> AdmittersDraft {
      AdmittersDraft {
        meeting_id: SignedMeetingId::new(self.meeting.to_string().parse().unwrap()),
        conversation_id: SignedChatId::new(self.chat.to_string().parse().unwrap()),
        sequence: 1,
        expires_at_ms: u64::try_from(ACCESS_ENDS_MS).unwrap(),
        link_admission: LinkAdmission::Lobby,
        admitters,
      }
    }

    /// The identity key the server holds for a peer.
    async fn identity_key_of(&self, peer: &Peer) -> IdentityPublicKey {
      let response = self
        .alice
        .api
        .identity_key(peer.client.user_id())
        .await
        .unwrap()
        .unwrap();
      IdentityPublicKey::from_bytes(response.public_key.as_slice().try_into().unwrap())
    }
  }

  async fn check(peer: &mut Peer, chat: ConversationId) {
    peer.client.check_meeting_admission(chat).await.unwrap();
  }

  fn signed_user(user: UserId) -> SignedUserId {
    SignedUserId::new(user.to_string().parse().unwrap())
  }
}

/// Co-hosts and handing the meeting over (#1720): the host lists the organizer signs, which a
/// guest follows from the key of its link and a member from the group's founder.
mod hosts {
  use rand_core::OsRng;
  use teamcraft_core::client::HostsChange;
  use teamcraft_crypto::identity::{IdentityKeyPair, IdentityPublicKey};
  use teamcraft_crypto::ids::{
    ConversationId as SignedChatId, MeetingId as SignedMeetingId, UserId as SignedUserId,
  };
  use teamcraft_crypto::meeting_hosts::{Host, HostsDraft};

  use super::*;

  #[tokio::test]
  async fn a_co_host_lets_a_guest_in_while_the_organizer_is_away() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    // No admitters were published: only the host list names Bob.
    rig.name_hosts(&[bob.client.user_id()], None).await;
    bob
      .client
      .send_text(rig.chat, None, "before the guest")
      .await
      .unwrap();
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    // The organizer left the call; Bob's device lets the guest in.
    rig.member_lets_the_guest_in(&mut bob).await.unwrap();
    bob
      .client
      .send_text(rig.chat, None, "after the guest")
      .await
      .unwrap();
    settle(&mut [&mut bob, &mut rig.guest]).await;
    rig.guest.client.full_sync().await.unwrap();

    assert!(rig.guest.client.has_group(rig.chat).unwrap());
    assert_eq!(rig.guest.texts(rig.chat), vec!["after the guest"]);
    // The organizer's device takes the co-host's add when it is back.
    settle(&mut [&mut rig.alice]).await;
    rig.alice.client.full_sync().await.unwrap();
    assert_eq!(rig.call_devices().await, 3);
  }

  #[tokio::test]
  async fn a_co_host_the_organizer_took_off_lets_no_guest_in() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    rig.name_hosts(&[bob.client.user_id()], None).await;
    rig.name_hosts(&[], None).await;
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    let refused = rig.member_lets_the_guest_in(&mut bob).await.unwrap_err();

    assert!(
      matches!(refused, CoreError::GuestRefused(GuestRefusal::NotAnAdmitter)),
      "{refused}"
    );
  }

  #[tokio::test]
  async fn a_host_list_the_server_forges_lets_nobody_in() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    // The server serves a list in the organizer's name, signed with a key of its own.
    let forged = rig.host_draft(rig.alice.client.user_id(), &bob).await;
    let forger = IdentityKeyPair::generate(&mut OsRng);
    let forged = HostsDraft {
      organizer: Host {
        user_id: signed_user(rig.alice.client.user_id()),
        identity_key: forger.public_key(),
      },
      ..forged
    }
    .sign(&forger)
    .unwrap();
    rig
      .server
      .set_meeting_hosts(rig.chat, vec![forged.to_bytes().unwrap()]);
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    let refused = rig.member_lets_the_guest_in(&mut bob).await.unwrap_err();

    assert!(
      matches!(refused, CoreError::GuestRefused(GuestRefusal::NotAnAdmitter)),
      "{refused}"
    );
  }

  #[tokio::test]
  async fn a_guest_refuses_a_co_hosts_welcome_once_the_server_swaps_the_list() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    rig.name_hosts(&[bob.client.user_id()], None).await;
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;
    rig.member_lets_the_guest_in(&mut bob).await.unwrap();
    // Before the guest opens the welcome, the server swaps the organizer's list for one naming
    // Bob that another key signed.
    let forger = IdentityKeyPair::generate(&mut OsRng);
    let forged = rig
      .host_draft(rig.alice.client.user_id(), &bob)
      .await
      .sign(&forger)
      .unwrap();
    rig
      .server
      .set_meeting_hosts(rig.chat, vec![forged.to_bytes().unwrap()]);

    rig.guest.client.full_sync().await.unwrap();

    assert!(!rig.guest.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn a_guest_with_a_link_from_before_the_handover_is_let_in_by_the_new_organizer() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    // Alice hands the meeting to Bob and stays co-host.
    let alice_id = rig.alice.client.user_id();
    rig.name_hosts(&[alice_id], Some(bob.client.user_id())).await;
    let organizer = bob
      .api
      .conversation(rig.chat)
      .await
      .unwrap()
      .maybe_organizer_user_id;
    assert_eq!(organizer, Some(bob.client.user_id()));
    // The guest has the link Alice copied before: her key.
    rig.guest_knows_the_link();
    rig.server_admits_the_guest().await;

    rig.member_lets_the_guest_in(&mut bob).await.unwrap();
    bob
      .client
      .send_text(rig.chat, None, "from the new organizer")
      .await
      .unwrap();
    settle(&mut [&mut bob, &mut rig.guest]).await;
    rig.guest.client.full_sync().await.unwrap();

    assert!(rig.guest.client.has_group(rig.chat).unwrap());
    assert_eq!(rig.guest.texts(rig.chat), vec!["from the new organizer"]);
  }

  #[tokio::test]
  async fn after_the_handover_links_carry_the_new_key_and_the_former_organizer_still_admits() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    let alice_id = rig.alice.client.user_id();
    rig.name_hosts(&[alice_id], Some(bob.client.user_id())).await;
    // Bob's device signs the hosts again with his key (it does so with the admitters' upkeep).
    assert!(
      bob
        .client
        .sync_meeting_hosts(rig.meeting, rig.chat)
        .await
        .unwrap()
    );
    assert!(
      !bob
        .client
        .sync_meeting_hosts(rig.meeting, rig.chat)
        .await
        .unwrap(),
      "once"
    );
    bob
      .client
      .prepare_guest_link(rig.chat, rig.meeting)
      .await
      .unwrap();
    let new_fingerprint = guest_link_fragment(&bob.store, rig.meeting)
      .unwrap()
      .strip_prefix(&format!("{VERIFIER_KEY}="))
      .unwrap()
      .to_owned();
    assert_ne!(new_fingerprint, rig.fingerprint, "Bob's key now");
    rig
      .guest
      .client
      .set_guest_organizer_anchor(rig.chat, rig.meeting, &new_fingerprint)
      .unwrap();
    rig.server_admits_the_guest().await;

    // Alice, co-host now, lets the guest with Bob's link in.
    rig.organizer_lets_the_guest_in(ACCESS_ENDS_MS).await.unwrap();
    settle(&mut [&mut rig.alice, &mut bob, &mut rig.guest]).await;
    rig.guest.client.full_sync().await.unwrap();

    assert!(rig.guest.client.has_group(rig.chat).unwrap());
    // Bob's device takes his co-host's add.
    bob.client.full_sync().await.unwrap();
    assert_eq!(bob.client.verified_call_devices(rig.chat).await.unwrap().len(), 3);
  }

  #[tokio::test]
  async fn a_member_added_after_the_handover_joins_the_group_the_former_organizer_founded() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;
    rig
      .name_hosts(&[rig.alice.client.user_id()], Some(bob.client.user_id()))
      .await;
    let mut carol = rig.member("Carol").await;

    bob
      .client
      .let_into_meeting_chat(rig.chat, carol.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut bob, &mut carol]).await;
    carol.client.full_sync().await.unwrap();

    assert!(carol.client.has_group(rig.chat).unwrap());
  }

  #[tokio::test]
  async fn only_the_organizer_names_hosts() {
    let mut rig = rig().await;
    let mut bob = rig.member("Bob").await;
    rig.let_into_group(&mut bob).await;

    let refused = bob
      .client
      .change_meeting_hosts(&HostsChange {
        meeting: rig.meeting,
        conversation: rig.chat,
        co_hosts: vec![bob.client.user_id()],
        maybe_new_organizer: None,
      })
      .await
      .unwrap_err();

    assert!(matches!(refused, CoreError::NotMeetingOrganizer(_)), "{refused}");
    assert!(matches!(
      teamcraft_core::client::meeting_hosts_change(&bob.store, rig.chat),
      Some(teamcraft_core::client::HostsChangeRecord::Failed { .. })
    ));
    assert!(rig.server.meeting_hosts(rig.chat).is_empty());
  }

  impl Rig {
    /// Alice's device names the co-hosts, or hands the meeting over.
    async fn name_hosts(&mut self, co_hosts: &[UserId], maybe_new_organizer: Option<UserId>) {
      self
        .alice
        .client
        .change_meeting_hosts(&HostsChange {
          meeting: self.meeting,
          conversation: self.chat,
          co_hosts: co_hosts.to_vec(),
          maybe_new_organizer,
        })
        .await
        .unwrap();
      assert_eq!(
        teamcraft_core::client::meeting_hosts_change(&self.alice.store, self.chat),
        Some(teamcraft_core::client::HostsChangeRecord::Saved)
      );
    }

    /// A list for this meeting and chat naming `co_host`, signed as `signer`, unsigned yet.
    async fn host_draft(&self, signer: UserId, co_host: &Peer) -> HostsDraft {
      let response = self
        .alice
        .api
        .identity_key(co_host.client.user_id())
        .await
        .unwrap()
        .unwrap();
      let identity_key = IdentityPublicKey::from_bytes(response.public_key.as_slice().try_into().unwrap());
      HostsDraft {
        meeting_id: SignedMeetingId::new(self.meeting.to_string().parse().unwrap()),
        conversation_id: SignedChatId::new(self.chat.to_string().parse().unwrap()),
        sequence: u64::MAX / 2,
        signer_user_id: signed_user(signer),
        organizer: Host {
          user_id: signed_user(signer),
          identity_key,
        },
        co_hosts: vec![Host {
          user_id: signed_user(co_host.client.user_id()),
          identity_key,
        }],
      }
    }
  }

  fn signed_user(user: UserId) -> SignedUserId {
    SignedUserId::new(user.to_string().parse().unwrap())
  }
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
      .set_guest_organizer_anchor(self.chat, self.meeting, &self.fingerprint)
      .unwrap();
  }

  /// A member's device lets the guest's device into the group.
  async fn member_lets_the_guest_in(&self, member: &mut Peer) -> Result<(), CoreError> {
    member
      .client
      .let_guest_into_meeting_chat(
        self.chat,
        self.guest_id(),
        &self.guest.client.own_fingerprint(),
        ACCESS_ENDS_MS,
      )
      .await
  }

  /// Alice's device lets a member into the meeting chat's group, and the member's device joins.
  async fn let_into_group(&mut self, member: &mut Peer) {
    self
      .alice
      .client
      .let_into_meeting_chat(self.chat, member.client.user_id())
      .await
      .unwrap();
    settle(&mut [&mut self.alice, member]).await;
    member.client.full_sync().await.unwrap();
    assert!(member.client.has_group(self.chat).unwrap());
  }

  /// Alice's device signs and publishes who may let guests in, as its group stands.
  async fn publish_admitters(&mut self) -> bool {
    self
      .alice
      .client
      .sync_meeting_admitters(self.meeting, self.chat, ACCESS_ENDS_MS)
      .await
      .unwrap()
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
  let guest_clock = Arc::new(ManualClock::new(NOW_MS));
  let mut client = PendingEnrollment::complete_as_guest(
    guest_api.clone(),
    store.clone(),
    Box::new(guest_clock.clone()),
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
    guest_clock,
  }
}
