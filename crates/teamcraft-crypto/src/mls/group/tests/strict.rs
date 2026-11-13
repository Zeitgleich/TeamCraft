//! Strict channels (e2ee.md §4.6.2, issue #1197): the roles in the group context, turning them on,
//! every refusal a modified client or a hostile server can try, and the honest paths beside them.
//! Every refused commit leaves every member in its epoch.

use std::collections::BTreeMap;

use openmls::prelude::{Capabilities, KeyPackage};

use super::*;
use crate::channel_anchor::{AnchorKind, ChannelAnchor, Founding, Judged, Standing};
use crate::ids::MeetingId;
use crate::strict_roles::{
  StrictHash, StrictRole, StrictRoleKind, StrictRolesChange, StrictRolesError, StrictSignerRole,
};

const EVE: u64 = 5;

/// Alice is a workspace admin without a channel role; Carol the channel admin, Dave a moderator,
/// Bob a plain member. Eve is outside.
struct StrictWorld {
  clients: BTreeMap<u64, Client>,
  groups: BTreeMap<u64, ConversationGroup>,
  directory: TestDirectory,
  authority: TestAuthority,
}

impl StrictWorld {
  fn new() -> Self {
    let mut world = Self::unstrict(&[]);
    world.turn_on();
    world
  }

  /// The group before it turns strict: Alice, Carol, Dave, Bob. The users in `old_apps` join
  /// with a leaf that doesn't announce strict channels.
  fn unstrict(old_apps: &[u64]) -> Self {
    let clients: BTreeMap<u64, Client> = [ALICE, BOB, CAROL, DAVE, EVE]
      .into_iter()
      .map(|id| (id, Client::new(id)))
      .collect();
    let users: Vec<&TestUser> = clients.values().map(|client| &client.user).collect();
    let directory = TestDirectory::with(&users);
    let authority = TestAuthority::allowing(&[UserId::new(ALICE)]);
    let alice = &clients[&ALICE];
    let group = ConversationGroup::create(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
    )
    .unwrap();
    let mut world = Self {
      groups: BTreeMap::from([(ALICE, group)]),
      clients,
      directory,
      authority,
    };
    for user in [CAROL, DAVE, BOB] {
      let key_package = if old_apps.contains(&user) {
        world.clients[&user].key_package_without_strict()
      } else {
        world.clients[&user].key_package()
      };
      let change = world.signed(ALICE, add(user, HistoryShare::Full));
      world.commit_change(ALICE, &change, &[key_package]).unwrap();
    }
    world
  }

  fn user(&self, id: u64) -> &TestUser {
    &self.clients[&id].user
  }

  fn group(&self, id: u64) -> &ConversationGroup {
    &self.groups[&id]
  }

  fn epoch(&self) -> u64 {
    self.groups.values().next().unwrap().epoch()
  }

  fn signed(&self, signer: u64, action: MembershipAction) -> SignedMembershipChange {
    self.signed_by(self.user(signer), action)
  }

  fn signed_by(&self, user: &TestUser, action: MembershipAction) -> SignedMembershipChange {
    MembershipChange::new(CONVERSATION, self.epoch(), action, &mut OsRng)
      .sign(&user.device, user.certificate.clone())
      .unwrap()
  }

  /// The roles as Alice turns them on: Carol admin, Dave moderator.
  fn roles(&self) -> Vec<StrictRole> {
    vec![
      strict_role(self.user(CAROL), StrictRoleKind::Admin),
      strict_role(self.user(DAVE), StrictRoleKind::Moderator),
    ]
  }

  fn roles_change(&self, signer: u64, roles: Vec<StrictRole>) -> SignedStrictRolesChange {
    let maybe_current = self.groups.values().next().unwrap().strict_roles().unwrap();
    let (version, prev_hash) = match &maybe_current {
      Some(current) => (current.version() + 1, current.hash().unwrap()),
      None => (1, StrictHash::ZERO),
    };
    let signer_role = if signer == ALICE {
      StrictSignerRole::Workspace
    } else {
      StrictSignerRole::ChannelAdmin
    };
    let user = self.user(signer);
    StrictRolesChange {
      conversation_id: CONVERSATION,
      version,
      prev_hash,
      signed_for_epoch: self.epoch(),
      signer_role,
      roster_version: u64::from(signer == ALICE),
      role_log_version: version,
      role_log_hash: StrictHash::ZERO,
      roles,
    }
    .sign(&user.device, &user.certificate)
    .unwrap()
  }

  fn turn_on(&mut self) {
    let signed = self.roles_change(ALICE, self.roles());
    let updates = self.commit_roles(ALICE, &signed).unwrap();
    for update in updates {
      assert_eq!(update.maybe_strict_roles_change.as_deref(), Some(&signed));
    }
  }

  /// `committer` commits the roles change; every other member merges it. Returns their updates.
  fn commit_roles(
    &mut self,
    committer: u64,
    signed: &SignedStrictRolesChange,
  ) -> Result<Vec<MembershipUpdate>, MlsError> {
    let client = &self.clients[&committer];
    let group = self.groups.get_mut(&committer).unwrap();
    let output = group.commit_strict_roles(
      &client.provider,
      &client.user.device,
      signed,
      &self.directory,
      &self.authority,
    )?;
    group.confirm_commit(&client.provider).unwrap();
    Ok(self.deliver_ok(committer, &output))
  }

  /// `committer` commits a signed change; on success every other member merges it and a new
  /// user joins with the welcome.
  fn commit_change(
    &mut self,
    committer: u64,
    change: &SignedMembershipChange,
    key_packages: &[Vec<u8>],
  ) -> Result<Vec<MembershipUpdate>, MlsError> {
    let client = &self.clients[&committer];
    let group = self.groups.get_mut(&committer).unwrap();
    let output = group.commit_membership_change(
      &client.provider,
      &client.user.device,
      change,
      key_packages,
      &self.directory,
      &self.authority,
    )?;
    group.confirm_commit(&client.provider).unwrap();
    Ok(self.deliver_ok(committer, &output))
  }

  /// Every member but the committer merges the commit; a new member joins.
  fn deliver_ok(&mut self, committer: u64, output: &CommitOutput) -> Vec<MembershipUpdate> {
    let mut updates = Vec::new();
    let mut gone = Vec::new();
    for (id, group) in &mut self.groups {
      if *id == committer {
        continue;
      }
      let client = &self.clients[id];
      let event = group
        .process_message(&client.provider, &output.commit, &self.directory, &self.authority)
        .unwrap_or_else(|error| panic!("user {id} refused an honest commit: {error}"));
      let GroupEvent::MembershipChanged(update) = event else {
        panic!("expected a commit")
      };
      if update.is_self_removed {
        gone.push(*id);
      }
      updates.push(update);
    }
    for id in gone {
      self.groups.remove(&id);
    }
    if let Some(welcome) = &output.maybe_welcome {
      for (id, client) in &self.clients {
        if self.groups.contains_key(id) {
          continue;
        }
        if let Ok(group) = ConversationGroup::join(&client.provider, welcome, CONVERSATION, &self.directory) {
          self.groups.insert(*id, group);
        }
      }
    }
    updates
  }

  /// `committer` builds a commit with arbitrary adds, removals and authenticated data, bypassing
  /// TeamCraft's checks, as a modified client could. Its own group stays where it was.
  fn raw_commit(
    &mut self,
    committer: u64,
    key_packages: &[Vec<u8>],
    removed: &[u64],
    aad: Vec<u8>,
  ) -> Vec<u8> {
    let output = self.stage_raw(committer, key_packages, removed, aad);
    let client = &self.clients[&committer];
    let group = self.groups.get_mut(&committer).unwrap();
    group.discard_commit(&client.provider).unwrap();
    output.commit
  }

  fn stage_raw(
    &mut self,
    committer: u64,
    key_packages: &[Vec<u8>],
    removed: &[u64],
    aad: Vec<u8>,
  ) -> CommitOutput {
    let client = &self.clients[&committer];
    let group = self.groups.get_mut(&committer).unwrap();
    let key_packages = key_packages
      .iter()
      .map(|bytes| validated_package(&client.provider, bytes))
      .collect();
    let removed = group
      .group
      .members()
      .filter_map(|leaf| {
        let member = credential::member_of(&leaf.credential).unwrap();
        removed
          .contains(&member.user_id.get())
          .then_some((leaf.index, member))
      })
      .collect();
    group.group.set_aad(aad);
    let output = group.stage_commit(
      &client.provider,
      &client.user.device,
      key_packages,
      removed,
      false,
    );
    group.group.set_aad(Vec::new());
    output.unwrap()
  }

  /// Every member but the committer refuses the commit and stays in its epoch with its members
  /// and roles.
  fn expect_refused_by_all(
    &mut self,
    committer: u64,
    commit: &[u8],
    is_expected: impl Fn(&MlsError) -> bool,
  ) {
    for (id, group) in &mut self.groups {
      if *id == committer {
        continue;
      }
      let client = &self.clients[id];
      let (epoch, members, roles) = (
        group.epoch(),
        group.members().unwrap(),
        group.strict_roles().unwrap(),
      );
      let error = group
        .process_message(&client.provider, commit, &self.directory, &self.authority)
        .expect_err("the commit must be refused");
      assert!(is_expected(&error), "user {id}: unexpected error: {error}");
      assert_eq!(group.epoch(), epoch);
      assert_eq!(group.members().unwrap(), members);
      assert_eq!(group.strict_roles().unwrap(), roles);
    }
  }

  /// `committer` would commit the change: refused before anything is sent.
  fn expect_committer_refuses(
    &mut self,
    committer: u64,
    change: &SignedMembershipChange,
    key_packages: &[Vec<u8>],
  ) {
    let epoch = self.group(committer).epoch();
    let error = self
      .commit_change(committer, change, key_packages)
      .expect_err("an honest committer must refuse it");
    assert!(
      matches!(error, MlsError::StrictRefused { .. }),
      "unexpected error: {error}"
    );
    assert!(!self.group(committer).has_pending_commit());
    assert_eq!(self.group(committer).epoch(), epoch);
  }

  /// A change honest committers refuse, committed by Bob's modified client: refused by all.
  fn expect_forged_change_refused(
    &mut self,
    change: &SignedMembershipChange,
    key_packages: &[Vec<u8>],
    removed: &[u64],
  ) {
    let commit = self.raw_commit(BOB, key_packages, removed, change.to_bytes().unwrap());
    self.expect_refused_by_all(BOB, &commit, |error| {
      matches!(error, MlsError::StrictRefused { .. })
    });
  }

  fn eve_key_package(&self) -> Vec<u8> {
    self.clients[&EVE].key_package()
  }
}

impl Client {
  /// A key package of an app that announces the anchor but not strict channels.
  fn key_package_without_strict(&self) -> Vec<u8> {
    let capabilities = Capabilities::new(
      None,
      None,
      Some(&[ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE)]),
      None,
      None,
    );
    let builder = KeyPackage::builder()
      .leaf_node_capabilities(capabilities)
      .key_package_lifetime(crate::mls::lifetime_from(now_secs()));
    crate::mls::build_key_package(&self.provider, &self.user.device, &self.user.certificate, builder).unwrap()
  }
}

fn strict_role(user: &TestUser, kind: StrictRoleKind) -> StrictRole {
  StrictRole {
    user_id: user.user_id,
    role: kind,
    identity_key: user.identity.public_key(),
  }
}

fn validated_package(provider: &DefaultMlsProvider, bytes: &[u8]) -> KeyPackage {
  KeyPackageIn::tls_deserialize_exact(bytes)
    .unwrap()
    .validate(provider.crypto(), ProtocolVersion::Mls10)
    .unwrap()
}

fn remove(user_id: u64) -> MembershipAction {
  MembershipAction::RemoveUser {
    user_id: UserId::new(user_id),
  }
}

fn roles_aad(signed: &SignedStrictRolesChange) -> Vec<u8> {
  let mut aad = STRICT_ROLES_AAD_MAGIC.to_vec();
  aad.extend_from_slice(&signed.to_bytes().unwrap());
  aad
}

mod turning_on {
  use super::*;

  #[test]
  fn every_member_holds_the_same_roles() {
    let world = StrictWorld::new();
    let roles = world.group(ALICE).strict_roles().unwrap().unwrap();
    assert_eq!(roles.roles(), world.roles().as_slice());
    for group in world.groups.values() {
      assert!(group.is_strict());
      assert_eq!(group.strict_roles().unwrap().as_ref(), Some(&roles));
      let required = group.group.extensions().required_capabilities().unwrap();
      assert!(
        required
          .extension_types()
          .contains(&ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE))
      );
    }
  }

  /// D3: only workspace signers; a channel admin of the role log can't (trust on first use).
  #[test]
  fn a_member_or_channel_admin_cannot_turn_it_on() {
    let mut world = StrictWorld::unstrict(&[]);
    let by_carol = world.roles_change(CAROL, world.roles());
    assert!(matches!(
      world.commit_roles(CAROL, &by_carol),
      Err(MlsError::StrictRoles(StrictRolesError::NotAllowed(_)))
    ));
    // Carol's modified client commits it anyway.
    let extensions = with_strict(
      world.group(CAROL).group.extensions(),
      &StrictRolesVerdict::Roles(roles_from(&world, &by_carol)),
    )
    .unwrap();
    let commit = raw_context_commit(&mut world, CAROL, extensions, roles_aad(&by_carol));
    world.expect_refused_by_all(CAROL, &commit, |error| {
      matches!(error, MlsError::StrictRoles(StrictRolesError::NotAllowed(_)))
    });
  }

  /// Step 3 and D6: while a leaf doesn't announce the capability, it can't be turned on; once
  /// that device refreshed its leaf (an empty commit every member takes), it can.
  #[test]
  fn an_old_leaf_blocks_it_until_refreshed() {
    let mut world = StrictWorld::unstrict(&[BOB]);
    let bob = GroupMember {
      user_id: UserId::new(BOB),
      device_id: world.user(BOB).device.device_id(),
    };
    assert_eq!(world.group(ALICE).leaves_without_strict().unwrap(), vec![bob]);
    let signed = world.roles_change(ALICE, world.roles());
    let error = world.commit_roles(ALICE, &signed).unwrap_err();
    assert!(
      matches!(error, MlsError::Openmls { .. }),
      "unexpected error: {error}"
    );
    assert!(!world.group(ALICE).is_strict());
    world
      .groups
      .get_mut(&ALICE)
      .unwrap()
      .discard_commit(&world.clients[&ALICE].provider)
      .unwrap();

    let client = &world.clients[&BOB];
    let group = world.groups.get_mut(&BOB).unwrap();
    let output = group
      .commit_leaf_refresh(&client.provider, &client.user.device)
      .unwrap();
    group.confirm_commit(&client.provider).unwrap();
    let updates = world.deliver_ok(BOB, &output);
    assert!(
      updates
        .iter()
        .all(|update| update.added.is_empty() && update.removed.is_empty())
    );
    assert!(world.group(ALICE).leaves_without_strict().unwrap().is_empty());

    world.turn_on();
    assert!(world.groups.values().all(ConversationGroup::is_strict));
  }

  /// An anchored channel keeps its anchor byte for byte and requires both.
  #[test]
  fn an_anchored_channel_keeps_its_anchor() {
    let alice = Client::new(ALICE);
    let anchor = anchor_of(&alice, AnchorKind::Channel);
    let mut group = ConversationGroup::create_anchored(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
      &anchor,
    )
    .unwrap();
    let directory = TestDirectory::with(&[&alice.user]);
    let authority = TestAuthority::allowing(&[alice.user.user_id]);
    let signed = alone_turn_on(&alice);
    group
      .commit_strict_roles(
        &alice.provider,
        &alice.user.device,
        &signed,
        &directory,
        &authority,
      )
      .unwrap();
    group.confirm_commit(&alice.provider).unwrap();
    assert!(group.is_strict());
    assert_eq!(group.anchor_bytes(), Some(anchor.to_bytes().unwrap().as_slice()));
    let required = group.group.extensions().required_capabilities().unwrap();
    assert_eq!(
      required.extension_types(),
      &[
        ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE),
        ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE)
      ]
    );
  }

  /// Only channels: a meeting chat never becomes strict.
  #[test]
  fn a_meeting_chat_cannot_turn_strict() {
    let alice = Client::new(ALICE);
    let anchor = anchor_of(
      &alice,
      AnchorKind::MeetingChat {
        meeting_id: MeetingId::new(9),
      },
    );
    let mut group = ConversationGroup::create_anchored(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
      &anchor,
    )
    .unwrap();
    let directory = TestDirectory::with(&[&alice.user]);
    let authority = TestAuthority::allowing(&[alice.user.user_id]);
    assert!(matches!(
      group.commit_strict_roles(
        &alice.provider,
        &alice.user.device,
        &alone_turn_on(&alice),
        &directory,
        &authority
      ),
      Err(MlsError::StrictRoles(StrictRolesError::NotAChannel))
    ));
  }
}

mod honest {
  use super::*;

  /// A member commits on behalf of the admin (admin.md §2.2): who signed matters, not who
  /// committed.
  #[test]
  fn a_channel_admin_adds_and_removes_through_any_member() {
    let mut world = StrictWorld::new();
    let add_eve = world.signed(CAROL, add(EVE, HistoryShare::FromJoin));
    let key_package = world.eve_key_package();
    world.commit_change(BOB, &add_eve, &[key_package]).unwrap();
    assert!(world.groups.contains_key(&EVE));
    assert!(world.group(EVE).is_strict());
    let remove_bob = world.signed(CAROL, remove(BOB));
    world.commit_change(DAVE, &remove_bob, &[]).unwrap();
    assert!(!world.groups.contains_key(&BOB));
  }

  /// D1: a moderator removes a plain member.
  #[test]
  fn a_moderator_removes_a_plain_member() {
    let mut world = StrictWorld::new();
    let change = world.signed(DAVE, remove(BOB));
    world.commit_change(CAROL, &change, &[]).unwrap();
    assert!(!world.groups.contains_key(&BOB));
  }

  /// Leaving is always allowed, a plain member's and the last admin's.
  #[test]
  fn anybody_leaves() {
    let mut world = StrictWorld::new();
    let bob_leaves = world.signed(BOB, remove(BOB));
    world.commit_change(ALICE, &bob_leaves, &[]).unwrap();
    let carol_leaves = world.signed(CAROL, remove(CAROL));
    world.commit_change(DAVE, &carol_leaves, &[]).unwrap();
    assert!(!world.groups.contains_key(&CAROL));
  }

  /// Workspace signers keep §4.6, from inside the group or out.
  #[test]
  fn a_workspace_admin_adds_and_removes() {
    let mut world = StrictWorld::new();
    let add_eve = world.signed(ALICE, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    world.commit_change(BOB, &add_eve, &[key_package]).unwrap();
    let remove_carol = world.signed(ALICE, remove(CAROL));
    world.commit_change(BOB, &remove_carol, &[]).unwrap();
    assert!(!world.groups.contains_key(&CAROL));
  }

  /// The server's word about guests is out: a channel admin the server calls a guest still acts.
  #[test]
  fn the_servers_guest_flag_decides_nothing() {
    let mut world = StrictWorld::new();
    let add_eve = world.signed(CAROL, add(EVE, HistoryShare::FromJoin));
    let key_package = world.eve_key_package();
    let client = &world.clients[&CAROL];
    let guest = GuestFlag {
      inner: &world.directory,
      guest: UserId::new(CAROL),
    };
    let group = world.groups.get_mut(&CAROL).unwrap();
    let output = group
      .commit_membership_change(
        &client.provider,
        &client.user.device,
        &add_eve,
        &[key_package],
        &guest,
        &world.authority,
      )
      .unwrap();
    for id in [ALICE, BOB, DAVE] {
      let client = &world.clients[&id];
      let guest = GuestFlag {
        inner: &world.directory,
        guest: UserId::new(CAROL),
      };
      world
        .groups
        .get_mut(&id)
        .unwrap()
        .process_message(&client.provider, &output.commit, &guest, &world.authority)
        .unwrap();
    }
  }

  #[test]
  fn an_admin_changes_the_roles_and_turns_strict_off() {
    let mut world = StrictWorld::new();
    let mut roles = world.roles();
    roles.insert(0, strict_role(world.user(BOB), StrictRoleKind::Moderator));
    let promote_bob = world.roles_change(CAROL, roles.clone());
    world.commit_roles(DAVE, &promote_bob).unwrap();
    for group in world.groups.values() {
      assert_eq!(group.strict_roles().unwrap().unwrap().roles(), roles.as_slice());
    }
    // Bob, now a moderator, removes no admin but a plain member: none left but Alice.
    let bob_removes_alice = world.signed(BOB, remove(ALICE));
    world.commit_change(CAROL, &bob_removes_alice, &[]).unwrap();

    let off = world.roles_change(CAROL, Vec::new());
    world.commit_roles(BOB, &off).unwrap();
    for group in world.groups.values() {
      assert!(!group.is_strict());
      assert!(group.strict_roles().unwrap().is_none());
      let strict_type = ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE);
      assert!(
        group
          .group
          .extensions()
          .required_capabilities()
          .is_none_or(|required| !required.extension_types().contains(&strict_type))
      );
    }
  }

  /// Further devices of a member and a leaf refresh change nobody's membership: still allowed.
  #[test]
  fn a_members_further_device_and_a_leaf_refresh_pass() {
    let mut world = StrictWorld::new();
    let bob_phone = Client::from_user(world.user(BOB).new_device(201));
    let client = &world.clients[&CAROL];
    let group = world.groups.get_mut(&CAROL).unwrap();
    let output = group
      .commit_device_additions(
        &client.provider,
        &client.user.device,
        &[bob_phone.key_package()],
        &world.directory,
      )
      .unwrap();
    group.confirm_commit(&client.provider).unwrap();
    world.deliver_ok(CAROL, &output);
    let client = &world.clients[&DAVE];
    let group = world.groups.get_mut(&DAVE).unwrap();
    let output = group
      .commit_leaf_refresh(&client.provider, &client.user.device)
      .unwrap();
    group.confirm_commit(&client.provider).unwrap();
    world.deliver_ok(DAVE, &output);
  }
}

mod refused {
  use super::*;

  /// The attack the issue opens with: a member with a modified client signs an addition. Honest
  /// committers refuse it, and so does every member when the modified client commits it itself.
  #[test]
  fn a_member_signing_an_addition() {
    let mut world = StrictWorld::new();
    let change = world.signed(BOB, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    world.expect_committer_refuses(CAROL, &change, std::slice::from_ref(&key_package));
    world.expect_forged_change_refused(&change, &[key_package], &[]);
  }

  #[test]
  fn a_member_signing_a_removal() {
    let mut world = StrictWorld::new();
    let change = world.signed(BOB, remove(DAVE));
    world.expect_committer_refuses(CAROL, &change, &[]);
    world.expect_forged_change_refused(&change, &[], &[DAVE]);
  }

  /// D1: a moderator adds nobody, removes no moderator or admin and nobody's single device.
  #[test]
  fn a_moderator_beyond_plain_members() {
    let mut world = StrictWorld::new();
    let key_package = world.eve_key_package();
    let adds = world.signed(DAVE, add(EVE, HistoryShare::Full));
    world.expect_committer_refuses(CAROL, &adds, std::slice::from_ref(&key_package));
    world.expect_forged_change_refused(&adds, &[key_package], &[]);
    let removes_admin = world.signed(DAVE, remove(CAROL));
    world.expect_committer_refuses(ALICE, &removes_admin, &[]);
    world.expect_forged_change_refused(&removes_admin, &[], &[CAROL]);
    let removes_device = world.signed(
      DAVE,
      MembershipAction::RemoveDevice {
        user_id: UserId::new(ALICE),
        device_id: world.user(ALICE).device.device_id(),
      },
    );
    world.expect_committer_refuses(CAROL, &removes_device, &[]);
    world.expect_forged_change_refused(&removes_device, &[], &[ALICE]);
  }

  /// D2: a space admin or a custom scheme's holder may manage members by the server's roles,
  /// which aren't signed: in a strict channel that grants nothing. In a channel that isn't strict
  /// the same change passes.
  #[test]
  fn a_space_admin_or_scheme_holder() {
    let mut world = StrictWorld::new();
    world.authority = TestAuthority::allowing(&[UserId::new(ALICE)]).with_schemes(&[UserId::new(BOB)]);
    let change = world.signed(BOB, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    world.expect_committer_refuses(CAROL, &change, std::slice::from_ref(&key_package));
    world.expect_forged_change_refused(&change, &[key_package], &[]);

    let mut plain = StrictWorld::unstrict(&[]);
    plain.authority = TestAuthority::allowing(&[UserId::new(ALICE)]).with_schemes(&[UserId::new(BOB)]);
    let change = plain.signed(BOB, add(EVE, HistoryShare::Full));
    let key_package = plain.eve_key_package();
    plain.commit_change(CAROL, &change, &[key_package]).unwrap();
  }

  /// "A role counts only with a leaf": the admin's other device, never added, signs.
  #[test]
  fn an_admins_device_without_a_leaf() {
    let mut world = StrictWorld::new();
    let carol_phone = world.user(CAROL).new_device(301);
    let change = world.signed_by(&carol_phone, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    world.expect_committer_refuses(DAVE, &change, std::slice::from_ref(&key_package));
    world.expect_forged_change_refused(&change, &[key_package], &[]);
  }

  /// A device the server certified for the admin under a key of its own isn't the admin.
  #[test]
  fn a_device_of_another_identity_claiming_the_admin() {
    let mut world = StrictWorld::new();
    let impostor = TestUser::new(CAROL);
    let change = world.signed_by(&impostor, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    let error = world
      .commit_change(DAVE, &change, std::slice::from_ref(&key_package))
      .unwrap_err();
    assert!(
      matches!(error, MlsError::Membership(_)),
      "unexpected error: {error}"
    );
    let commit = world.raw_commit(BOB, &[key_package], &[], change.to_bytes().unwrap());
    world.expect_refused_by_all(BOB, &commit, |error| matches!(error, MlsError::Membership(_)));
  }

  /// A workspace signer the device can't confirm yet: held back to read the roster, not refused.
  #[test]
  fn a_workspace_signer_with_a_roster_not_read_yet() {
    let mut world = StrictWorld::new();
    let change = world.signed(ALICE, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    let commit = world.raw_commit(BOB, &[key_package], &[], change.to_bytes().unwrap());
    let client = &world.clients[&CAROL];
    let group = world.groups.get_mut(&CAROL).unwrap();
    let error = group
      .process_message(&client.provider, &commit, &world.directory, &UnreadRoster)
      .unwrap_err();
    assert!(
      matches!(error, MlsError::StrictRosterNeeded(_)),
      "unexpected error: {error}"
    );
    assert!(error.is_strict_retry());
    // With the roster read, it passes (on another member: openmls spends the epoch's secret on
    // the first try, so the app retries the entry from the queue on a reloaded group).
    let client = &world.clients[&DAVE];
    world
      .groups
      .get_mut(&DAVE)
      .unwrap()
      .process_message(&client.provider, &commit, &world.directory, &world.authority)
      .unwrap();
  }

  /// Unsigned removals and additions of new users stay refused as in §4.6.
  #[test]
  fn unsigned_changes() {
    let mut world = StrictWorld::new();
    let key_package = world.eve_key_package();
    let adds = world.raw_commit(CAROL, &[key_package], &[], Vec::new());
    world.expect_refused_by_all(CAROL, &adds, |error| {
      matches!(error, MlsError::MissingSignedChange)
    });
    let removes = world.raw_commit(CAROL, &[], &[BOB], Vec::new());
    world.expect_refused_by_all(CAROL, &removes, |error| {
      matches!(error, MlsError::MissingSignedChange)
    });
  }

  /// An old app can't be added to a strict channel; the adder is told before anything is sent.
  #[test]
  fn adding_an_old_app() {
    let mut world = StrictWorld::new();
    let change = world.signed(CAROL, add(EVE, HistoryShare::Full));
    let old = world.clients[&EVE].key_package_without_strict();
    assert!(matches!(
      world.commit_change(CAROL, &change, &[old]),
      Err(MlsError::MustUpdate(_))
    ));
  }
}

mod role_changes {
  use super::*;

  #[test]
  fn a_moderator_or_member_changing_the_roles() {
    let mut world = StrictWorld::new();
    for signer in [DAVE, BOB] {
      let signed = world.roles_change(signer, Vec::new());
      assert!(matches!(
        world.commit_roles(CAROL, &signed),
        Err(MlsError::StrictRoles(StrictRolesError::NotAllowed(_)))
      ));
      world
        .groups
        .get_mut(&CAROL)
        .unwrap()
        .discard_commit(&world.clients[&CAROL].provider)
        .unwrap();
    }
  }

  /// The commit's context must be exactly what the signed change says: other roles, a dropped
  /// extension are refused.
  #[test]
  fn a_context_beyond_the_signed_change() {
    let mut world = StrictWorld::new();
    let mut roles = world.roles();
    roles.insert(0, strict_role(world.user(BOB), StrictRoleKind::Admin));
    let signed = world.roles_change(CAROL, roles);
    let other_roles = world.roles_change(CAROL, vec![strict_role(world.user(CAROL), StrictRoleKind::Admin)]);
    let other = with_strict(
      world.group(CAROL).group.extensions(),
      &StrictRolesVerdict::Roles(roles_from(&world, &other_roles)),
    )
    .unwrap();
    let commit = raw_context_commit(&mut world, CAROL, other, roles_aad(&signed));
    world.expect_refused_by_all(CAROL, &commit, |error| {
      matches!(error, MlsError::ContextChangeMismatch)
    });

    // The roles without their requirement don't get past openmls itself
    // (`ExtensionNotInRequiredCapabilities`), so that case needs no check of ours.

    // Dropping the roles without the signed "off": refused as today's clients refuse any context
    // change without a signed one.
    let commit = raw_context_commit(&mut world, CAROL, Extensions::empty(), Vec::new());
    world.expect_refused_by_all(CAROL, &commit, |error| {
      matches!(error, MlsError::UnsupportedProposal)
    });
  }

  /// A roles commit does nothing else: no addition rides along.
  #[test]
  fn a_roles_commit_that_also_adds() {
    let mut world = StrictWorld::new();
    let signed = world.roles_change(CAROL, world.roles());
    let extensions = with_strict(
      world.group(CAROL).group.extensions(),
      &StrictRolesVerdict::Roles(roles_from(&world, &signed)),
    )
    .unwrap();
    let key_package = world.eve_key_package();
    let commit = raw_context_commit_adding(&mut world, CAROL, extensions, roles_aad(&signed), &[key_package]);
    world.expect_refused_by_all(CAROL, &commit, |error| {
      matches!(error, MlsError::UnsupportedProposal)
    });
  }

  /// A demotion races the demoted admin's change: their order decides, and the later one is
  /// refused (built on the old roles, or signed by somebody no longer admin).
  #[test]
  fn a_change_by_an_admin_demoted_before_it() {
    let mut world = StrictWorld::new();
    let mut roles = world.roles();
    roles.insert(0, strict_role(world.user(BOB), StrictRoleKind::Admin));
    let promote = world.roles_change(ALICE, roles);
    world.commit_roles(ALICE, &promote).unwrap();
    let bob_adds = world.signed(BOB, add(EVE, HistoryShare::Full));
    let demote = world.roles_change(CAROL, world.roles());
    world.commit_roles(CAROL, &demote).unwrap();
    let key_package = world.eve_key_package();
    world.expect_committer_refuses(DAVE, &bob_adds, std::slice::from_ref(&key_package));
  }
}

/// Finding 2 of §4.6.2: a device that refused a commit must not be brought back by the one who
/// made it, with the refused change in its tree.
mod falling_out {
  use super::*;

  #[test]
  fn the_forger_cannot_bring_a_refusing_device_back() {
    let mut world = StrictWorld::new();
    let known = world.group(DAVE).strict_roles().unwrap().unwrap();
    // Bob's modified client signs and commits Eve's addition and follows it himself.
    let change = world.signed(BOB, add(EVE, HistoryShare::Full));
    let key_package = world.eve_key_package();
    let output = world.stage_raw(BOB, &[key_package], &[], change.to_bytes().unwrap());
    world
      .groups
      .get_mut(&BOB)
      .unwrap()
      .confirm_commit(&world.clients[&BOB].provider)
      .unwrap();
    world.expect_refused_by_all(BOB, &output.commit, |error| {
      matches!(error, MlsError::StrictRefused { .. })
    });

    // Dave asks to be added back; Bob answers from his side of the fork.
    let dave_package = world.clients[&DAVE].key_package();
    let welcome = world
      .stage_raw(BOB, &[dave_package], &[DAVE], Vec::new())
      .maybe_welcome
      .unwrap();
    let staged =
      ConversationGroup::stage_join_replacing(&world.clients[&DAVE].provider, &welcome, CONVERSATION)
        .unwrap();
    assert!(matches!(
      staged.check_strict_sender(&known, &world.directory, &world.authority),
      Err(MlsError::StrictWelcomeRefused(user)) if user == UserId::new(BOB)
    ));
  }

  #[test]
  fn an_admin_or_workspace_admin_brings_it_back() {
    let mut world = StrictWorld::new();
    let known = world.group(DAVE).strict_roles().unwrap().unwrap();
    for sender in [CAROL, ALICE] {
      let dave_package = world.clients[&DAVE].key_package();
      let output = world.stage_raw(sender, &[dave_package], &[DAVE], Vec::new());
      world
        .groups
        .get_mut(&sender)
        .unwrap()
        .discard_commit(&world.clients[&sender].provider)
        .unwrap();
      let staged = ConversationGroup::stage_join_replacing(
        &world.clients[&DAVE].provider,
        &output.maybe_welcome.unwrap(),
        CONVERSATION,
      )
      .unwrap();
      assert_eq!(
        staged
          .check_strict_sender(&known, &world.directory, &world.authority)
          .unwrap()
          .user_id,
        UserId::new(sender)
      );
      assert_eq!(staged.strict_roles().unwrap().as_ref(), Some(&known));
    }
  }

  /// Finding 4: a commit from a group a member founded alone isn't an authorization verdict but
  /// an entry the device can't read at its own epoch; it is told apart from a refusal.
  #[test]
  fn a_commit_from_a_group_founded_alone_is_unreadable_not_refused() {
    let mut world = StrictWorld::new();
    let bob_again = Client::from_user(world.user(BOB).new_device(202));
    let (_, output) = ConversationGroup::refound(
      &bob_again.provider,
      &bob_again.user.device,
      &bob_again.user.certificate,
      CONVERSATION,
      world.epoch(),
    )
    .unwrap();
    world.expect_refused_by_all(BOB, &output.commit, |error| {
      matches!(error, MlsError::Openmls { .. }) && !error.is_strict_retry()
    });
  }
}

/// A directory that calls one user a meeting guest (the server's word).
struct GuestFlag<'a> {
  inner: &'a TestDirectory,
  guest: UserId,
}

impl IdentityDirectory for GuestFlag<'_> {
  fn pinned_identity(&self, user_id: UserId) -> Option<crate::identity::IdentityPublicKey> {
    self.inner.pinned_identity(user_id)
  }

  fn is_listed_device(&self, device: &VerifiedDevice) -> bool {
    self.inner.is_listed_device(device)
  }

  fn is_guest(&self, user_id: UserId) -> bool {
    user_id == self.guest
  }
}

/// A roster this device hasn't read yet.
struct UnreadRoster;

impl MembershipAuthority for UnreadRoster {
  fn is_authorized(&self, _: UserId, _: ConversationId, _: &MembershipAction) -> bool {
    true
  }

  fn workspace_signer(&self, _: UserId, _: u64, _: Judged) -> Standing {
    Standing::Unknown
  }
}

fn roles_from(world: &StrictWorld, signed: &SignedStrictRolesChange) -> StrictRoles {
  let _ = world;
  StrictRoles::for_tests(signed.clone())
}

fn raw_context_commit(
  world: &mut StrictWorld,
  committer: u64,
  extensions: Extensions<GroupContext>,
  aad: Vec<u8>,
) -> Vec<u8> {
  raw_context_commit_adding(world, committer, extensions, aad, &[])
}

fn raw_context_commit_adding(
  world: &mut StrictWorld,
  committer: u64,
  extensions: Extensions<GroupContext>,
  aad: Vec<u8>,
  key_packages: &[Vec<u8>],
) -> Vec<u8> {
  let client = &world.clients[&committer];
  let provider = &client.provider;
  let key_packages: Vec<KeyPackage> = key_packages
    .iter()
    .map(|bytes| validated_package(provider, bytes))
    .collect();
  let group = world.groups.get_mut(&committer).unwrap();
  group.group.set_aad(aad);
  let bundle = group
    .group
    .commit_builder()
    .propose_adds(key_packages)
    .propose_group_context_extensions(extensions)
    .unwrap()
    .load_psks(provider.storage())
    .unwrap()
    .build(provider.rand(), provider.crypto(), &client.user.device, |_| true)
    .unwrap()
    .stage_commit(provider)
    .unwrap();
  group.group.set_aad(Vec::new());
  let commit = bundle.into_commit().to_bytes().unwrap();
  group.discard_commit(provider).unwrap();
  commit
}

fn anchor_of(client: &Client, kind: AnchorKind) -> ChannelAnchor {
  ChannelAnchor::found(
    &client.user.device,
    &client.user.certificate,
    &Founding {
      conversation_id: CONVERSATION,
      kind,
      device_list_version: 1,
      signed_at_ms: 1_700_000_000_000,
      maybe_space_id: None,
      name: "general",
    },
  )
  .unwrap()
}

/// Alice, alone in her group, turns it strict with herself as admin.
fn alone_turn_on(alice: &Client) -> SignedStrictRolesChange {
  StrictRolesChange {
    conversation_id: CONVERSATION,
    version: 1,
    prev_hash: StrictHash::ZERO,
    signed_for_epoch: 0,
    signer_role: StrictSignerRole::Workspace,
    roster_version: 1,
    role_log_version: 1,
    role_log_hash: StrictHash::ZERO,
    roles: vec![strict_role(&alice.user, StrictRoleKind::Admin)],
  }
  .sign(&alice.user.device, &alice.user.certificate)
  .unwrap()
}
