use openmls::prelude::{KeyPackageIn, ProtocolVersion};
use rand_core::OsRng;

use super::*;
use crate::content_key::{ContentKey, ContentKeyRing};
use crate::ids::{ClientMessageId, DeviceId, UserId};
use crate::membership::tests::{TestAuthority, TestDirectory, TestUser};
use crate::membership::{HistoryShare, MembershipError};
use crate::message::{MessageContext, MessageError, decrypt_message, encrypt_message};
use crate::mls::{DefaultMlsProvider, create_key_package};

const CONVERSATION: ConversationId = ConversationId::new(42);
const ALICE: u64 = 1;
const BOB: u64 = 2;
const CAROL: u64 = 3;
const DAVE: u64 = 4;

#[test]
fn add_member_and_share_full_history() {
  let mut world = World::new();
  let message_before_bob = world.alice_message(b"before Bob joined");

  let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  let bob_key_package = world.bob.key_package();
  let output = world.commit_change(&change, &[bob_key_package]);
  let update = world
    .carol_processes(&output.commit)
    .expect("Carol accepts the signed add");
  let GroupEvent::MembershipChanged(update) = update else {
    panic!("expected a membership change")
  };
  assert_eq!(update.added, vec![member(&world.bob.user)]);
  assert_eq!(
    update.maybe_change.as_ref().map(MembershipChange::change_id),
    Some(change.unverified().change_id())
  );

  let mut bob_group = world.bob_joins(&output);
  assert_eq!(bob_group.epoch(), world.alice_group.epoch());
  assert_eq!(bob_group.members().unwrap().len(), 3);
  let verified = bob_group.verified_members(&world.directory).unwrap();
  let ids: Vec<_> = verified.iter().map(VerifiedDevice::device_id).collect();
  let members: Vec<_> = bob_group
    .members()
    .unwrap()
    .iter()
    .map(|member| member.device_id)
    .collect();
  assert_eq!(ids, members, "final certificates with the members' device ids");

  let bundle = ContentKeyBundle::full_history(&mut world.alice_ring, &mut OsRng);
  let key_message = world
    .alice_group
    .send_content_keys(&world.alice.provider, &world.alice.user.device, &bundle)
    .unwrap();
  let mut bob_ring = ContentKeyRing::new(CONVERSATION);
  apply_keys(
    &mut bob_group,
    &world.bob,
    &key_message,
    &world.directory,
    &world.authority,
    &mut bob_ring,
  );

  let plaintext = decrypt_message(
    &bob_ring,
    &world.alice_context(),
    &message_before_bob,
    &world.alice_key(),
  )
  .unwrap();
  assert_eq!(plaintext.expose_secret(), b"before Bob joined");
}

#[test]
fn from_join_history_hides_earlier_messages() {
  let mut world = World::new();
  let message_before_bob = world.alice_message(b"secret plans");
  let change = world.signed_change(ALICE, add(BOB, HistoryShare::FromJoin));
  let output = world.commit_change(&change, &[world.bob.key_package()]);
  let mut bob_group = world.bob_joins(&output);

  // The members switch to the new epoch's key; Bob derives the same one and gets it as current.
  let epoch_key = world.alice_rotates_to_epoch_key();
  assert_eq!(
    bob_group.epoch_content_key(&world.bob.provider).unwrap().id(),
    epoch_key.id()
  );
  let bundle = ContentKeyBundle::epoch_key(CONVERSATION, epoch_key);
  let key_message = world
    .alice_group
    .send_content_keys(&world.alice.provider, &world.alice.user.device, &bundle)
    .unwrap();
  let mut bob_ring = ContentKeyRing::new(CONVERSATION);
  apply_keys(
    &mut bob_group,
    &world.bob,
    &key_message,
    &world.directory,
    &world.authority,
    &mut bob_ring,
  );

  assert!(matches!(
    decrypt_message(
      &bob_ring,
      &world.alice_context(),
      &message_before_bob,
      &world.alice_key()
    ),
    Err(MessageError::UnknownKey(_))
  ));
  let message_after = world.alice_message(b"welcome Bob");
  assert_eq!(
    decrypt_message(
      &bob_ring,
      &world.alice_context(),
      &message_after,
      &world.alice_key()
    )
    .unwrap()
    .expose_secret(),
    b"welcome Bob"
  );
}

#[test]
fn removed_member_cannot_read_new_messages() {
  let mut world = World::new();
  let add_bob = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  let output = world.commit_change(&add_bob, &[world.bob.key_package()]);
  world.carol_processes(&output.commit).unwrap();
  let mut bob_group = world.bob_joins(&output);

  let remove_bob = world.signed_change(
    ALICE,
    MembershipAction::RemoveUser {
      user_id: UserId::new(BOB),
    },
  );
  let output = world.commit_change(&remove_bob, &[]);
  assert!(output.maybe_welcome.is_none());
  let GroupEvent::MembershipChanged(update) = world.carol_processes(&output.commit).unwrap() else {
    panic!("expected a membership change")
  };
  assert_eq!(update.removed, vec![member(&world.bob.user)]);
  let GroupEvent::MembershipChanged(bob_update) = bob_group
    .process_message(
      &world.bob.provider,
      &output.commit,
      &world.directory,
      &world.authority,
    )
    .unwrap()
  else {
    panic!("expected a membership change")
  };
  assert!(bob_update.is_self_removed);

  // Rotation after removal (e2ee.md §4.2): every remaining member derives the new epoch's key
  // when it merges the removal; there is no separate message the server could hold back.
  let epoch_key = world.alice_rotates_to_epoch_key();
  assert_eq!(
    world
      .carol_group
      .epoch_content_key(&world.carol.provider)
      .unwrap()
      .id(),
    epoch_key.id()
  );
  let bobs_old_ring = ContentKeyRing::new(CONVERSATION);
  let message_after = world.alice_message(b"after Bob left");
  assert!(matches!(
    decrypt_message(
      &bobs_old_ring,
      &world.alice_context(),
      &message_after,
      &world.alice_key()
    ),
    Err(MessageError::UnknownKey(_))
  ));
}

#[test]
fn epoch_keys_differ_per_epoch_and_conversation() {
  let mut world = World::new();
  let first = world
    .alice_group
    .epoch_content_key(&world.alice.provider)
    .unwrap();
  let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  world.commit_change(&change, &[world.bob.key_package()]);
  let second = world
    .alice_group
    .epoch_content_key(&world.alice.provider)
    .unwrap();
  assert_ne!(first.id(), second.id());
}

#[test]
fn device_addition_needs_only_cross_signature() {
  let mut world = World::new();
  let carol_phone = Client::from_user(world.carol.user.new_device(301));
  let key_package = carol_phone.key_package();
  let output = world
    .carol_group
    .commit_device_additions(
      &world.carol.provider,
      &world.carol.user.device,
      &[key_package],
      &world.directory,
    )
    .unwrap();
  world.carol_group.confirm_commit(&world.carol.provider).unwrap();

  let event = world.alice_group.process_message(
    &world.alice.provider,
    &output.commit,
    &world.directory,
    &world.authority,
  );
  let Ok(GroupEvent::MembershipChanged(update)) = event else {
    panic!("expected a membership change")
  };
  assert!(update.maybe_change.is_none());
  assert_eq!(update.added, vec![member(&carol_phone.user)]);

  let welcome = output.maybe_welcome.unwrap();
  let phone_group =
    ConversationGroup::join(&carol_phone.provider, &welcome, CONVERSATION, &world.directory).unwrap();
  assert_eq!(phone_group.members().unwrap().len(), 3);
}

#[test]
fn discarded_commit_leaves_group_unchanged() {
  let mut world = World::new();
  let epoch = world.alice_group.epoch();
  let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  world
    .alice_group
    .commit_membership_change(
      &world.alice.provider,
      &world.alice.user.device,
      &change,
      &[world.bob.key_package()],
      &world.directory,
      &world.authority,
    )
    .unwrap();
  world.alice_group.discard_commit(&world.alice.provider).unwrap();

  assert_eq!(world.alice_group.epoch(), epoch);
  assert!(matches!(
    world.alice_group.confirm_commit(&world.alice.provider),
    Err(MlsError::NoPendingCommit)
  ));
  // The change was not applied, so it can still be committed later.
  world.commit_change(&change, &[world.bob.key_package()]);
}

mod rejection {
  use super::*;

  #[test]
  fn committer_rejects_unauthorized_change() {
    let mut world = World::new();
    let change = world.signed_change(CAROL, add(BOB, HistoryShare::Full));
    let result = world.carol_group.commit_membership_change(
      &world.carol.provider,
      &world.carol.user.device,
      &change,
      &[world.bob.key_package()],
      &world.directory,
      &world.authority,
    );
    assert!(matches!(
      result,
      Err(MlsError::Membership(MembershipError::Unauthorized { .. }))
    ));
  }

  #[test]
  fn member_rejects_unsigned_add() {
    // A malicious client (or the server via a compromised member) adds Dave without any change.
    let mut world = World::new();
    let dave = world.dave.key_package();
    let commit = world.raw_alice_commit(&[dave], &[], Vec::new());
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::MissingSignedChange));
  }

  #[test]
  fn member_rejects_unsigned_remove() {
    let mut world = World::new();
    let carol_leaf = world.leaf_of(CAROL);
    let commit = world.raw_alice_commit(&[], &[carol_leaf], Vec::new());
    let result =
      world
        .carol_group
        .process_message(&world.carol.provider, &commit, &world.directory, &world.authority);
    assert!(matches!(result, Err(MlsError::MissingSignedChange)));
  }

  #[test]
  fn member_rejects_change_signed_by_non_admin() {
    let mut world = World::new();
    let change = world.signed_change(CAROL, add(DAVE, HistoryShare::Full));
    let dave = world.dave.key_package();
    let commit = world.raw_alice_commit(&[dave], &[], change.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| {
      matches!(error, MlsError::Membership(MembershipError::Unauthorized { .. }))
    });
  }

  #[test]
  fn member_rejects_wrongly_signed_change() {
    let mut world = World::new();
    let change = world.signed_change(ALICE, add(DAVE, HistoryShare::Full));
    let mut bytes = change.to_bytes().unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1; // flips a bit of the signature
    let dave = world.dave.key_package();
    let commit = world.raw_alice_commit(&[dave], &[], bytes);
    world.expect_carol_rejects(&commit, |error| {
      matches!(error, MlsError::Membership(MembershipError::InvalidSignature))
    });
  }

  #[test]
  fn member_rejects_commit_that_does_more_than_the_change() {
    // Signed "add Bob", but the commit adds Dave as well.
    let mut world = World::new();
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let key_packages = [world.bob.key_package(), world.dave.key_package()];
    let commit = world.raw_alice_commit(&key_packages, &[], change.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| {
      matches!(error, MlsError::ChangeDoesNotMatchCommit)
    });
  }

  #[test]
  fn member_rejects_partial_user_removal() {
    // Signed "remove Carol", but one of Carol's devices stays in the group.
    let mut world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let output = world
      .carol_group
      .commit_device_additions(
        &world.carol.provider,
        &world.carol.user.device,
        &[carol_phone.key_package()],
        &world.directory,
      )
      .unwrap();
    world.carol_group.confirm_commit(&world.carol.provider).unwrap();
    world
      .alice_group
      .process_message(
        &world.alice.provider,
        &output.commit,
        &world.directory,
        &world.authority,
      )
      .unwrap();
    let mut phone_group = ConversationGroup::join(
      &carol_phone.provider,
      &output.maybe_welcome.unwrap(),
      CONVERSATION,
      &world.directory,
    )
    .unwrap();

    let change = world.signed_change(
      ALICE,
      MembershipAction::RemoveUser {
        user_id: UserId::new(CAROL),
      },
    );
    let laptop_leaf = world.leaf_of_device(CAROL * 100);
    let commit = world.raw_alice_commit(&[], &[laptop_leaf], change.to_bytes().unwrap());
    let result =
      phone_group.process_message(&carol_phone.provider, &commit, &world.directory, &world.authority);
    assert!(matches!(result, Err(MlsError::ChangeDoesNotMatchCommit)));
  }

  #[test]
  fn member_rejects_replayed_change() {
    let mut world = World::new();
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let output = world.commit_change(&change, &[world.bob.key_package()]);
    world.carol_processes(&output.commit).unwrap();
    let remove = world.signed_change(
      ALICE,
      MembershipAction::RemoveUser {
        user_id: UserId::new(BOB),
      },
    );
    let output = world.commit_change(&remove, &[]);
    world.carol_processes(&output.commit).unwrap();

    // The server replays the old "add Bob" change in a new commit.
    let result = world.alice_group.commit_membership_change(
      &world.alice.provider,
      &world.alice.user.device,
      &change,
      &[world.bob.key_package()],
      &world.directory,
      &world.authority,
    );
    assert!(matches!(result, Err(MlsError::ReplayedChange(_))));
    let commit = world.raw_alice_commit(&[world.bob.key_package()], &[], change.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::ReplayedChange(_)));
  }

  #[test]
  fn later_joiner_refuses_an_old_add_replayed_after_a_removal() {
    // Security review A3: Alice added Bob, Bob was removed, Dave joined later. The server hands
    // Dave the old signed "add Bob"; Dave never saw it applied, but it predates his join.
    let mut world = World::new();
    let add_bob = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    world.commit_change(&add_bob, &[world.bob.key_package()]);
    let remove_bob = world.signed_change(
      ALICE,
      MembershipAction::RemoveUser {
        user_id: UserId::new(BOB),
      },
    );
    world.commit_change(&remove_bob, &[]);
    let add_dave = world.signed_change(ALICE, add(DAVE, HistoryShare::Full));
    let output = world.commit_change(&add_dave, &[world.dave.key_package()]);
    let mut dave_group = ConversationGroup::join(
      &world.dave.provider,
      output.maybe_welcome.as_ref().unwrap(),
      CONVERSATION,
      &world.directory,
    )
    .unwrap();

    let result = dave_group.commit_membership_change(
      &world.dave.provider,
      &world.dave.user.device,
      &add_bob,
      &[world.bob.key_package()],
      &world.directory,
      &world.authority,
    );
    assert!(matches!(result, Err(MlsError::StaleChange { .. })));
  }

  #[test]
  fn change_is_stale_once_its_user_changed_again() {
    // Two adds of Bob signed in the same epoch: once one is committed, the other is stale.
    let mut world = World::new();
    let first = world.signed_change(ALICE, add(BOB, HistoryShare::FromJoin));
    let second = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let output = world.commit_change(&first, &[world.bob.key_package()]);
    world.carol_processes(&output.commit).unwrap();
    let remove_bob = world.signed_change(
      ALICE,
      MembershipAction::RemoveUser {
        user_id: UserId::new(BOB),
      },
    );
    let output = world.commit_change(&remove_bob, &[]);
    world.carol_processes(&output.commit).unwrap();

    let commit = world.raw_alice_commit(&[world.bob.key_package()], &[], second.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::StaleChange { .. }));
  }

  #[test]
  fn change_signed_for_a_future_epoch_is_refused() {
    let mut world = World::new();
    let future = MembershipChange::new(
      CONVERSATION,
      world.alice_group.epoch() + 1,
      add(BOB, HistoryShare::Full),
      &mut OsRng,
    )
    .sign(&world.alice.user.device, world.alice.user.certificate.clone())
    .unwrap();
    let commit = world.raw_alice_commit(&[world.bob.key_package()], &[], future.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::StaleChange { .. }));
  }

  #[test]
  fn a_revoked_device_is_not_added_back() {
    // Security review A4: Carol's lost phone was removed; the server serves its old key package.
    let mut world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let output = world
      .carol_group
      .commit_device_additions(
        &world.carol.provider,
        &world.carol.user.device,
        &[carol_phone.key_package()],
        &world.directory,
      )
      .unwrap();
    world.carol_group.confirm_commit(&world.carol.provider).unwrap();
    world
      .alice_group
      .process_message(
        &world.alice.provider,
        &output.commit,
        &world.directory,
        &world.authority,
      )
      .unwrap();
    let revoke = world.signed_change(
      CAROL,
      MembershipAction::RemoveDevice {
        user_id: UserId::new(CAROL),
        device_id: DeviceId::new(301),
      },
    );
    let output = world
      .carol_group
      .commit_membership_change(
        &world.carol.provider,
        &world.carol.user.device,
        &revoke,
        &[],
        &world.directory,
        &world.authority,
      )
      .unwrap();
    world.carol_group.confirm_commit(&world.carol.provider).unwrap();
    world
      .alice_group
      .process_message(
        &world.alice.provider,
        &output.commit,
        &world.directory,
        &world.authority,
      )
      .unwrap();

    let result = world.alice_group.commit_device_additions(
      &world.alice.provider,
      &world.alice.user.device,
      &[carol_phone.key_package()],
      &world.directory,
    );
    assert!(matches!(result, Err(MlsError::RemovedDevice(_))));
  }

  #[test]
  fn devices_missing_from_the_device_list_are_refused() {
    let mut world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let directory = InactiveDevices {
      inner: &world.directory,
      inactive: DeviceId::new(301),
    };
    let result = world.carol_group.commit_device_additions(
      &world.carol.provider,
      &world.carol.user.device,
      &[carol_phone.key_package()],
      &directory,
    );
    assert!(matches!(result, Err(MlsError::InactiveDevice(_))));

    // A member whose directory has the newer list refuses the commit of one that doesn't.
    let commit = world
      .carol_group
      .commit_device_additions(
        &world.carol.provider,
        &world.carol.user.device,
        &[carol_phone.key_package()],
        &world.directory,
      )
      .unwrap()
      .commit;
    let error = world
      .alice_group
      .process_message(&world.alice.provider, &commit, &directory, &world.authority)
      .unwrap_err();
    assert!(matches!(error, MlsError::InactiveDevice(_)));
  }

  /// A directory whose device lists leave out one device.
  struct InactiveDevices<'a> {
    inner: &'a TestDirectory,
    inactive: DeviceId,
  }

  impl IdentityDirectory for InactiveDevices<'_> {
    fn pinned_identity(&self, user_id: UserId) -> Option<crate::identity::IdentityPublicKey> {
      self.inner.pinned_identity(user_id)
    }

    fn is_active_device(&self, device: &VerifiedDevice) -> bool {
      device.device_id() != self.inactive
    }
  }

  #[test]
  fn member_rejects_change_for_other_conversation() {
    let mut world = World::new();
    let epoch = world.alice_group.epoch();
    let change = MembershipChange::new(
      ConversationId::new(43),
      epoch,
      add(DAVE, HistoryShare::Full),
      &mut OsRng,
    )
    .sign(&world.alice.user.device, world.alice.user.certificate.clone())
    .unwrap();
    let commit = world.raw_alice_commit(&[world.dave.key_package()], &[], change.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| {
      matches!(error, MlsError::WrongConversation { .. })
    });
  }

  #[test]
  fn rejects_device_not_signed_by_pinned_identity() {
    // The server publishes a key package for "Bob" certified by an identity key it controls.
    let mut world = World::new();
    let fake_bob = Client::new(BOB);
    let fake_key_package = fake_bob.key_package();
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let result = world.alice_group.commit_membership_change(
      &world.alice.provider,
      &world.alice.user.device,
      &change,
      std::slice::from_ref(&fake_key_package),
      &world.directory,
      &world.authority,
    );
    assert!(matches!(result, Err(MlsError::InvalidCredential(_))));

    let commit = world.raw_alice_commit(&[fake_key_package], &[], change.to_bytes().unwrap());
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::InvalidCredential(_)));
  }

  #[test]
  fn unsigned_device_addition_for_new_user_is_rejected() {
    let mut world = World::new();
    let result = world.carol_group.commit_device_additions(
      &world.carol.provider,
      &world.carol.user.device,
      &[world.dave.key_package()],
      &world.directory,
    );
    assert!(matches!(result, Err(MlsError::MissingSignedChange)));
  }

  #[test]
  fn rejects_old_epoch_application_message() {
    let mut world = World::new();
    let bundle = ContentKeyBundle::full_history(&mut world.alice_ring, &mut OsRng);
    let stale = world
      .alice_group
      .send_content_keys(&world.alice.provider, &world.alice.user.device, &bundle)
      .unwrap();
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let output = world.commit_change(&change, &[world.bob.key_package()]);
    world.carol_processes(&output.commit).unwrap();

    assert!(matches!(
      world.carol_processes(&stale),
      Err(MlsError::Openmls { .. })
    ));
  }

  #[test]
  fn rejects_replayed_application_message() {
    let mut world = World::new();
    let bundle = ContentKeyBundle::full_history(&mut world.alice_ring, &mut OsRng);
    let key_message = world
      .alice_group
      .send_content_keys(&world.alice.provider, &world.alice.user.device, &bundle)
      .unwrap();
    assert!(world.carol_processes(&key_message).is_ok());
    assert!(matches!(
      world.carol_processes(&key_message),
      Err(MlsError::Openmls { .. })
    ));
  }

  #[test]
  fn join_rejects_members_without_pinned_identity() {
    let mut world = World::new();
    let welcome = world.welcome_for_bob();
    let without_alice = TestDirectory::with(&[&world.bob.user, &world.carol.user]);
    assert!(matches!(
      ConversationGroup::join(&world.bob.provider, &welcome, CONVERSATION, &without_alice),
      Err(MlsError::UnknownUser(_))
    ));
  }

  #[test]
  fn join_rejects_welcome_for_other_conversation() {
    let mut world = World::new();
    let welcome = world.welcome_for_bob();
    let result = ConversationGroup::join(
      &world.bob.provider,
      &welcome,
      ConversationId::new(43),
      &world.directory,
    );
    assert!(matches!(result, Err(MlsError::UnexpectedMessage(_))));
  }

  #[test]
  fn rejects_malformed_messages() {
    let mut world = World::new();
    for garbage in [&[][..], &[0, 1, 2], &[0xff; 64]] {
      assert!(world.carol_processes(garbage).is_err());
      assert!(ConversationGroup::join(&world.bob.provider, garbage, CONVERSATION, &world.directory).is_err());
    }
  }
}

/// Alice (admin) and Carol share a group; Bob and Dave are pinned but not members.
struct World {
  alice: Client,
  bob: Client,
  carol: Client,
  dave: Client,
  directory: TestDirectory,
  authority: TestAuthority,
  alice_group: ConversationGroup,
  carol_group: ConversationGroup,
  alice_ring: ContentKeyRing,
}

struct Client {
  user: TestUser,
  provider: DefaultMlsProvider,
}

impl World {
  fn new() -> Self {
    let alice = Client::new(ALICE);
    let bob = Client::new(BOB);
    let carol = Client::new(CAROL);
    let dave = Client::new(DAVE);
    let directory = TestDirectory::with(&[&alice.user, &bob.user, &carol.user, &dave.user]);
    let authority = TestAuthority::allowing(&[UserId::new(ALICE)]);
    let mut alice_group = ConversationGroup::create(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
    )
    .unwrap();
    let add_carol = MembershipChange::new(CONVERSATION, 0, add(CAROL, HistoryShare::Full), &mut OsRng)
      .sign(&alice.user.device, alice.user.certificate.clone())
      .unwrap();
    let output = alice_group
      .commit_membership_change(
        &alice.provider,
        &alice.user.device,
        &add_carol,
        &[carol.key_package()],
        &directory,
        &authority,
      )
      .unwrap();
    alice_group.confirm_commit(&alice.provider).unwrap();
    let carol_group = ConversationGroup::join(
      &carol.provider,
      &output.maybe_welcome.unwrap(),
      CONVERSATION,
      &directory,
    )
    .unwrap();
    let mut alice_ring = ContentKeyRing::new(CONVERSATION);
    alice_ring.rotate(&mut OsRng);
    Self {
      alice,
      bob,
      carol,
      dave,
      directory,
      authority,
      alice_group,
      carol_group,
      alice_ring,
    }
  }

  fn signed_change(&self, signer: u64, action: MembershipAction) -> SignedMembershipChange {
    let user = [&self.alice, &self.bob, &self.carol, &self.dave]
      .into_iter()
      .find(|client| client.user.user_id == UserId::new(signer))
      .unwrap();
    MembershipChange::new(CONVERSATION, self.alice_group.epoch(), action, &mut OsRng)
      .sign(&user.user.device, user.user.certificate.clone())
      .unwrap()
  }

  /// Alice commits a change and merges it.
  fn commit_change(&mut self, change: &SignedMembershipChange, key_packages: &[Vec<u8>]) -> CommitOutput {
    let output = self
      .alice_group
      .commit_membership_change(
        &self.alice.provider,
        &self.alice.user.device,
        change,
        key_packages,
        &self.directory,
        &self.authority,
      )
      .unwrap();
    self.alice_group.confirm_commit(&self.alice.provider).unwrap();
    output
  }

  /// Alice builds a commit with arbitrary proposals and authenticated data, bypassing the checks
  /// of `commit_membership_change` – as a malicious or buggy member client could.
  fn raw_alice_commit(
    &mut self,
    key_packages: &[Vec<u8>],
    removals: &[LeafNodeIndex],
    aad: Vec<u8>,
  ) -> Vec<u8> {
    let key_packages = key_packages
      .iter()
      .map(|bytes| {
        KeyPackageIn::tls_deserialize_exact(bytes.as_slice())
          .unwrap()
          .validate(self.alice.provider.crypto(), ProtocolVersion::Mls10)
          .unwrap()
      })
      .collect();
    let removals = removals
      .iter()
      .map(|index| (*index, member(&self.alice.user)))
      .collect();
    self.alice_group.group.set_aad(aad);
    let output = self.alice_group.stage_commit(
      &self.alice.provider,
      &self.alice.user.device,
      key_packages,
      removals,
    );
    self.alice_group.discard_commit(&self.alice.provider).unwrap();
    output.unwrap().commit
  }

  fn carol_processes(&mut self, message: &[u8]) -> Result<GroupEvent, MlsError> {
    self
      .carol_group
      .process_message(&self.carol.provider, message, &self.directory, &self.authority)
  }

  fn expect_carol_rejects(&mut self, commit: &[u8], is_expected: impl Fn(&MlsError) -> bool) {
    let epoch = self.carol_group.epoch();
    let members = self.carol_group.members().unwrap();
    let error = self.carol_processes(commit).expect_err("commit must be rejected");
    assert!(is_expected(&error), "unexpected error: {error}");
    assert_eq!(self.carol_group.epoch(), epoch);
    assert_eq!(self.carol_group.members().unwrap(), members);
  }

  fn welcome_for_bob(&mut self) -> Vec<u8> {
    let change = self.signed_change(ALICE, add(BOB, HistoryShare::Full));
    self
      .commit_change(&change, &[self.bob.key_package()])
      .maybe_welcome
      .unwrap()
  }

  fn bob_joins(&self, output: &CommitOutput) -> ConversationGroup {
    let welcome = output.maybe_welcome.as_ref().unwrap();
    ConversationGroup::join(&self.bob.provider, welcome, CONVERSATION, &self.directory).unwrap()
  }

  fn leaf_of(&self, user_id: u64) -> LeafNodeIndex {
    self.leaf_of_device(user_id * 100)
  }

  fn leaf_of_device(&self, device_id: u64) -> LeafNodeIndex {
    self
      .alice_group
      .group
      .members()
      .find(|member| credential::member_of(&member.credential).unwrap().device_id.get() == device_id)
      .unwrap()
      .index
  }

  /// Alice derives the current epoch's content key and makes it current, as after a removal.
  fn alice_rotates_to_epoch_key(&mut self) -> ContentKey {
    let key = self.alice_group.epoch_content_key(&self.alice.provider).unwrap();
    let id = key.id();
    self.alice_ring.insert(key.duplicate()).unwrap();
    self.alice_ring.set_current(id).unwrap();
    key
  }

  fn alice_key(&self) -> crate::identity::DeviceSigningPublicKey {
    self.alice.user.device.public_keys().signing_key
  }

  fn alice_context(&self) -> MessageContext {
    MessageContext {
      conversation_id: CONVERSATION,
      sender_user_id: self.alice.user.user_id,
      sender_device_id: self.alice.user.device.device_id(),
      client_message_id: ClientMessageId::from_bytes([1; 16]),
    }
  }

  fn alice_message(&self, plaintext: &[u8]) -> crate::message::EncryptedMessage {
    encrypt_message(
      &self.alice_ring,
      &self.alice_context(),
      plaintext,
      &self.alice.user.device,
      &mut OsRng,
    )
    .unwrap()
  }
}

impl Client {
  fn new(user_id: u64) -> Self {
    Self::from_user(TestUser::new(user_id))
  }

  fn from_user(user: TestUser) -> Self {
    Self {
      user,
      provider: DefaultMlsProvider::default(),
    }
  }

  fn key_package(&self) -> Vec<u8> {
    create_key_package(&self.provider, &self.user.device, &self.user.certificate).unwrap()
  }
}

fn apply_keys(
  group: &mut ConversationGroup,
  client: &Client,
  message: &[u8],
  directory: &TestDirectory,
  authority: &TestAuthority,
  ring: &mut ContentKeyRing,
) {
  let GroupEvent::ContentKeys { bundle, .. } = group
    .process_message(&client.provider, message, directory, authority)
    .unwrap()
  else {
    panic!("expected content keys")
  };
  bundle.apply_to(ring).unwrap();
}

fn add(user_id: u64, history: HistoryShare) -> MembershipAction {
  MembershipAction::AddUser {
    user_id: UserId::new(user_id),
    history,
  }
}

fn member(user: &TestUser) -> GroupMember {
  GroupMember {
    user_id: user.user_id,
    device_id: user.device.device_id(),
  }
}

#[test]
fn group_survives_reload_from_storage() {
  let mut world = World::new();
  let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  world.commit_change(&change, &[world.bob.key_package()]);
  let applied: Vec<ChangeId> = world.alice_group.applied_changes().copied().collect();
  assert!(!world.alice_group.has_pending_commit());

  let history = ChangeHistory::from_bytes(&world.alice_group.change_history().to_bytes().unwrap()).unwrap();
  let reloaded = ConversationGroup::load(&world.alice.provider, CONVERSATION, applied, history)
    .unwrap()
    .unwrap();
  assert_eq!(reloaded.epoch(), world.alice_group.epoch());
  assert_eq!(reloaded.members().unwrap(), world.alice_group.members().unwrap());
  assert!(
    reloaded
      .applied_changes()
      .any(|id| *id == change.unverified().change_id())
  );
  assert!(
    ConversationGroup::load(
      &world.alice.provider,
      ConversationId::new(99),
      [],
      ChangeHistory::default()
    )
    .unwrap()
    .is_none()
  );
}

#[test]
fn last_resort_key_package_can_be_used_twice() {
  let mut world = World::new();
  let last_resort = crate::mls::create_last_resort_key_package(
    &world.bob.provider,
    &world.bob.user.device,
    &world.bob.user.certificate,
  )
  .unwrap();
  let add_bob = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  let output = world.commit_change(&add_bob, std::slice::from_ref(&last_resort));
  world.bob_joins(&output);

  // A second conversation reuses the same last-resort package.
  let mut other = ConversationGroup::create(
    &world.alice.provider,
    &world.alice.user.device,
    &world.alice.user.certificate,
    ConversationId::new(77),
  )
  .unwrap();
  let change = MembershipChange::new(
    ConversationId::new(77),
    0,
    add(BOB, HistoryShare::Full),
    &mut OsRng,
  )
  .sign(&world.alice.user.device, world.alice.user.certificate.clone())
  .unwrap();
  let output = other
    .commit_membership_change(
      &world.alice.provider,
      &world.alice.user.device,
      &change,
      &[last_resort],
      &world.directory,
      &world.authority,
    )
    .unwrap();
  other.confirm_commit(&world.alice.provider).unwrap();
  let welcome = output.maybe_welcome.unwrap();
  ConversationGroup::join(
    &world.bob.provider,
    &welcome,
    ConversationId::new(77),
    &world.directory,
  )
  .unwrap();
}
