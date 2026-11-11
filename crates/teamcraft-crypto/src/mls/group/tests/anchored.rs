//! Anchored groups (e2ee.md §4.6.5, issue #1725): founding with the anchor, moves and renames as
//! group context extensions commits, and who can be in such a group.

use openmls::prelude::{Capabilities, KeyPackage, LeafNodeParameters, MlsGroup, MlsGroupJoinConfig};

use super::*;
use crate::channel_anchor::{
  AnchorExpectation, AnchorKind, ChannelAnchor, ExpectedKind, Founding, NameAuthority, NameSetter,
  PropertiesChangeError, PropertiesTarget, SignedPropertiesChange,
};
use crate::ids::SpaceId;

const SPACE: SpaceId = SpaceId::new(5);
const OTHER_SPACE: SpaceId = SpaceId::new(6);

mod founding {
  use super::*;

  #[test]
  fn a_welcome_carries_the_anchor_which_verifies_before_the_join() {
    let world = AnchoredWorld::new();
    let alice_anchor = world.alice_group.anchor_bytes().unwrap().to_vec();
    assert_eq!(world.carol_group.anchor_bytes(), Some(alice_anchor.as_slice()));
    let verified = world
      .carol_group
      .verified_anchor(&expectation(), &world.directory, &world.authority)
      .unwrap()
      .unwrap();
    assert_eq!(verified.founder().map(|(user, _)| user), Some(UserId::new(ALICE)));
    assert_eq!(verified.properties().maybe_space_id, Some(SPACE));
  }

  #[test]
  fn the_staged_welcome_names_its_anchor_and_its_conversation_is_checked() {
    let mut world = AnchoredWorld::new();
    let output = world.add_bob();
    let staged = ConversationGroup::stage_join(
      &world.bob.provider,
      output.maybe_welcome.as_ref().unwrap(),
      CONVERSATION,
    )
    .unwrap();
    assert!(
      staged
        .verified_anchor(&expectation(), &world.directory, &world.authority)
        .unwrap()
        .is_some()
    );
    let mut other = expectation();
    other.conversation_id = ConversationId::new(43);
    assert!(matches!(
      staged.verified_anchor(&other, &world.directory, &world.authority),
      Err(MlsError::Anchor(_))
    ));
  }

  #[test]
  fn an_anchor_for_another_conversation_founds_nothing() {
    let world = AnchoredWorld::new();
    let provider = DefaultMlsProvider::default();
    let result = ConversationGroup::create_anchored(
      &provider,
      &world.alice.user.device,
      &world.alice.user.certificate,
      ConversationId::new(43),
      &world.anchor(),
    );
    assert!(matches!(result, Err(MlsError::WrongConversation { .. })));
  }

  #[test]
  fn a_group_without_an_anchor_has_none_to_verify_or_change() {
    let mut world = World::new();
    assert!(world.alice_group.anchor_bytes().is_none());
    assert!(
      world
        .alice_group
        .verified_anchor(&expectation(), &world.directory, &world.authority)
        .unwrap()
        .is_none()
    );
    let anchored = AnchoredWorld::new();
    let signed = anchored.alice_change(None, "x");
    assert!(matches!(
      world.alice_group.commit_properties_change(
        &world.alice.provider,
        &world.alice.user.device,
        &signed,
        &world.directory,
        &world.authority,
      ),
      Err(MlsError::NotAnchored)
    ));
  }
}

mod changes {
  use super::*;

  #[test]
  fn a_members_rename_is_committed_and_every_member_holds_it() {
    let mut world = AnchoredWorld::new();
    let anchor = world.current_anchor();
    let signed = anchor
      .sign_change(
        &world.carol.user.device,
        &world.carol.user.certificate,
        NameAuthority::Member,
        &PropertiesTarget {
          maybe_space_id: Some(SPACE),
          name: "random",
        },
        world.carol_group.epoch(),
        0,
      )
      .unwrap();
    let output = world
      .carol_group
      .commit_properties_change(
        &world.carol.provider,
        &world.carol.user.device,
        &signed,
        &world.directory,
        &world.authority,
      )
      .unwrap();
    let GroupEvent::MembershipChanged(update) = world.alice_processes(&output.commit).unwrap() else {
      panic!("expected a commit")
    };
    assert_eq!(update.maybe_properties_change, Some(Box::new(signed)));
    assert!(update.added.is_empty() && update.removed.is_empty());
    world.carol_group.confirm_commit(&world.carol.provider).unwrap();
    assert_eq!(world.carol_group.anchor_bytes(), world.alice_group.anchor_bytes());
    let verified = world
      .alice_group
      .verified_anchor(&expectation(), &world.directory, &world.authority)
      .unwrap()
      .unwrap();
    assert_eq!(verified.properties().version, 2);
    assert_eq!(
      verified.properties().name_set_by,
      NameSetter {
        authority: NameAuthority::Member,
        user_id: UserId::new(CAROL)
      }
    );
  }

  #[test]
  fn a_workspace_admins_move_keeps_the_founding_and_other_extensions() {
    let mut world = AnchoredWorld::new();
    let before = world.current_anchor();
    let signed = world.alice_change(Some(OTHER_SPACE), "general");
    let output = world.alice_commits_change(&signed);
    world.carol_processes(&output.commit).unwrap();
    let after = world.current_anchor();
    assert_eq!(after.founding(), before.founding());
    assert_eq!(after.founding_hash().unwrap(), before.founding_hash().unwrap());
    assert_eq!(after.properties().maybe_space_id, Some(OTHER_SPACE));
    assert!(
      world
        .carol_group
        .group
        .extensions()
        .required_capabilities()
        .is_some(),
      "the requirement stays"
    );
  }

  #[test]
  fn a_member_cannot_commit_a_move() {
    let mut world = AnchoredWorld::new();
    let signed = world
      .current_anchor()
      .sign_change(
        &world.carol.user.device,
        &world.carol.user.certificate,
        NameAuthority::Member,
        &PropertiesTarget {
          maybe_space_id: Some(OTHER_SPACE),
          name: "general",
        },
        world.carol_group.epoch(),
        0,
      )
      .unwrap();
    let result = world.carol_group.commit_properties_change(
      &world.carol.provider,
      &world.carol.user.device,
      &signed,
      &world.directory,
      &world.authority,
    );
    assert!(matches!(
      result,
      Err(MlsError::PropertiesChange(PropertiesChangeError::NotAllowed(_)))
    ));
  }

  #[test]
  fn a_context_change_beyond_the_signed_properties_is_refused() {
    let mut world = AnchoredWorld::new();
    let signed = world.alice_change(None, "renamed");
    let current = world.current_anchor();
    let aad = properties_aad(&signed);

    // Another founding with the signed change on top.
    let other_founding = world.anchor_named("other").with_change(signed.clone());
    let replaced = world.extensions_with(Some(&other_founding), true);
    let commit = world.raw_context_commit(replaced, aad.clone(), &[]);
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::ContextChangeMismatch));

    // The right anchor, but the requirement dropped: openmls itself refuses an unknown context
    // extension that `required_capabilities` doesn't name, so not even a raw commit gets built.
    let changed = current.with_change(signed.clone());
    let dropped = world.extensions_with(Some(&changed), false);
    assert!(world.try_raw_context_commit(dropped, aad.clone(), &[]).is_err());

    // Properties nobody signed.
    let unsigned = current.with_change(world.alice_change(None, "something else"));
    let commit = world.raw_context_commit(world.extensions_with(Some(&unsigned), true), aad.clone(), &[]);
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::ContextChangeMismatch));

    // The anchor stripped.
    let commit = world.raw_context_commit(world.extensions_with(None, true), aad.clone(), &[]);
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::ContextChangeMismatch));

    // A context change without a signed change.
    let commit = world.raw_context_commit(world.extensions_with(Some(&changed), true), Vec::new(), &[]);
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::UnsupportedProposal));

    // The signed change, plus somebody added on the side.
    let commit = world.raw_context_commit(
      world.extensions_with(Some(&changed), true),
      aad,
      &[world.bob.key_package()],
    );
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::UnsupportedProposal));

    // The honest commit still goes through afterwards.
    let output = world.alice_commits_change(&signed);
    world.carol_processes(&output.commit).unwrap();
  }

  #[test]
  fn a_signed_change_with_a_membership_commit_is_refused() {
    let mut world = AnchoredWorld::new();
    let signed = world.alice_change(None, "renamed");
    let commit = world.raw_add_commit(&[world.bob.key_package()], properties_aad(&signed));
    world.expect_carol_refuses(&commit, |error| matches!(error, MlsError::UnsupportedProposal));
  }
}

/// openmls refuses a leaf that doesn't announce what `required_capabilities` names, on every path
/// (e2ee.md §4.6.5, "Who can be in an anchored group"). Each test builds the path with openmls
/// directly where TeamCraft's own checks would stop it first.
mod required_capabilities {
  use super::*;

  #[test]
  fn an_add_by_value_of_an_old_app_is_refused() {
    let mut world = AnchoredWorld::new();
    let old = world.bob.old_key_package();
    // TeamCraft says so before anything is sent: the person has to update.
    assert!(matches!(
      world
        .alice_group
        .check_key_package(&world.alice.provider, &old, &world.directory),
      Err(MlsError::MustUpdate(_))
    ));
    let change = world.signed_add_bob();
    assert!(matches!(
      world.alice_group.commit_membership_change(
        &world.alice.provider,
        &world.alice.user.device,
        &change,
        std::slice::from_ref(&old),
        &world.directory,
        &world.authority,
      ),
      Err(MlsError::MustUpdate(_))
    ));
    // And openmls refuses the commit itself.
    let key_package = validated(&world.alice.provider, &old);
    let result = world.alice_group.stage_commit(
      &world.alice.provider,
      &world.alice.user.device,
      vec![key_package],
      Vec::new(),
      false,
    );
    assert!(result.is_err(), "openmls committed an add of an old app");
  }

  #[test]
  fn an_add_by_reference_of_an_old_app_is_refused() {
    let mut world = AnchoredWorld::new();
    let key_package = validated(&world.alice.provider, &world.bob.old_key_package());
    let proposed = world.alice_group.group.propose_add_member(
      &world.alice.provider,
      &world.alice.user.device,
      &key_package,
    );
    let committed = proposed.is_ok()
      && world
        .alice_group
        .group
        .commit_to_pending_proposals(&world.alice.provider, &world.alice.user.device)
        .is_ok();
    assert!(!committed, "openmls committed an add by reference of an old app");
  }

  #[test]
  fn a_welcome_into_an_anchored_group_never_reaches_an_old_app() {
    // A group from before anchors, where an old app joins; then somebody tries to require the
    // anchor in the same commit that adds another old app. Either openmls refuses the commit, or
    // the old app refuses the welcome.
    let mut world = World::new();
    let anchored = AnchoredWorld::new();
    let key_package = validated(&world.alice.provider, &world.bob.old_key_package());
    let extensions = anchored.extensions_with(Some(&anchored.current_anchor()), true);
    let provider = &world.alice.provider;
    let built = world
      .alice_group
      .group
      .commit_builder()
      .propose_adds([key_package])
      .propose_group_context_extensions(extensions)
      .map_err(|error| error.to_string())
      .and_then(|builder| {
        builder
          .load_psks(provider.storage())
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| {
        builder
          .build(
            provider.rand(),
            provider.crypto(),
            &world.alice.user.device,
            |_| true,
          )
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| builder.stage_commit(provider).map_err(|error| error.to_string()))
      .ok();
    let Some(bundle) = built else {
      return;
    };
    let welcome = bundle.to_welcome_msg().unwrap().to_bytes().unwrap();
    assert!(
      ConversationGroup::join(&world.bob.provider, &welcome, CONVERSATION, &world.directory).is_err(),
      "an old app opened a welcome into an anchored group"
    );
  }

  #[test]
  fn an_update_proposal_dropping_the_capability_is_refused() {
    let mut world = AnchoredWorld::new();
    let result = world.carol_group.group.propose_self_update(
      &world.carol.provider,
      &world.carol.user.device,
      old_leaf(),
    );
    assert!(result.is_err(), "openmls proposed a leaf without the anchor");
  }

  #[test]
  fn an_update_path_leaf_dropping_the_capability_is_refused() {
    let mut world = AnchoredWorld::new();
    let provider = &world.carol.provider;
    let result = world
      .carol_group
      .group
      .commit_builder()
      .leaf_node_parameters(old_leaf())
      .force_self_update(true)
      .load_psks(provider.storage())
      .map_err(|error| error.to_string())
      .and_then(|builder| {
        builder
          .build(
            provider.rand(),
            provider.crypto(),
            &world.carol.user.device,
            |_| true,
          )
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| builder.stage_commit(provider).map_err(|error| error.to_string()));
    assert!(
      result.is_err(),
      "openmls committed a path leaf without the anchor"
    );
  }

  #[test]
  fn an_external_commit_leaf_without_the_capability_is_refused() {
    let world = AnchoredWorld::new();
    let carol_phone = Client::from_user(world.carol.user.new_device(301));
    let group_info = world
      .alice_group
      .export_group_info(&world.alice.provider, &world.alice.user.device)
      .unwrap();
    let MlsMessageBodyIn::GroupInfo(info) = MlsMessageIn::tls_deserialize_exact(group_info.as_slice())
      .unwrap()
      .extract()
    else {
      panic!("expected a group info")
    };
    let provider = &carol_phone.provider;
    let result = MlsGroup::external_commit_builder()
      .with_config(
        MlsGroupJoinConfig::builder()
          .use_ratchet_tree_extension(true)
          .build(),
      )
      .build_group(
        provider,
        info,
        credential::credential_with_key(&carol_phone.user.certificate).unwrap(),
      )
      .map_err(|error| error.to_string())
      .and_then(|builder| {
        builder
          .leaf_node_parameters(old_leaf())
          .load_psks(provider.storage())
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| {
        builder
          .build(
            provider.rand(),
            provider.crypto(),
            &carol_phone.user.device,
            |_| true,
          )
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| builder.finalize(provider).map_err(|error| error.to_string()));
    assert!(result.is_err(), "openmls joined with a leaf without the anchor");

    // TeamCraft's own external join announces it and gets in.
    let joined = ConversationGroup::join_external(
      &carol_phone.provider,
      &carol_phone.user.device,
      &carol_phone.user.certificate,
      &group_info,
      CONVERSATION,
      0,
      &world.directory,
    );
    assert!(joined.is_ok(), "{joined:?}");
  }

  #[test]
  fn requiring_the_anchor_while_an_old_app_is_in_the_group_is_refused() {
    let mut world = World::new();
    let change = world.signed_change(ALICE, add(BOB, HistoryShare::Full));
    // A group from before anchors takes the old app.
    world.commit_change(&change, &[world.bob.old_key_package()]);
    let anchored = AnchoredWorld::new();
    let extensions = anchored.extensions_with(Some(&anchored.current_anchor()), true);
    let provider = &world.alice.provider;
    let result = world
      .alice_group
      .group
      .commit_builder()
      .propose_group_context_extensions(extensions)
      .map_err(|error| error.to_string())
      .and_then(|builder| {
        builder
          .load_psks(provider.storage())
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| {
        builder
          .build(
            provider.rand(),
            provider.crypto(),
            &world.alice.user.device,
            |_| true,
          )
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| builder.stage_commit(provider).map_err(|error| error.to_string()));
    assert!(
      result.is_err(),
      "openmls required the anchor of an old app's leaf"
    );
  }

  #[test]
  fn every_new_leaf_announces_the_anchor() {
    let world = AnchoredWorld::new();
    for bytes in [
      world.bob.key_package(),
      crate::mls::create_last_resort_key_package(
        &world.bob.provider,
        &world.bob.user.device,
        &world.bob.user.certificate,
        now_secs(),
      )
      .unwrap(),
    ] {
      let key_package = validated(&world.bob.provider, &bytes);
      assert!(
        key_package
          .leaf_node()
          .capabilities()
          .extensions()
          .contains(&ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE))
      );
    }
  }
}

/// Alice (workspace admin) founded an anchored channel and added Carol; Bob is outside.
struct AnchoredWorld {
  alice: Client,
  bob: Client,
  carol: Client,
  directory: TestDirectory,
  authority: TestAuthority,
  alice_group: ConversationGroup,
  carol_group: ConversationGroup,
}

impl AnchoredWorld {
  fn new() -> Self {
    let alice = Client::new(ALICE);
    let bob = Client::new(BOB);
    let carol = Client::new(CAROL);
    let directory = TestDirectory::with(&[&alice.user, &bob.user, &carol.user]);
    let authority = TestAuthority::allowing(&[UserId::new(ALICE)]);
    let anchor = found(&alice, "general");
    let mut alice_group = ConversationGroup::create_anchored(
      &alice.provider,
      &alice.user.device,
      &alice.user.certificate,
      CONVERSATION,
      &anchor,
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
    Self {
      alice,
      bob,
      carol,
      directory,
      authority,
      alice_group,
      carol_group,
    }
  }

  fn anchor(&self) -> ChannelAnchor {
    found(&self.alice, "general")
  }

  fn anchor_named(&self, name: &str) -> ChannelAnchor {
    found(&self.alice, name)
  }

  fn current_anchor(&self) -> ChannelAnchor {
    ChannelAnchor::from_bytes(
      self.carol_group.anchor_bytes().unwrap(),
      crate::channel_anchor::DEFAULT_MAX_ANCHOR_BYTES,
    )
    .unwrap()
  }

  /// Alice's signed move or rename of the current anchor, in the current epoch.
  fn alice_change(&self, maybe_space_id: Option<SpaceId>, name: &str) -> SignedPropertiesChange {
    self
      .current_anchor()
      .sign_change(
        &self.alice.user.device,
        &self.alice.user.certificate,
        NameAuthority::Workspace,
        &PropertiesTarget { maybe_space_id, name },
        self.alice_group.epoch(),
        1,
      )
      .unwrap()
  }

  fn alice_commits_change(&mut self, signed: &SignedPropertiesChange) -> CommitOutput {
    let output = self
      .alice_group
      .commit_properties_change(
        &self.alice.provider,
        &self.alice.user.device,
        signed,
        &self.directory,
        &self.authority,
      )
      .unwrap();
    self.alice_group.confirm_commit(&self.alice.provider).unwrap();
    output
  }

  fn signed_add_bob(&self) -> SignedMembershipChange {
    MembershipChange::new(
      CONVERSATION,
      self.alice_group.epoch(),
      add(BOB, HistoryShare::Full),
      &mut OsRng,
    )
    .sign(&self.alice.user.device, self.alice.user.certificate.clone())
    .unwrap()
  }

  fn add_bob(&mut self) -> CommitOutput {
    let change = self.signed_add_bob();
    let output = self
      .alice_group
      .commit_membership_change(
        &self.alice.provider,
        &self.alice.user.device,
        &change,
        &[self.bob.key_package()],
        &self.directory,
        &self.authority,
      )
      .unwrap();
    self.alice_group.confirm_commit(&self.alice.provider).unwrap();
    output
  }

  /// The group context as it is, with the anchor replaced by `maybe_anchor` (dropped for `None`)
  /// and the requirement kept or dropped.
  fn extensions_with(
    &self,
    maybe_anchor: Option<&ChannelAnchor>,
    is_required: bool,
  ) -> Extensions<GroupContext> {
    let mut extensions = Vec::new();
    if let Some(anchor) = maybe_anchor {
      extensions.push(Extension::Unknown(
        CHANNEL_ANCHOR_EXTENSION_TYPE,
        UnknownExtension(anchor.to_bytes().unwrap()),
      ));
    }
    if is_required {
      extensions.push(Extension::RequiredCapabilities(
        RequiredCapabilitiesExtension::new(
          &[ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE)],
          &[],
          &[],
        ),
      ));
    }
    Extensions::from_vec(extensions).unwrap()
  }

  /// Alice commits arbitrary context extensions (and adds), bypassing TeamCraft's checks – as a
  /// malicious or buggy member client could.
  fn raw_context_commit(
    &mut self,
    extensions: Extensions<GroupContext>,
    aad: Vec<u8>,
    key_packages: &[Vec<u8>],
  ) -> Vec<u8> {
    self
      .try_raw_context_commit(extensions, aad, key_packages)
      .unwrap()
  }

  fn try_raw_context_commit(
    &mut self,
    extensions: Extensions<GroupContext>,
    aad: Vec<u8>,
    key_packages: &[Vec<u8>],
  ) -> Result<Vec<u8>, String> {
    let key_packages: Vec<KeyPackage> = key_packages
      .iter()
      .map(|bytes| validated(&self.alice.provider, bytes))
      .collect();
    let provider = &self.alice.provider;
    self.alice_group.group.set_aad(aad);
    let result = self
      .alice_group
      .group
      .commit_builder()
      .propose_adds(key_packages)
      .propose_group_context_extensions(extensions)
      .map_err(|error| error.to_string())
      .and_then(|builder| {
        builder
          .load_psks(provider.storage())
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| {
        builder
          .build(
            provider.rand(),
            provider.crypto(),
            &self.alice.user.device,
            |_| true,
          )
          .map_err(|error| error.to_string())
      })
      .and_then(|builder| builder.stage_commit(provider).map_err(|error| error.to_string()));
    self.alice_group.group.set_aad(Vec::new());
    let bundle = result?;
    let commit = bundle
      .into_commit()
      .to_bytes()
      .map_err(|error| error.to_string())?;
    self
      .alice_group
      .discard_commit(provider)
      .map_err(|error| error.to_string())?;
    Ok(commit)
  }

  fn raw_add_commit(&mut self, key_packages: &[Vec<u8>], aad: Vec<u8>) -> Vec<u8> {
    let key_packages = key_packages
      .iter()
      .map(|bytes| validated(&self.alice.provider, bytes))
      .collect();
    self.alice_group.group.set_aad(aad);
    let output = self.alice_group.stage_commit(
      &self.alice.provider,
      &self.alice.user.device,
      key_packages,
      Vec::new(),
      false,
    );
    self.alice_group.group.set_aad(Vec::new());
    self.alice_group.discard_commit(&self.alice.provider).unwrap();
    output.unwrap().commit
  }

  fn alice_processes(&mut self, message: &[u8]) -> Result<GroupEvent, MlsError> {
    self
      .alice_group
      .process_message(&self.alice.provider, message, &self.directory, &self.authority)
  }

  fn carol_processes(&mut self, message: &[u8]) -> Result<GroupEvent, MlsError> {
    self
      .carol_group
      .process_message(&self.carol.provider, message, &self.directory, &self.authority)
  }

  fn expect_carol_refuses(&mut self, commit: &[u8], is_expected: impl Fn(&MlsError) -> bool) {
    let epoch = self.carol_group.epoch();
    let anchor = self.carol_group.anchor_bytes().map(<[u8]>::to_vec);
    let error = self.carol_processes(commit).expect_err("commit must be refused");
    assert!(is_expected(&error), "unexpected error: {error}");
    assert_eq!(self.carol_group.epoch(), epoch);
    assert_eq!(self.carol_group.anchor_bytes().map(<[u8]>::to_vec), anchor);
  }
}

impl Client {
  /// A key package as an app from before the anchor makes it: no capability for it.
  fn old_key_package(&self) -> Vec<u8> {
    let builder = KeyPackage::builder().key_package_lifetime(crate::mls::lifetime_from(now_secs()));
    crate::mls::build_key_package(&self.provider, &self.user.device, &self.user.certificate, builder).unwrap()
  }
}

fn found(client: &Client, name: &str) -> ChannelAnchor {
  ChannelAnchor::found(
    &client.user.device,
    &client.user.certificate,
    &Founding {
      conversation_id: CONVERSATION,
      kind: AnchorKind::Channel,
      device_list_version: 1,
      signed_at_ms: 1_700_000_000_000,
      maybe_space_id: Some(SPACE),
      name,
    },
  )
  .unwrap()
}

fn expectation() -> AnchorExpectation {
  AnchorExpectation {
    conversation_id: CONVERSATION,
    kind: ExpectedKind::Channel,
  }
}

fn properties_aad(signed: &SignedPropertiesChange) -> Vec<u8> {
  let mut aad = PROPERTIES_CHANGE_AAD_MAGIC.to_vec();
  aad.extend_from_slice(&signed.to_bytes().unwrap());
  aad
}

fn validated(provider: &DefaultMlsProvider, bytes: &[u8]) -> KeyPackage {
  KeyPackageIn::tls_deserialize_exact(bytes)
    .unwrap()
    .validate(provider.crypto(), ProtocolVersion::Mls10)
    .unwrap()
}

/// Leaf parameters as an app from before the anchor sets them.
fn old_leaf() -> LeafNodeParameters {
  LeafNodeParameters::builder()
    .with_capabilities(Capabilities::default())
    .build()
}
