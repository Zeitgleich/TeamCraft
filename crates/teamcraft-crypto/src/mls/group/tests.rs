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
  assert_eq!(
    update.maybe_signer,
    Some(UserId::new(ALICE)),
    "the verified signer"
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

/// Issue #424: a device removed from a group deletes all of the group's storage entries, the leaf
/// keys of the epoch before its removal included.
#[test]
fn a_removed_member_deletes_every_entry_of_the_group() {
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
  let event = bob_group
    .process_message(
      &world.bob.provider,
      &output.commit,
      &world.directory,
      &world.authority,
    )
    .unwrap();
  assert!(matches!(event, GroupEvent::MembershipChanged(update) if update.is_self_removed));
  let group_entries = |provider: &DefaultMlsProvider| {
    // openmls_memory_storage keys a group's entries with its ID as a JSON array of bytes.
    let id = group_id(CONVERSATION);
    let marker: Vec<String> = id.as_slice().iter().map(u8::to_string).collect();
    let marker = format!("[{}]", marker.join(","));
    provider
      .storage()
      .values
      .read()
      .unwrap()
      .keys()
      .filter(|key| {
        key
          .windows(marker.len())
          .any(|window| window == marker.as_bytes())
      })
      .count()
  };
  assert!(group_entries(&world.bob.provider) > 0);

  bob_group.delete(&world.bob.provider).unwrap();

  assert_eq!(group_entries(&world.bob.provider), 0);
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
  assert!(update.maybe_signer.is_none());
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

  /// A directory that lists one user as a meeting guest.
  struct GuestDirectory<'a> {
    inner: &'a TestDirectory,
    guest: UserId,
  }

  impl IdentityDirectory for GuestDirectory<'_> {
    fn pinned_identity(&self, user_id: UserId) -> Option<crate::identity::IdentityPublicKey> {
      self.inner.pinned_identity(user_id)
    }

    fn is_guest(&self, user_id: UserId) -> bool {
      user_id == self.guest
    }
  }

  /// An authority that lets every member change the group, as the client does for members.
  struct EveryMember;

  impl MembershipAuthority for EveryMember {
    fn is_authorized(&self, _: UserId, _: ConversationId, _: &MembershipAction) -> bool {
      true
    }
  }

  #[test]
  fn a_guests_commit_cannot_add_anybody_else() {
    let mut world = World::new();
    let change = world.signed_change(CAROL, add(DAVE, HistoryShare::FromJoin));
    let commit = world
      .carol_group
      .commit_membership_change(
        &world.carol.provider,
        &world.carol.user.device,
        &change,
        &[world.dave.key_package()],
        &world.directory,
        &EveryMember,
      )
      .unwrap()
      .commit;

    // The same commit from a member is fine; from the guest it is refused.
    let guest_view = GuestDirectory {
      inner: &world.directory,
      guest: UserId::new(CAROL),
    };
    let error = world
      .alice_group
      .process_message(&world.alice.provider, &commit, &guest_view, &EveryMember)
      .unwrap_err();
    assert!(matches!(error, MlsError::GuestOverreach), "{error}");
    assert!(
      !world
        .alice_group
        .members()
        .unwrap()
        .iter()
        .any(|member| member.user_id == UserId::new(DAVE))
    );
  }

  #[test]
  fn a_guest_may_commit_somebodys_signed_leave_and_nothing_else_signed() {
    // Somebody else's signed removal of Alice, committed by the guest: refused.
    let mut world = World::new();
    let removal = world.signed_change(
      BOB,
      MembershipAction::RemoveUser {
        user_id: UserId::new(ALICE),
      },
    );
    let commit = world
      .carol_group
      .commit_membership_change(
        &world.carol.provider,
        &world.carol.user.device,
        &removal,
        &[],
        &world.directory,
        &EveryMember,
      )
      .unwrap()
      .commit;
    let view = GuestDirectory {
      inner: &world.directory,
      guest: UserId::new(CAROL),
    };
    let error = world
      .alice_group
      .process_message(&world.alice.provider, &commit, &view, &EveryMember)
      .unwrap_err();
    assert!(matches!(error, MlsError::GuestOverreach), "{error}");

    // Alice's own signed leave, committed by the guest: taken.
    let mut world = World::new();
    let leave = world.signed_change(
      ALICE,
      MembershipAction::RemoveUser {
        user_id: UserId::new(ALICE),
      },
    );
    let commit = world
      .carol_group
      .commit_membership_change(
        &world.carol.provider,
        &world.carol.user.device,
        &leave,
        &[],
        &world.directory,
        &EveryMember,
      )
      .unwrap()
      .commit;
    let view = GuestDirectory {
      inner: &world.directory,
      guest: UserId::new(CAROL),
    };
    world
      .alice_group
      .process_message(&world.alice.provider, &commit, &view, &EveryMember)
      .unwrap();
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
  fn a_staged_join_names_everybody_in_the_tree_before_trusting_them() {
    let mut world = World::new();
    let welcome = world.welcome_for_bob();
    let staged = ConversationGroup::stage_join(&world.bob.provider, &welcome, CONVERSATION).unwrap();
    let mut claimed = staged.claimed_users().unwrap();
    claimed.sort();
    let mut expected: Vec<UserId> = world
      .alice_group
      .members()
      .unwrap()
      .into_iter()
      .map(|member| member.user_id)
      .collect();
    expected.sort();
    expected.dedup();
    assert!(claimed.contains(&UserId::new(ALICE)) && claimed.contains(&UserId::new(BOB)));
    assert_eq!(claimed, expected);
    // Completing still verifies every leaf: a directory without one of them is refused.
    let without_alice = TestDirectory::with(&[&world.bob.user, &world.carol.user]);
    assert!(matches!(
      staged.complete(&world.bob.provider, &without_alice),
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

/// A member catching up reads whom a commit's signed change names before it processes the commit
/// (issue #972).
mod named_in_change {
  use super::*;
  use crate::mls::users_named_in_change;

  #[test]
  fn a_signed_commit_names_its_target_and_signer() {
    let mut world = World::new();
    let change = world.signed_change(ALICE, add(DAVE, HistoryShare::Full));
    let dave = world.dave.key_package();
    let output = world.commit_change(&change, &[dave]);
    assert_eq!(
      users_named_in_change(&output.commit),
      [UserId::new(DAVE), UserId::new(ALICE)]
    );
  }

  #[test]
  fn anything_else_names_nobody() {
    let mut world = World::new();
    let dave = world.dave.key_package();
    let unsigned = world.raw_alice_commit(&[dave], &[], Vec::new());
    assert!(users_named_in_change(&unsigned).is_empty());
    for garbage in [&[][..], &[0, 1, 2], &[0xff; 64]] {
      assert!(users_named_in_change(garbage).is_empty());
    }
  }
}

mod key_package_lifetime {
  use super::*;
  use crate::mls::{KEY_PACKAGE_LIFETIME_SECS, inspect_key_package};

  const DAY_SECS: u64 = 24 * 60 * 60;

  #[test]
  fn lifetime_follows_the_creators_clock() {
    let world = World::new();
    let now = now_secs();
    let info = inspect_key_package(&world.bob.key_package_at(now)).unwrap();
    assert_eq!(info.not_after_unix_secs, now + KEY_PACKAGE_LIFETIME_SECS);
    assert!(!info.is_last_resort);
    let last_resort = crate::mls::create_last_resort_key_package(
      &world.bob.provider,
      &world.bob.user.device,
      &world.bob.user.certificate,
      now,
    )
    .unwrap();
    assert!(inspect_key_package(&last_resort).unwrap().is_last_resort);
  }

  #[test]
  fn an_expired_package_is_refused_by_server_and_committer() {
    let mut world = World::new();
    let expired = world.bob.key_package_at(now_secs() - 90 * DAY_SECS);
    assert!(inspect_key_package(&expired).is_err());
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let result = world.alice_group.commit_membership_change(
      &world.alice.provider,
      &world.alice.user.device,
      &change,
      &[expired],
      &world.directory,
      &world.authority,
    );
    assert!(result.is_err());
    assert!(!world.alice_group.has_pending_commit());
  }

  #[test]
  fn a_creator_clock_hours_ahead_is_tolerated() {
    let mut world = World::new();
    let ahead = world.bob.key_package_at(now_secs() + 6 * 60 * 60);
    assert!(inspect_key_package(&ahead).is_ok());
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let output = world.commit_change(&change, &[ahead]);
    world.carol_processes(&output.commit).unwrap();
    world.bob_joins(&output);
  }

  #[test]
  fn garbage_is_not_a_key_package() {
    for garbage in [&[][..], &[0, 1, 2], &[0xff; 64]] {
      assert!(inspect_key_package(garbage).is_err());
    }
  }
}

/// Content keys for a device added back, sealed to it (e2ee.md §4.6, #147).
mod sealed_keys {
  use super::*;
  use crate::mls::content_keys::{ApplicationPayload, SealedContentKeys};

  #[test]
  fn only_the_device_they_are_sealed_to_opens_them() {
    let mut world = World::new();
    let output = world.commit_change(
      &world.signed_change(ALICE, add(BOB, HistoryShare::Full)),
      &[world.bob.key_package()],
    );
    world.carol_processes(&output.commit).unwrap();
    let mut bob_group = world.bob_joins(&output);
    let missed = missed_keys_of(&world.alice_ring);
    let message = world
      .alice_group
      .send_sealed_content_keys(
        &world.alice.provider,
        &world.alice.user.device,
        &verified(&world, &world.carol.user),
        &missed,
      )
      .unwrap();

    let GroupEvent::SealedContentKeys { sender, sealed } = world.carol_processes(&message).unwrap() else {
      panic!("expected sealed content keys")
    };
    assert_eq!(sender, member(&world.alice.user));
    assert_eq!(sealed.recipient(), member(&world.carol.user));
    let opened = sealed.open(&world.carol.user.device).unwrap();
    assert_eq!(key_ids(opened.keys()), key_ids(world.alice_ring.keys()));
    assert_eq!(opened.current(), None, "never a current key");

    // Bob is a member and sees the message, but it opens only on Carol's device.
    let GroupEvent::SealedContentKeys { sealed, .. } = bob_group
      .process_message(&world.bob.provider, &message, &world.directory, &world.authority)
      .unwrap()
    else {
      panic!("expected sealed content keys")
    };
    assert!(matches!(
      sealed.open(&world.bob.user.device),
      Err(MlsError::SealedKeysRejected)
    ));
  }

  #[test]
  fn tampered_or_moved_sealed_keys_are_rejected() {
    let world = World::new();
    let carol = verified(&world, &world.carol.user);
    let alice = member(&world.alice.user);
    let epoch = world.alice_group.epoch();
    let sealed = missed_keys_of(&world.alice_ring)
      .seal_for(&carol, alice, epoch)
      .unwrap();
    let open = |bytes: &[u8], sender: GroupMember, epoch: u64| {
      let ApplicationPayload::Sealed(wire) = ApplicationPayload::from_bytes(bytes)? else {
        panic!("expected a sealed payload")
      };
      SealedContentKeys::received(wire, sender, epoch).open(&world.carol.user.device)
    };
    assert!(open(&sealed, alice, epoch).is_ok());

    let mut tampered = sealed.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    let bob = member(&world.bob.user);
    for (bytes, sender, at_epoch) in [
      (tampered.as_slice(), alice, epoch),
      (sealed.as_slice(), bob, epoch),
      (sealed.as_slice(), alice, epoch + 1),
      (sealed.as_slice(), alice, epoch.saturating_sub(1)),
    ] {
      assert!(matches!(
        open(bytes, sender, at_epoch),
        Err(MlsError::SealedKeysRejected)
      ));
    }
    // Moved to another conversation: the version byte, the conversation ID (8 bytes), then the
    // recipient.
    let mut moved = sealed.clone();
    moved[8] ^= 1;
    assert!(matches!(
      open(&moved, alice, epoch),
      Err(MlsError::SealedKeysRejected)
    ));
    assert!(
      open(&sealed[..sealed.len() - 1], alice, epoch).is_err(),
      "truncated"
    );
  }

  #[test]
  fn sealed_keys_name_no_current_key_and_go_only_to_members() {
    let mut world = World::new();
    let carol = verified(&world, &world.carol.user);
    let with_current = ContentKeyBundle::full_history(&mut world.alice_ring, &mut OsRng);
    assert!(matches!(
      world.alice_group.send_sealed_content_keys(
        &world.alice.provider,
        &world.alice.user.device,
        &carol,
        &with_current,
      ),
      Err(MlsError::UnexpectedMessage(_))
    ));
    let dave = verified(&world, &world.dave.user);
    assert!(matches!(
      world.alice_group.send_sealed_content_keys(
        &world.alice.provider,
        &world.alice.user.device,
        &dave,
        &missed_keys_of(&world.alice_ring),
      ),
      Err(MlsError::UnexpectedMessage(_))
    ));
  }

  fn missed_keys_of(ring: &ContentKeyRing) -> ContentKeyBundle {
    ContentKeyBundle::without_current(
      CONVERSATION,
      ring.keys().iter().map(ContentKey::duplicate).collect(),
    )
  }

  fn verified(world: &World, user: &TestUser) -> VerifiedDevice {
    user
      .certificate
      .verify(&world.directory.pinned_identity(user.user_id).unwrap())
      .unwrap()
  }

  fn key_ids(keys: &[ContentKey]) -> Vec<crate::content_key::ContentKeyId> {
    keys.iter().map(ContentKey::id).collect()
  }
}

#[test]
fn one_member_is_verified_alone_and_a_welcome_names_its_maker() {
  let mut world = World::new();
  let carol = world
    .alice_group
    .verified_member(member(&world.carol.user), &world.directory)
    .unwrap()
    .unwrap();
  assert_eq!(GroupMember::from(&carol), member(&world.carol.user));
  assert!(
    world
      .alice_group
      .verified_member(member(&world.dave.user), &world.directory)
      .unwrap()
      .is_none(),
    "not in the group"
  );
  // Carol's identity isn't pinned: only her leaf fails, Alice's still verifies.
  let without_carol = TestDirectory::with(&[&world.alice.user]);
  assert!(
    world
      .alice_group
      .verified_member(member(&world.carol.user), &without_carol)
      .is_err()
  );
  assert!(
    world
      .alice_group
      .verified_member(member(&world.alice.user), &without_carol)
      .unwrap()
      .is_some()
  );

  let welcome = world.welcome_for_bob();
  let staged = ConversationGroup::stage_join(&world.bob.provider, &welcome, CONVERSATION).unwrap();
  assert_eq!(staged.sender().unwrap(), member(&world.alice.user));
}

mod refounding {
  use super::*;

  #[test]
  fn a_refounded_group_commits_at_the_queues_epoch_and_takes_further_devices() {
    let alice = Client::new(ALICE);
    let directory = TestDirectory::with(&[&alice.user]);
    let (mut group, output) = ConversationGroup::refound(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
      5,
    )
    .unwrap();
    assert_eq!(group.epoch(), 5, "the commit addresses the queue's epoch");
    assert!(output.maybe_group_info.is_some());
    assert!(group.pending_commit().unwrap().is_commit(&output.commit));
    group.confirm_commit(&alice.provider).unwrap();
    assert_eq!(group.epoch(), 6);
    assert_eq!(group.members().unwrap(), vec![member(&alice.user)]);

    let laptop = Client::from_user(alice.user.new_device(101));
    let added = group
      .commit_device_additions(
        &alice.provider,
        &alice.user.device,
        &[laptop.key_package()],
        &directory,
      )
      .unwrap();
    group.confirm_commit(&alice.provider).unwrap();
    let laptop_group = ConversationGroup::join(
      &laptop.provider,
      &added.maybe_welcome.unwrap(),
      CONVERSATION,
      &directory,
    )
    .unwrap();
    assert_eq!(laptop_group.epoch(), 7);
    assert_eq!(laptop_group.members().unwrap().len(), 2);
  }

  #[test]
  fn a_device_of_the_old_group_cannot_read_the_new_one() {
    let mut world = World::new();
    let epoch = world.alice_group.epoch();
    let alice_laptop = Client::from_user(world.alice.user.new_device(102));
    let (_, output) = ConversationGroup::refound(
      &alice_laptop.provider,
      &alice_laptop.user.device,
      &alice_laptop.user.certificate,
      CONVERSATION,
      epoch,
    )
    .unwrap();
    let event = world.carol_group.process_message(
      &world.carol.provider,
      &output.commit,
      &world.directory,
      &world.authority,
    );
    assert!(event.is_err(), "the old group refuses the new group's commit");
    assert_eq!(world.carol_group.epoch(), epoch);
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
      false,
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
    self.key_package_at(now_secs())
  }

  /// A key package as made by a device whose clock shows `now_unix_secs`.
  fn key_package_at(&self, now_unix_secs: u64) -> Vec<u8> {
    create_key_package(
      &self.provider,
      &self.user.device,
      &self.user.certificate,
      now_unix_secs,
    )
    .unwrap()
  }
}

fn now_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs()
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
  let reloaded = ConversationGroup::load(&world.alice.provider, CONVERSATION, applied, history, None)
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
      ChangeHistory::default(),
      None
    )
    .unwrap()
    .is_none()
  );
}

#[test]
fn a_pending_commit_survives_a_reload_and_is_recognised_in_the_queue() {
  let mut world = World::new();
  let epoch = world.alice_group.epoch();
  let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  let output = world
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
  let pending =
    PendingCommit::from_bytes(&world.alice_group.pending_commit().unwrap().to_bytes().unwrap()).unwrap();
  let applied: Vec<ChangeId> = world.alice_group.applied_changes().copied().collect();
  let history = world.alice_group.change_history().clone();

  // The app restarts before the server's answer arrives; the commit shows up in the queue.
  let mut reloaded = ConversationGroup::load(
    &world.alice.provider,
    CONVERSATION,
    applied,
    history,
    Some(pending),
  )
  .unwrap()
  .unwrap();
  let pending = reloaded.pending_commit().unwrap();
  assert!(pending.is_commit(&output.commit));
  assert!(!pending.is_commit(b"another commit"));
  let update = reloaded.confirm_commit(&world.alice.provider).unwrap();
  assert_eq!(reloaded.epoch(), epoch + 1);
  assert_eq!(
    update.maybe_change.map(|change| change.change_id()),
    Some(change.unverified().change_id())
  );
  assert!(reloaded.pending_commit().is_none());
  // The other members merge the same commit and stay in step.
  world.carol_processes(&output.commit).unwrap();
  assert_eq!(world.carol_group.epoch(), reloaded.epoch());
}

mod external_join {
  use openmls::prelude::{MlsGroup, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn};

  use super::*;

  #[test]
  fn a_further_device_of_a_member_joins_on_its_own() {
    let mut world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    let epoch = world.alice_group.epoch();
    let (phone_group, output) = ConversationGroup::join_external(
      &carol_phone.provider,
      &carol_phone.user.device,
      &carol_phone.user.certificate,
      &group_info,
      CONVERSATION,
      0,
      &world.directory,
    )
    .unwrap();
    // openmls merges an external commit right away.
    assert_eq!(phone_group.epoch(), epoch + 1);
    assert!(!phone_group.has_pending_commit());

    for (group, provider) in [
      (&mut world.alice_group, &world.alice.provider),
      (&mut world.carol_group, &world.carol.provider),
    ] {
      let Ok(GroupEvent::MembershipChanged(update)) =
        group.process_message(provider, &output.commit, &world.directory, &world.authority)
      else {
        panic!("expected a membership change")
      };
      assert!(update.maybe_change.is_none());
      assert_eq!(update.added, vec![member(&carol_phone.user)]);
      assert_eq!(group.epoch(), phone_group.epoch());
    }
    assert_eq!(phone_group.members().unwrap().len(), 3);
    assert_eq!(
      phone_group.epoch_content_key(&carol_phone.provider).unwrap().id(),
      world
        .alice_group
        .epoch_content_key(&world.alice.provider)
        .unwrap()
        .id()
    );
    // The commit carried the next epoch's group information.
    assert!(output.maybe_group_info.is_some());
  }

  #[test]
  fn a_user_who_is_not_a_member_cannot_join_on_their_own() {
    let mut world = World::new();
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    let result = ConversationGroup::join_external(
      &world.bob.provider,
      &world.bob.user.device,
      &world.bob.user.certificate,
      &group_info,
      CONVERSATION,
      0,
      &world.directory,
    );
    assert!(matches!(result, Err(MlsError::MissingSignedChange)));

    // A client that skips its own check is refused by the members.
    let commit = raw_external_commit(&world.bob, &group_info);
    world.expect_carol_rejects(&commit, |error| matches!(error, MlsError::MissingSignedChange));
  }

  #[test]
  fn a_device_already_in_the_group_or_from_another_conversation_is_refused() {
    let world = World::new();
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    let other = DefaultMlsProvider::default();
    let again = ConversationGroup::join_external(
      &other,
      &world.carol.user.device,
      &world.carol.user.certificate,
      &group_info,
      CONVERSATION,
      0,
      &world.directory,
    );
    assert!(again.is_err());
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let wrong_conversation = ConversationGroup::join_external(
      &carol_phone.provider,
      &carol_phone.user.device,
      &carol_phone.user.certificate,
      &group_info,
      ConversationId::new(43),
      0,
      &world.directory,
    );
    assert!(matches!(wrong_conversation, Err(MlsError::UnexpectedMessage(_))));
  }

  #[test]
  fn group_information_older_than_the_users_floor_is_refused() {
    let world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    // The signed epoch counts, whatever the server claims around it.
    let result = ConversationGroup::join_external(
      &carol_phone.provider,
      &carol_phone.user.device,
      &carol_phone.user.certificate,
      &group_info,
      CONVERSATION,
      world.alice_group.epoch() + 1,
      &world.directory,
    );
    assert!(matches!(result, Err(MlsError::StaleGroupInfo { .. })));
    // Nothing of the group stays behind on the device.
    assert!(
      ConversationGroup::load(
        &carol_phone.provider,
        CONVERSATION,
        [],
        ChangeHistory::default(),
        None
      )
      .unwrap()
      .is_none()
    );
  }

  #[test]
  fn members_refuse_a_device_no_signed_device_list_names() {
    let mut world = World::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    let commit = raw_external_commit(&carol_phone, &group_info);
    let unlisted = Unlisted(&world.directory);
    let epoch = world.carol_group.epoch();
    let result =
      world
        .carol_group
        .process_message(&world.carol.provider, &commit, &unlisted, &world.authority);
    assert!(matches!(result, Err(MlsError::InactiveDevice(_))));
    assert_eq!(world.carol_group.epoch(), epoch);
  }

  /// A directory that knows the identities but no device list (the server 404s them).
  struct Unlisted<'a>(&'a TestDirectory);

  impl IdentityDirectory for Unlisted<'_> {
    fn pinned_identity(&self, user_id: UserId) -> Option<crate::identity::IdentityPublicKey> {
      self.0.pinned_identity(user_id)
    }
  }

  /// An external commit built with openmls directly, without TeamCraft's checks.
  fn raw_external_commit(client: &Client, group_info: &[u8]) -> Vec<u8> {
    let MlsMessageBodyIn::GroupInfo(info) =
      MlsMessageIn::tls_deserialize_exact(group_info).unwrap().extract()
    else {
      panic!("expected a group info")
    };
    let config = MlsGroupJoinConfig::builder()
      .use_ratchet_tree_extension(true)
      .build();
    let (_, bundle) = MlsGroup::external_commit_builder()
      .with_config(config)
      .build_group(
        &client.provider,
        info,
        credential::credential_with_key(&client.user.certificate).unwrap(),
      )
      .unwrap()
      .load_psks(client.provider.storage())
      .unwrap()
      .build(
        client.provider.rand(),
        client.provider.crypto(),
        &client.user.device,
        |_| true,
      )
      .unwrap()
      .finalize(&client.provider)
      .unwrap();
    bundle.into_commit().to_bytes().unwrap()
  }
}

mod rejoin {
  use super::*;
  use crate::mls::{RejoinRequest, SignedRejoinRequest};

  #[test]
  fn a_member_adds_back_a_device_that_asked() {
    let mut world = World::new();
    // Bob is a member too, and checks Alice's rejoin commit.
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    let output = world.commit_change(&change, &[world.bob.key_package()]);
    world.carol_processes(&output.commit).unwrap();
    let mut bob_group = world.bob_joins(&output);

    // Carol's device lost its group state and starts from a fresh one.
    let fresh = DefaultMlsProvider::default();
    let request = world.carol_request(world.alice_group.epoch());
    let carol = member(&world.carol.user);
    assert_eq!(
      world
        .alice_group
        .check_rejoin_request(&request, &world.directory)
        .unwrap(),
      carol
    );
    let output = world
      .alice_group
      .commit_rejoin(
        &world.alice.provider,
        &world.alice.user.device,
        &request,
        &world.carol_key_package_in(&fresh),
        &world.directory,
      )
      .unwrap();
    let update = world.alice_group.confirm_commit(&world.alice.provider).unwrap();
    assert_eq!(update.maybe_rejoined, Some(carol));
    assert_eq!(
      (update.added.as_slice(), update.removed.as_slice()),
      (&[carol][..], &[carol][..])
    );

    let GroupEvent::MembershipChanged(seen) = bob_group
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
    assert_eq!(seen.maybe_rejoined, Some(carol));
    let carol_group = ConversationGroup::join(
      &fresh,
      &output.maybe_welcome.unwrap(),
      CONVERSATION,
      &world.directory,
    )
    .unwrap();
    assert_eq!(carol_group.epoch(), world.alice_group.epoch());
    assert_eq!(bob_group.epoch(), world.alice_group.epoch());
    assert_eq!(carol_group.members().unwrap().len(), 3);
    assert_eq!(
      carol_group.epoch_content_key(&fresh).unwrap().id(),
      bob_group.epoch_content_key(&world.bob.provider).unwrap().id()
    );

    // A request is answered once.
    for group in [&world.alice_group, &bob_group] {
      assert!(matches!(
        group.check_rejoin_request(&request, &world.directory),
        Err(MlsError::ReplayedChange(_))
      ));
    }
  }

  #[test]
  fn only_the_device_itself_asks() {
    let world = World::new();
    let result = RejoinRequest::new(
      CONVERSATION,
      world.alice_group.epoch(),
      member(&world.carol.user),
      &mut OsRng,
    )
    .sign(&world.alice.user.device, world.alice.user.certificate.clone());
    assert!(matches!(
      result,
      Err(MlsError::Membership(MembershipError::CertificateMismatch))
    ));
  }

  #[test]
  fn requests_for_other_epochs_groups_or_non_members_are_refused() {
    let world = World::new();
    let epoch = world.alice_group.epoch();
    let future = world.carol_request(epoch + 1);
    assert!(matches!(
      world.alice_group.check_rejoin_request(&future, &world.directory),
      Err(MlsError::StaleChange { .. })
    ));
    let other_group = RejoinRequest::new(
      ConversationId::new(43),
      epoch,
      member(&world.carol.user),
      &mut OsRng,
    )
    .sign(&world.carol.user.device, world.carol.user.certificate.clone())
    .unwrap();
    assert!(matches!(
      world
        .alice_group
        .check_rejoin_request(&other_group, &world.directory),
      Err(MlsError::WrongConversation { .. })
    ));
    let not_a_member = RejoinRequest::new(CONVERSATION, epoch, member(&world.bob.user), &mut OsRng)
      .sign(&world.bob.user.device, world.bob.user.certificate.clone())
      .unwrap();
    assert!(matches!(
      world
        .alice_group
        .check_rejoin_request(&not_a_member, &world.directory),
      Err(MlsError::ChangeDoesNotMatchCommit)
    ));
  }

  #[test]
  fn a_rejoin_commit_must_re_add_exactly_the_asking_device() {
    let mut world = World::new();
    let request = world.carol_request(world.alice_group.epoch());
    let mut aad = b"TCRJ".to_vec();
    aad.extend_from_slice(&request.to_bytes().unwrap());
    let carol_leaf = world.leaf_of(CAROL);

    // Carol's leaf goes, Dave comes in instead.
    let swapped = world.raw_alice_commit(&[world.dave.key_package()], &[carol_leaf], aad.clone());
    world.expect_carol_rejects(&swapped, |error| {
      matches!(error, MlsError::ChangeDoesNotMatchCommit)
    });
    // Only the removal.
    let removal = world.raw_alice_commit(&[], &[carol_leaf], aad);
    world.expect_carol_rejects(&removal, |error| {
      matches!(error, MlsError::ChangeDoesNotMatchCommit)
    });
  }

  impl World {
    fn carol_request(&self, epoch: u64) -> SignedRejoinRequest {
      RejoinRequest::new(CONVERSATION, epoch, member(&self.carol.user), &mut OsRng)
        .sign(&self.carol.user.device, self.carol.user.certificate.clone())
        .unwrap()
    }

    fn carol_key_package_in(&self, provider: &DefaultMlsProvider) -> Vec<u8> {
      create_key_package(
        provider,
        &self.carol.user.device,
        &self.carol.user.certificate,
        now_secs(),
      )
      .unwrap()
    }
  }
}

#[test]
fn last_resort_key_package_can_be_used_twice() {
  let mut world = World::new();
  let last_resort = crate::mls::create_last_resort_key_package(
    &world.bob.provider,
    &world.bob.user.device,
    &world.bob.user.certificate,
    now_secs(),
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

#[test]
fn group_keeps_working_after_its_key_package_is_deleted() {
  let mut world = World::new();
  let last_resort = crate::mls::create_last_resort_key_package(
    &world.bob.provider,
    &world.bob.user.device,
    &world.bob.user.certificate,
    now_secs(),
  )
  .unwrap();
  let add_bob = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
  let output = world.commit_change(&add_bob, std::slice::from_ref(&last_resort));
  let mut bob_group = world.bob_joins(&output);

  // e2ee.md §4.3.1: a replaced last-resort package's private part goes; the group keeps its own
  // leaf key.
  let hash_ref = crate::mls::key_package_ref(&last_resort).unwrap();
  crate::mls::delete_key_package(&world.bob.provider, &hash_ref).unwrap();
  assert!(
    crate::mls::stored_key_packages(&world.bob.provider)
      .unwrap()
      .is_empty()
  );

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
}
