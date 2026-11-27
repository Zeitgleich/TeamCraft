//! Strict roles (e2ee.md §4.6.2, issue #1197): the encoding, the hash chain, who may change the
//! roles, and the membership verdict, every role against every action.

use super::*;
use crate::ids::DeviceId;
use crate::membership::HistoryShare;
use crate::membership::tests::{TestAuthority, TestDirectory, TestUser};

const CONVERSATION: ConversationId = ConversationId::new(42);
const EPOCH: u64 = 10;
const ALICE: u64 = 1; // workspace admin
const CAROL: u64 = 3; // channel admin
const DAVE: u64 = 4; // moderator
const BOB: u64 = 2; // member

struct Fixture {
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  dave: TestUser,
  directory: TestDirectory,
  authority: TestAuthority,
}

impl Fixture {
  fn new() -> Self {
    let alice = TestUser::new(ALICE);
    let bob = TestUser::new(BOB);
    let carol = TestUser::new(CAROL);
    let dave = TestUser::new(DAVE);
    let directory = TestDirectory::with(&[&alice, &bob, &carol, &dave]);
    let authority = TestAuthority::allowing(&[alice.user_id]);
    Self {
      alice,
      bob,
      carol,
      dave,
      directory,
      authority,
    }
  }

  /// Everybody's device has a leaf.
  fn leaves(&self) -> Vec<TreeLeaf> {
    [&self.alice, &self.bob, &self.carol, &self.dave]
      .into_iter()
      .map(leaf_of)
      .collect()
  }

  /// Carol admin, Dave moderator.
  fn roles(&self) -> Vec<StrictRole> {
    vec![
      role(&self.carol, StrictRoleKind::Admin),
      role(&self.dave, StrictRoleKind::Moderator),
    ]
  }

  fn change(&self, version: u64, prev_hash: StrictHash, signer_role: StrictSignerRole) -> StrictRolesChange {
    StrictRolesChange {
      conversation_id: CONVERSATION,
      version,
      prev_hash,
      signed_for_epoch: EPOCH,
      signer_role,
      roster_version: if signer_role == StrictSignerRole::Workspace {
        1
      } else {
        0
      },
      role_log_version: 77,
      role_log_hash: StrictHash::from_bytes([7; 32]),
      roles: self.roles(),
    }
  }

  /// Alice, a workspace admin, turns the channel strict.
  fn turn_on(&self) -> SignedStrictRolesChange {
    self
      .change(1, StrictHash::ZERO, StrictSignerRole::Workspace)
      .sign(&self.alice.device, &self.alice.certificate)
      .unwrap()
  }

  fn strict(&self) -> StrictRoles {
    match self.check(None, &self.turn_on()).unwrap() {
      StrictRolesVerdict::Roles(roles) => roles,
      StrictRolesVerdict::Off => panic!("turned off"),
    }
  }

  fn check(
    &self,
    maybe_current: Option<&StrictRoles>,
    signed: &SignedStrictRolesChange,
  ) -> Result<StrictRolesVerdict, StrictRolesError> {
    self.check_with(maybe_current, signed, &self.leaves(), &self.authority)
  }

  fn check_with(
    &self,
    maybe_current: Option<&StrictRoles>,
    signed: &SignedStrictRolesChange,
    leaves: &[TreeLeaf],
    authority: &impl MembershipAuthority,
  ) -> Result<StrictRolesVerdict, StrictRolesError> {
    StrictRoles::check_change(
      maybe_current,
      signed,
      CONVERSATION,
      EPOCH,
      leaves,
      &self.directory,
      authority,
    )
  }

  /// A change of the current roles by Carol, the channel admin.
  fn admin_change(&self, current: &StrictRoles, roles: Vec<StrictRole>) -> SignedStrictRolesChange {
    let mut change = self.change(
      current.version() + 1,
      current.hash().unwrap(),
      StrictSignerRole::ChannelAdmin,
    );
    change.roles = roles;
    change.sign(&self.carol.device, &self.carol.certificate).unwrap()
  }
}

fn role(user: &TestUser, kind: StrictRoleKind) -> StrictRole {
  StrictRole {
    user_id: user.user_id,
    role: kind,
    identity_key: user.identity.public_key(),
  }
}

fn leaf_of(user: &TestUser) -> TreeLeaf {
  TreeLeaf {
    certificate: user.certificate.clone(),
    signature_key: user.device.public_keys().signing_key.as_bytes().to_vec(),
  }
}

mod encoding {
  use super::*;

  #[test]
  fn the_extension_round_trips_and_names_its_roles() {
    let fixture = Fixture::new();
    let strict = fixture.strict();
    let decoded = StrictRoles::from_bytes(&strict.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, strict);
    assert_eq!(decoded.version(), 1);
    assert_eq!(decoded.conversation_id(), CONVERSATION);
    assert_eq!(
      decoded.role_of(fixture.carol.user_id).unwrap().role,
      StrictRoleKind::Admin
    );
    assert_eq!(
      decoded.role_of(fixture.dave.user_id).unwrap().role,
      StrictRoleKind::Moderator
    );
    assert!(decoded.role_of(fixture.bob.user_id).is_none());
    assert_eq!(decoded.admins().count(), 1);
    let signed = SignedStrictRolesChange::from_bytes(&fixture.turn_on().to_bytes().unwrap()).unwrap();
    assert_eq!(signed.claimed_signer().unwrap(), fixture.alice.user_id);
  }

  #[test]
  fn a_newer_format_or_role_means_update_and_garbage_is_malformed() {
    let fixture = Fixture::new();
    let mut bytes = fixture.strict().to_bytes().unwrap();
    assert!(matches!(
      StrictRoles::from_bytes(&[]),
      Err(StrictRolesError::Malformed(_))
    ));
    bytes[0] = 2;
    assert!(matches!(
      StrictRoles::from_bytes(&bytes),
      Err(StrictRolesError::MustUpdate)
    ));
    let mut unknown_role = fixture.strict().to_bytes().unwrap();
    // The first role's kind byte sits after its user ID; replace every Admin byte that follows
    // Carol's ID with an unknown role.
    let carol = CAROL.to_be_bytes();
    let at = unknown_role
      .windows(8)
      .position(|window| window == carol)
      .unwrap();
    unknown_role[at + 8] = 9;
    assert!(matches!(
      StrictRoles::from_bytes(&unknown_role),
      Err(StrictRolesError::MustUpdate)
    ));
    let mut trailing = fixture.strict().to_bytes().unwrap();
    trailing.push(0);
    assert!(matches!(
      StrictRoles::from_bytes(&trailing),
      Err(StrictRolesError::Malformed(_))
    ));
  }

  #[test]
  fn an_oversized_extension_is_not_parsed() {
    let bytes = vec![STRICT_ROLES_FORMAT; MAX_STRICT_ROLES_BYTES + 1];
    assert!(matches!(
      StrictRoles::from_bytes(&bytes),
      Err(StrictRolesError::TooLarge { .. })
    ));
    assert!(matches!(
      SignedStrictRolesChange::from_bytes(&bytes),
      Err(StrictRolesError::TooLarge { .. })
    ));
  }

  #[test]
  fn the_chain_hash_is_labelled_and_ignores_the_signature() {
    let fixture = Fixture::new();
    let change = fixture.change(1, StrictHash::ZERO, StrictSignerRole::Workspace);
    let by_alice = change
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    let again = change
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert_eq!(by_alice.change().hash().unwrap(), again.change().hash().unwrap());
    let plain: [u8; 32] = Sha256::digest(change.tls_serialize_detached().unwrap()).into();
    assert_ne!(change.hash().unwrap().as_bytes(), &plain);
    let mut other = change.clone();
    other.version = 2;
    assert_ne!(other.hash().unwrap(), change.hash().unwrap());
  }
}

mod turning_on {
  use super::*;

  #[test]
  fn a_workspace_admin_turns_it_on() {
    let fixture = Fixture::new();
    let roles = fixture.strict();
    assert_eq!(roles.roles(), fixture.roles().as_slice());
  }

  /// D3: until #1171, only workspace owners and admins. A channel admin of the role log is trust
  /// on first use and could lock the real admins out.
  #[test]
  fn nobody_else_turns_it_on() {
    let fixture = Fixture::new();
    let by_member = fixture
      .change(1, StrictHash::ZERO, StrictSignerRole::Workspace)
      .sign(&fixture.bob.device, &fixture.bob.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &by_member),
      Err(StrictRolesError::NotAllowed(_))
    ));
    let as_channel_admin = fixture
      .change(1, StrictHash::ZERO, StrictSignerRole::ChannelAdmin)
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &as_channel_admin),
      Err(StrictRolesError::NotAllowed(_))
    ));
  }

  #[test]
  fn turning_on_starts_the_chain_and_names_an_admin_with_a_leaf() {
    let fixture = Fixture::new();
    let mut chained = fixture.change(1, StrictHash::from_bytes([1; 32]), StrictSignerRole::Workspace);
    let signed = chained
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::BrokenChain)
    ));

    chained.prev_hash = StrictHash::ZERO;
    chained.roles = vec![role(&fixture.dave, StrictRoleKind::Moderator)];
    let no_admin = chained
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &no_admin),
      Err(StrictRolesError::NoAdmin)
    ));

    chained.roles = Vec::new();
    let empty = chained
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &empty),
      Err(StrictRolesError::NotStrict)
    ));

    chained.roles = fixture.roles();
    let signed = chained
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    let without_carol: Vec<TreeLeaf> = [&fixture.alice, &fixture.bob, &fixture.dave]
      .into_iter()
      .map(leaf_of)
      .collect();
    assert!(matches!(
      fixture.check_with(None, &signed, &without_carol, &fixture.authority),
      Err(StrictRolesError::NoAdminWithLeaf)
    ));
  }

  /// An admin's leaf counts only under the identity key the roles name.
  #[test]
  fn an_admin_named_with_another_key_has_no_leaf() {
    let fixture = Fixture::new();
    let mut change = fixture.change(1, StrictHash::ZERO, StrictSignerRole::Workspace);
    change.roles = vec![StrictRole {
      identity_key: fixture.bob.identity.public_key(),
      ..role(&fixture.carol, StrictRoleKind::Admin)
    }];
    let signed = change
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::NoAdminWithLeaf)
    ));
  }

  /// A failed roster read holds the entry back; it isn't refused for it.
  #[test]
  fn an_unverifiable_roster_holds_it_back() {
    let fixture = Fixture::new();
    assert!(matches!(
      fixture.check_with(None, &fixture.turn_on(), &fixture.leaves(), &UnknownRoster),
      Err(StrictRolesError::RosterUnknown(1))
    ));
  }

  #[test]
  fn roles_must_be_ordered_unique_and_few() {
    let fixture = Fixture::new();
    let mut change = fixture.change(1, StrictHash::ZERO, StrictSignerRole::Workspace);
    change.roles.reverse();
    let signed = change
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::UnorderedRoles)
    ));
    change.roles = vec![role(&fixture.carol, StrictRoleKind::Admin); 2];
    let signed = change
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::UnorderedRoles)
    ));
    change.roles = (1..=MAX_STRICT_ROLES as u64 + 1)
      .map(|id| StrictRole {
        user_id: UserId::new(id),
        role: StrictRoleKind::Admin,
        identity_key: fixture.carol.identity.public_key(),
      })
      .collect();
    let signed = change
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::TooManyRoles(65))
    ));
  }
}

mod changes {
  use super::*;

  #[test]
  fn an_admin_with_a_leaf_changes_the_roles_and_turns_it_off() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let mut roles = fixture.roles();
    roles.insert(0, role(&fixture.bob, StrictRoleKind::Moderator));
    let promoted = fixture.admin_change(&current, roles.clone());
    let StrictRolesVerdict::Roles(next) = fixture.check(Some(&current), &promoted).unwrap() else {
      panic!("turned off")
    };
    assert_eq!(next.version(), 2);
    assert_eq!(next.roles(), roles.as_slice());
    let off = fixture.admin_change(&next, Vec::new());
    assert_eq!(fixture.check(Some(&next), &off).unwrap(), StrictRolesVerdict::Off);
  }

  #[test]
  fn a_workspace_admin_changes_them_too() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let mut change = fixture.change(2, current.hash().unwrap(), StrictSignerRole::Workspace);
    change.roles = vec![role(&fixture.carol, StrictRoleKind::Admin)];
    let signed = change
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &signed),
      Ok(StrictRolesVerdict::Roles(_))
    ));
  }

  #[test]
  fn a_moderator_member_or_outsider_changes_nothing() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let change = fixture.change(2, current.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    for user in [&fixture.dave, &fixture.bob] {
      let signed = change.clone().sign(&user.device, &user.certificate).unwrap();
      assert!(matches!(
        fixture.check(Some(&current), &signed),
        Err(StrictRolesError::NotAllowed(_))
      ));
    }
    // A member who claims to be a workspace signer: the roster says no.
    let mut as_workspace = change.clone();
    as_workspace.signer_role = StrictSignerRole::Workspace;
    let signed = as_workspace
      .sign(&fixture.bob.device, &fixture.bob.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &signed),
      Err(StrictRolesError::NotAllowed(_))
    ));
  }

  /// "A role counts only with a leaf": an admin who left, or a device of theirs not in the tree.
  #[test]
  fn an_admin_without_a_leaf_changes_nothing() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let carol_phone = fixture.carol.new_device(301);
    let change = fixture.change(2, current.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    let signed = change
      .sign(&carol_phone.device, &carol_phone.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &signed),
      Err(StrictRolesError::NotAllowed(_))
    ));
  }

  /// The context names the key an admin signs under; a device certified by another identity key
  /// (a server's invention) isn't that admin, whatever the directory says.
  #[test]
  fn a_device_of_another_identity_is_not_the_admin() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let impostor = TestUser::new(CAROL);
    let change = fixture.change(2, current.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    let signed = change.sign(&impostor.device, &impostor.certificate).unwrap();
    let mut leaves = fixture.leaves();
    leaves.push(leaf_of(&impostor));
    assert!(matches!(
      fixture.check_with(Some(&current), &signed, &leaves, &fixture.authority),
      Err(StrictRolesError::Signature(_))
    ));
  }

  #[test]
  fn replays_and_forks_of_the_chain_are_refused() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    // The change that set the current roles, again.
    assert!(matches!(
      fixture.check(Some(&current), &fixture.turn_on()),
      Err(StrictRolesError::WrongVersion {
        expected: 2,
        offered: 1
      })
    ));
    let skipped = fixture.change(3, current.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    let signed = skipped
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &signed),
      Err(StrictRolesError::WrongVersion { .. })
    ));
    let forked = fixture.change(2, StrictHash::from_bytes([9; 32]), StrictSignerRole::ChannelAdmin);
    let signed = forked
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &signed),
      Err(StrictRolesError::BrokenChain)
    ));
  }

  #[test]
  fn stale_future_and_foreign_changes_are_refused() {
    let fixture = Fixture::new();
    let current = fixture.strict();
    let mut change = fixture.change(2, current.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    change.signed_for_epoch = EPOCH + 1;
    let future = change
      .clone()
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &future),
      Err(StrictRolesError::Stale { .. })
    ));
    change.signed_for_epoch = 0;
    let at_edge = StrictRoles::check_change(
      Some(&current),
      &change
        .clone()
        .sign(&fixture.carol.device, &fixture.carol.certificate)
        .unwrap(),
      CONVERSATION,
      STRICT_CHANGE_WINDOW_EPOCHS + 1,
      &fixture.leaves(),
      &fixture.directory,
      &fixture.authority,
    );
    assert!(matches!(at_edge, Err(StrictRolesError::Stale { .. })));
    change.signed_for_epoch = EPOCH;
    change.conversation_id = ConversationId::new(43);
    let foreign = change
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.check(Some(&current), &foreign),
      Err(StrictRolesError::WrongConversation(_))
    ));
  }

  #[test]
  fn a_tampered_change_does_not_verify() {
    let fixture = Fixture::new();
    let mut signed = fixture.turn_on();
    signed.change.role_log_version += 1;
    assert!(matches!(
      fixture.check(None, &signed),
      Err(StrictRolesError::Signature(_))
    ));
  }

  /// Domain separation: a §4.6.1 role log entry with the same bytes, version and signer (which a
  /// founder may have signed on trust at first use) never passes as a roles change.
  #[test]
  fn an_entry_of_another_log_kind_is_not_a_roles_change() {
    let fixture = Fixture::new();
    let change = fixture.change(1, StrictHash::ZERO, StrictSignerRole::Workspace);
    let payload = change.tls_serialize_detached().unwrap();
    for log in [
      WorkspaceLog::ChannelRoles,
      WorkspaceLog::ChannelProperties,
      WorkspaceLog::Roster,
    ] {
      let entry = sign_log_entry(
        &fixture.alice.device,
        &fixture.alice.certificate,
        log,
        1,
        &payload,
      )
      .unwrap();
      let forged = SignedStrictRolesChange {
        change: change.clone(),
        entry: VLBytes::new(entry),
      };
      assert!(matches!(
        fixture.check(None, &forged),
        Err(StrictRolesError::Signature(_))
      ));
    }
  }
}

/// The table of e2ee.md §4.6.2, "Who may do what", every signer class against every action.
mod verdict {
  use super::*;

  const SIGNER: UserId = UserId::new(10);
  const OTHER: UserId = UserId::new(11);

  fn actions(target: UserId) -> [MembershipAction; 3] {
    [
      MembershipAction::AddUser {
        user_id: target,
        history: HistoryShare::FromJoin,
      },
      MembershipAction::RemoveUser { user_id: target },
      MembershipAction::RemoveDevice {
        user_id: target,
        device_id: DeviceId::new(5),
      },
    ]
  }

  #[test]
  fn every_signer_against_every_action() {
    use StrictSigner::{ChannelAdmin, Moderator, Other, Workspace};
    let member = None;
    let moderator = Some(StrictRoleKind::Moderator);
    let admin = Some(StrictRoleKind::Admin);
    // (signer, target role, [add, remove user, remove device]) for another user.
    let table = [
      (ChannelAdmin, member, [true, true, true]),
      (ChannelAdmin, moderator, [true, true, true]),
      (ChannelAdmin, admin, [true, true, true]),
      (Moderator, member, [false, true, false]),
      (Moderator, moderator, [false, false, false]),
      (Moderator, admin, [false, false, false]),
      (Workspace, member, [true, true, true]),
      (Workspace, admin, [true, true, true]),
      (Other, member, [false, false, false]),
      (Other, moderator, [false, false, false]),
      (Other, admin, [false, false, false]),
    ];
    for (signer, target_role, expected) in table {
      for (action, allowed) in actions(OTHER).iter().zip(expected) {
        assert_eq!(
          membership_verdict(SIGNER, signer, action, target_role),
          allowed,
          "{signer:?} on {action:?} of a {target_role:?}"
        );
      }
    }
  }

  /// Leaving is always allowed, the last admin included; adding oneself never is (no self-add).
  #[test]
  fn everybody_leaves_and_nobody_adds_themselves() {
    for signer in [
      StrictSigner::ChannelAdmin,
      StrictSigner::Moderator,
      StrictSigner::Workspace,
      StrictSigner::Other,
    ] {
      for target_role in [None, Some(StrictRoleKind::Moderator), Some(StrictRoleKind::Admin)] {
        let [add, remove_user, remove_device] = actions(SIGNER);
        assert!(membership_verdict(SIGNER, signer, &remove_user, target_role));
        assert!(membership_verdict(SIGNER, signer, &remove_device, target_role));
        let may_add = matches!(signer, StrictSigner::ChannelAdmin | StrictSigner::Workspace);
        assert_eq!(membership_verdict(SIGNER, signer, &add, target_role), may_add);
      }
    }
  }

  #[test]
  fn a_signer_class_needs_the_named_key_and_a_leaf() {
    let fixture = Fixture::new();
    let strict = fixture.strict();
    let leaves = fixture.leaves();
    let verified = |user: &TestUser| user.certificate.verify(&user.identity.public_key()).unwrap();
    assert_eq!(
      strict.signer_class(&verified(&fixture.carol), &leaves),
      StrictSigner::ChannelAdmin
    );
    assert_eq!(
      strict.signer_class(&verified(&fixture.dave), &leaves),
      StrictSigner::Moderator
    );
    assert_eq!(
      strict.signer_class(&verified(&fixture.bob), &leaves),
      StrictSigner::Other
    );
    assert_eq!(
      strict.signer_class(&verified(&fixture.alice), &leaves),
      StrictSigner::Other
    );
    let carol_phone = fixture.carol.new_device(301);
    assert_eq!(
      strict.signer_class(&verified(&carol_phone), &leaves),
      StrictSigner::Other
    );
    let impostor = TestUser::new(CAROL);
    let mut with_impostor = leaves.clone();
    with_impostor.push(leaf_of(&impostor));
    assert_eq!(
      strict.signer_class(&verified(&impostor), &with_impostor),
      StrictSigner::Other
    );
  }
}

/// A device that fell out carries the roles it knew forward along the changes the server kept
/// (e2ee.md §4.6.2, "Falling out in a strict channel"): of each version, the change that continues
/// the chain and whose signer the previous roles allow; a missing link or a fork refuses it.
mod chain {
  use super::*;

  #[test]
  fn an_empty_chain_ends_at_the_known_roles() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let end = fixture.verify(&known, &[]).unwrap();
    assert_eq!(end, ChainEnd::Roles(known.clone()));
    assert_eq!(end.version(), 1);
    assert_eq!(end.maybe_roles(), Some(&known));
  }

  #[test]
  fn an_admins_changes_carry_the_roles_forward_and_turn_strict_off() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let mut roles = fixture.roles();
    roles.insert(0, role(&fixture.bob, StrictRoleKind::Moderator));
    let promoted = fixture.admin_change(&known, roles.clone());
    let second = after(&promoted);
    let off = fixture.admin_change(&second, Vec::new());

    let end = fixture.verify(&known, &[&promoted]).unwrap();
    assert_eq!(end, ChainEnd::Roles(second.clone()));
    assert_eq!(end.maybe_roles().unwrap().roles(), roles.as_slice());
    let end = fixture.verify(&known, &[&promoted, &off]).unwrap();
    assert_eq!(
      end,
      ChainEnd::Off {
        version: 3,
        hash: hash_of(&off)
      }
    );
    assert_eq!(end.maybe_roles(), None);
  }

  /// A workspace signer is judged by the roster the change names, not the newest one: the device
  /// judges a change made while it was away.
  #[test]
  fn a_workspace_signer_is_judged_as_named() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let by_alice = fixture.workspace_change(&known, vec![role(&fixture.bob, StrictRoleKind::Admin)]);

    let end = verify_using(
      &known,
      &[&by_alice],
      &fixture.directory,
      &OnlyAsNamed(fixture.alice.user_id),
    )
    .unwrap();
    assert_eq!(end.version(), 2);
    assert_eq!(
      end
        .maybe_roles()
        .unwrap()
        .role_of(fixture.bob.user_id)
        .unwrap()
        .role,
      StrictRoleKind::Admin
    );
  }

  /// "Unknown" holds: the roster or the admin's device list can't be verified yet. Nothing is
  /// refused for it; the caller reads them again.
  #[test]
  fn an_unverifiable_roster_or_device_list_is_a_retry() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let by_alice = fixture.workspace_change(&known, fixture.roles());
    assert!(matches!(
      verify_using(&known, &[&by_alice], &fixture.directory, &UnknownRoster),
      Err(StrictRolesError::RosterUnknown(1))
    ));
    let by_carol = fixture.admin_change(&known, fixture.roles());
    let unlisted = Listing(&fixture.directory, Standing::Unknown);
    assert!(matches!(
      verify_using(&known, &[&by_carol], &unlisted, &fixture.authority),
      Err(StrictRolesError::DeviceListUnknown(user)) if user == fixture.carol.user_id
    ));
  }

  /// A change whose signer can't be judged yet may be the one the group took: a valid change of
  /// the same version beside it doesn't settle the step.
  #[test]
  fn an_unjudgeable_change_beside_a_valid_one_is_a_retry() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let by_carol = fixture.admin_change(&known, fixture.roles());
    let by_alice = fixture.workspace_change(&known, fixture.roles());
    assert!(matches!(
      verify_using(
        &known,
        &[&by_carol, &by_alice],
        &fixture.directory,
        &UnknownRoster
      ),
      Err(StrictRolesError::RosterUnknown(1))
    ));
  }

  /// The device list counts only under the identity key the roles name: one this device pinned
  /// differently (a key change, or a server's invention) says nothing about the admin's device.
  #[test]
  fn a_device_list_under_another_key_is_a_retry() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let by_carol = fixture.admin_change(&known, fixture.roles());
    let other = TestUser::new(CAROL);
    let pinned = vec![&fixture.alice, &fixture.bob, &fixture.dave, &other];
    let repinned = TestDirectory::with(&pinned);
    assert!(matches!(
      verify_using(&known, &[&by_carol], &repinned, &fixture.authority),
      Err(StrictRolesError::DeviceListUnknown(user)) if user == fixture.carol.user_id
    ));
  }

  /// Changes nobody allowed signed are passed over: the chain ends before them. A revoked admin
  /// device, a moderator, a member, a workspace admin signing as channel admin, an admin demoted
  /// in the chain, an impostor's key: none moves the roles.
  #[test]
  fn changes_nobody_allowed_signed_are_passed_over() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let revoked = Listing(&fixture.directory, Standing::Refused);
    let by_carol = fixture.admin_change(&known, Vec::new());
    assert_eq!(
      verify_using(&known, &[&by_carol], &revoked, &fixture.authority).unwrap(),
      ChainEnd::Roles(known.clone())
    );
    let change = fixture.change(2, known.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    for user in [&fixture.dave, &fixture.bob, &fixture.alice, &TestUser::new(CAROL)] {
      let signed = change.clone().sign(&user.device, &user.certificate).unwrap();
      assert_eq!(
        fixture.verify(&known, &[&signed]).unwrap(),
        ChainEnd::Roles(known.clone()),
        "{:?}",
        user.user_id
      );
    }
    let mut as_workspace = change;
    as_workspace.signer_role = StrictSignerRole::Workspace;
    let by_member = as_workspace
      .sign(&fixture.bob.device, &fixture.bob.certificate)
      .unwrap();
    assert_eq!(
      fixture.verify(&known, &[&by_member]).unwrap(),
      ChainEnd::Roles(known.clone())
    );
    // Carol hands the channel to Dave, then signs again as if she were still its admin.
    let to_dave = fixture.admin_change(&known, vec![role(&fixture.dave, StrictRoleKind::Admin)]);
    let after_dave = after(&to_dave);
    let by_carol_again = fixture.admin_change(&after_dave, Vec::new());
    assert_eq!(
      fixture.verify(&known, &[&to_dave, &by_carol_again]).unwrap(),
      ChainEnd::Roles(after_dave)
    );
  }

  /// A member plants junk beside the real change, or before it (the server keeps every commit's):
  /// passed over, the real change is taken, and nothing blocks the chain.
  #[test]
  fn junk_beside_the_real_change_is_passed_over() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let real = fixture.admin_change(&known, fixture.roles());
    let real_bytes = real.to_bytes().unwrap();
    let mut foreign = fixture.change(2, known.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    foreign.conversation_id = ConversationId::new(43);
    let foreign = foreign
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap()
      .to_bytes()
      .unwrap();
    let mut tampered = fixture.admin_change(&known, Vec::new());
    tampered.change.role_log_version += 1;
    let tampered = tampered.to_bytes().unwrap();
    let mislabelled = fixture
      .admin_change(&after(&real), Vec::new())
      .to_bytes()
      .unwrap();
    let links = [
      link(2, b"junk"),
      link(2, &foreign),
      link(2, &tampered),
      link(2, &mislabelled),
      link(2, &real_bytes),
      link(2, &real_bytes),
    ];
    let end = verify_chain(&known, &links, &fixture.directory, &fixture.authority).unwrap();
    assert_eq!(end, ChainEnd::Roles(after(&real)), "the real one, once");
  }

  /// A change signed for a predecessor the chain doesn't hold: the chain misses a link or forks,
  /// and is refused rather than ended there.
  #[test]
  fn a_change_for_another_predecessor_refuses_the_chain() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let second_change = fixture.admin_change(&known, fixture.roles());
    let third_change = fixture.admin_change(&after(&second_change), Vec::new());
    let mut other_roles = fixture.roles();
    other_roles.insert(0, role(&fixture.bob, StrictRoleKind::Moderator));
    // Another version 2, signed by the admin, that the third change doesn't continue.
    let fork = fixture.admin_change(&known, other_roles);
    assert!(matches!(
      fixture.verify(&known, &[&fork, &third_change]),
      Err(StrictRolesError::BrokenChain)
    ));
    let unchained = fixture
      .change(2, StrictHash::from_bytes([9; 32]), StrictSignerRole::ChannelAdmin)
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert!(matches!(
      fixture.verify(&known, &[&unchained]),
      Err(StrictRolesError::BrokenChain)
    ));
  }

  /// Two changes of one version that both continue the chain refuse it, wherever they are: the
  /// longer branch isn't taken, or an admin demoted later could sign one a version longer than the
  /// real one (each link as admin of the one before) and have it win.
  #[test]
  fn a_fork_anywhere_refuses_the_chain() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let taken = fixture.admin_change(&known, fixture.roles());
    let mut other_roles = fixture.roles();
    other_roles.insert(0, role(&fixture.bob, StrictRoleKind::Moderator));
    let refused = fixture.admin_change(&known, other_roles);
    let third = fixture.admin_change(&after(&taken), Vec::new());
    // Carol's own branch, one longer than the real chain.
    let branch_third = fixture.admin_change(&after(&refused), fixture.roles());
    let branch_fourth = fixture.admin_change(&after(&branch_third), Vec::new());

    for chain in [
      vec![&refused, &taken],
      vec![&refused, &taken, &third],
      vec![&taken, &refused, &branch_third, &branch_fourth],
    ] {
      assert!(matches!(
        fixture.verify(&known, &chain),
        Err(StrictRolesError::AmbiguousChain)
      ));
    }
  }

  #[test]
  fn gaps_replays_and_disorder_are_refused() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let second_change = fixture.admin_change(&known, fixture.roles());
    let third_change = fixture.admin_change(&after(&second_change), Vec::new());
    // The second change left out.
    assert!(matches!(
      fixture.verify(&known, &[&third_change]),
      Err(StrictRolesError::WrongVersion {
        expected: 2,
        offered: 3
      })
    ));
    // Only junk at version 2, then a third change.
    let third_bytes = third_change.to_bytes().unwrap();
    let links = [link(2, b"junk"), link(3, &third_bytes)];
    assert!(matches!(
      verify_chain(&known, &links, &fixture.directory, &fixture.authority),
      Err(StrictRolesError::ChainGap(2))
    ));
    // The change that set the known roles, again.
    assert!(matches!(
      fixture.verify(&known, &[&fixture.turn_on()]),
      Err(StrictRolesError::WrongVersion { .. })
    ));
    // Out of order.
    let second_bytes = second_change.to_bytes().unwrap();
    let links = [link(3, &third_bytes), link(2, &second_bytes)];
    assert!(matches!(
      verify_chain(&known, &links, &fixture.directory, &fixture.authority),
      Err(StrictRolesError::WrongVersion { .. })
    ));
  }

  /// After "off", the chain goes on only where strict is turned on again: by a workspace signer,
  /// from a zero hash, at the next version.
  #[test]
  fn after_off_only_turning_it_on_again_continues() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let off = fixture.admin_change(&known, Vec::new());
    let off_end = ChainEnd::Off {
      version: 2,
      hash: hash_of(&off),
    };
    let mut again = fixture.change(3, StrictHash::ZERO, StrictSignerRole::Workspace);
    let on_again = again
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    let end = fixture.verify(&known, &[&off, &on_again]).unwrap();
    assert_eq!(end.version(), 3);
    assert_eq!(end.maybe_roles().unwrap().roles(), fixture.roles().as_slice());

    let by_carol = again
      .clone()
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert_eq!(fixture.verify(&known, &[&off, &by_carol]).unwrap(), off_end);
    again.prev_hash = hash_of(&off);
    let chained_to_off = again
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert_eq!(fixture.verify(&known, &[&off, &chained_to_off]).unwrap(), off_end);
    again.prev_hash = StrictHash::ZERO;
    again.roles = Vec::new();
    let off_again = again
      .clone()
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert_eq!(fixture.verify(&known, &[&off, &off_again]).unwrap(), off_end);
    // A turning on while strict continues nothing.
    again.version = 2;
    again.roles = fixture.roles();
    let while_strict = again
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    assert_eq!(
      fixture.verify(&known, &[&while_strict]).unwrap(),
      ChainEnd::Roles(known.clone())
    );
  }

  #[test]
  fn unordered_and_adminless_roles_are_passed_over() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let mut change = fixture.change(2, known.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    change.roles.reverse();
    let unordered = change
      .clone()
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    change.roles = vec![role(&fixture.dave, StrictRoleKind::Moderator)];
    let adminless = change
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    for signed in [&unordered, &adminless] {
      assert_eq!(
        fixture.verify(&known, &[signed]).unwrap(),
        ChainEnd::Roles(known.clone())
      );
    }
  }

  /// Junk is passed over, but a change of a newer format may be the real one: the app has to
  /// update.
  #[test]
  fn oversized_links_are_passed_over_and_newer_ones_refuse() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let oversized = vec![0; MAX_STRICT_ROLES_BYTES + 1];
    let links = [link(2, &oversized)];
    assert_eq!(
      verify_chain(&known, &links, &fixture.directory, &fixture.authority).unwrap(),
      ChainEnd::Roles(known.clone())
    );
    let mut newer = fixture.admin_change(&known, fixture.roles()).to_bytes().unwrap();
    assert_eq!(newer[SIGNER_ROLE_OFFSET], StrictSignerRole::ChannelAdmin as u8);
    newer[SIGNER_ROLE_OFFSET] = 9;
    let links = [link(2, &newer)];
    assert!(matches!(
      verify_chain(&known, &links, &fixture.directory, &fixture.authority),
      Err(StrictRolesError::MustUpdate)
    ));
  }

  /// No epoch window: the device doesn't know the epochs.
  #[test]
  fn a_change_signed_in_any_epoch_continues_the_chain() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let mut change = fixture.change(2, known.hash().unwrap(), StrictSignerRole::ChannelAdmin);
    change.signed_for_epoch = EPOCH + 10 * STRICT_CHANGE_WINDOW_EPOCHS;
    let signed = change
      .sign(&fixture.carol.device, &fixture.carol.certificate)
      .unwrap();
    assert_eq!(fixture.verify(&known, &[&signed]).unwrap().version(), 2);
  }

  /// Versions end at what the server stores: nothing follows the highest, in a group as along the
  /// chain, and turning on there (or above) is refused too.
  #[test]
  fn the_highest_version_has_no_next() {
    let fixture = Fixture::new();
    for version in [MAX_STRICT_ROLES_VERSION, u64::MAX] {
      let top = fixture
        .change(version, StrictHash::ZERO, StrictSignerRole::Workspace)
        .sign(&fixture.alice.device, &fixture.alice.certificate)
        .unwrap();
      assert!(matches!(
        fixture.check(None, &top),
        Err(StrictRolesError::VersionExhausted(at)) if at == version
      ));
    }
    let below_top = fixture
      .change(
        MAX_STRICT_ROLES_VERSION - 1,
        StrictHash::ZERO,
        StrictSignerRole::Workspace,
      )
      .sign(&fixture.alice.device, &fixture.alice.certificate)
      .unwrap();
    let StrictRolesVerdict::Roles(below_top) = fixture.check(None, &below_top).unwrap() else {
      panic!("turned off")
    };
    let to_top = fixture.admin_change(&below_top, fixture.roles());
    assert_eq!(to_top.change().version, MAX_STRICT_ROLES_VERSION);
    assert!(fixture.check(Some(&below_top), &to_top).is_ok());
    let at_top = after(&to_top);
    let beyond = fixture.admin_change(&at_top, fixture.roles());
    assert!(matches!(
      fixture.check(Some(&at_top), &beyond),
      Err(StrictRolesError::VersionExhausted(MAX_STRICT_ROLES_VERSION))
    ));
    let bytes = beyond.to_bytes().unwrap();
    assert!(matches!(
      verify_chain(
        &at_top,
        &[link(beyond.change().version, &bytes)],
        &fixture.directory,
        &fixture.authority
      ),
      Err(StrictRolesError::VersionExhausted(MAX_STRICT_ROLES_VERSION))
    ));
  }

  /// The step-by-step verifier names the signers a step needs, of the links that continue the
  /// chain only.
  #[test]
  fn a_step_names_the_signers_it_needs() {
    let fixture = Fixture::new();
    let known = fixture.strict();
    let by_carol = fixture.admin_change(&known, fixture.roles()).to_bytes().unwrap();
    let by_alice = fixture
      .workspace_change(&known, fixture.roles())
      .to_bytes()
      .unwrap();
    let unchained = fixture
      .change(2, StrictHash::from_bytes([9; 32]), StrictSignerRole::ChannelAdmin)
      .sign(&fixture.dave.device, &fixture.dave.certificate)
      .unwrap()
      .to_bytes()
      .unwrap();
    let verifier = ChainVerifier::new(&known);
    let links = [link(2, &by_carol), link(2, &unchained), link(3, &by_alice)];
    assert_eq!(
      verifier.signers_for_next(&links),
      ChainSigners {
        users: vec![fixture.carol.user_id],
        has_workspace_signer: false,
      }
    );
    let links = [link(2, &by_alice), link(2, &by_carol)];
    assert_eq!(
      verifier.signers_for_next(&links),
      ChainSigners {
        users: vec![fixture.alice.user_id, fixture.carol.user_id],
        has_workspace_signer: true,
      }
    );
  }

  impl Fixture {
    fn verify(
      &self,
      known: &StrictRoles,
      chain: &[&SignedStrictRolesChange],
    ) -> Result<ChainEnd, StrictRolesError> {
      verify_using(known, chain, &self.directory, &self.authority)
    }

    /// A change of `current` by Alice, a workspace admin.
    fn workspace_change(&self, current: &StrictRoles, roles: Vec<StrictRole>) -> SignedStrictRolesChange {
      let mut change = self.change(
        current.version() + 1,
        current.hash().unwrap(),
        StrictSignerRole::Workspace,
      );
      change.roles = roles;
      change.sign(&self.alice.device, &self.alice.certificate).unwrap()
    }
  }

  /// Where the signer role byte of an encoded change sits: after the conversation, the version,
  /// the previous hash and the epoch.
  const SIGNER_ROLE_OFFSET: usize = 8 + 8 + 32 + 8;

  /// [`verify_chain`] over `chain`, each change stored under its own version.
  fn verify_using(
    known: &StrictRoles,
    chain: &[&SignedStrictRolesChange],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<ChainEnd, StrictRolesError> {
    let bytes: Vec<(u64, Vec<u8>)> = chain
      .iter()
      .map(|signed| (signed.change().version, signed.to_bytes().unwrap()))
      .collect();
    let links: Vec<ChainLink<'_>> = bytes
      .iter()
      .map(|(version, change)| link(*version, change))
      .collect();
    verify_chain(known, &links, directory, authority)
  }

  fn link(version: u64, change: &[u8]) -> ChainLink<'_> {
    ChainLink { version, change }
  }

  /// The roles `signed` sets, unchecked: what the next change continues.
  fn after(signed: &SignedStrictRolesChange) -> StrictRoles {
    StrictRoles::for_tests(signed.clone())
  }

  fn hash_of(signed: &SignedStrictRolesChange) -> StrictHash {
    signed.change().hash().unwrap()
  }

  /// A directory whose device lists answer `.1` for every device.
  struct Listing<'a>(&'a TestDirectory, Standing);

  impl IdentityDirectory for Listing<'_> {
    fn pinned_identity(&self, user_id: UserId) -> Option<IdentityPublicKey> {
      self.0.pinned_identity(user_id)
    }

    fn device_listing(&self, _device: &VerifiedDevice, _list_version: u64, _judged: Judged) -> Standing {
      self.1
    }
  }

  /// One workspace admin, confirmed only when judged as of the named roster.
  struct OnlyAsNamed(UserId);

  impl MembershipAuthority for OnlyAsNamed {
    fn is_authorized(&self, _: UserId, _: ConversationId, _: &MembershipAction) -> bool {
      true
    }

    fn workspace_signer(&self, user_id: UserId, _roster_version: u64, judged: Judged) -> Standing {
      match (user_id == self.0, judged) {
        (true, Judged::AsNamed) => Standing::Confirmed,
        (true, Judged::Newest) => Standing::Unknown,
        (false, _) => Standing::Refused,
      }
    }
  }
}

/// A roster this device can't verify yet.
struct UnknownRoster;

impl MembershipAuthority for UnknownRoster {
  fn is_authorized(&self, _: UserId, _: ConversationId, _: &MembershipAction) -> bool {
    true
  }

  fn workspace_signer(&self, _: UserId, _: u64, _: Judged) -> Standing {
    Standing::Unknown
  }
}
