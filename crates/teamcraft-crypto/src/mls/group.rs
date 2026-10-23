//! One MLS group per conversation, with TeamCraft's commit authorization rules.

use std::collections::BTreeSet;

use openmls::prelude::{
  GroupId, KeyPackage, LeafNodeIndex, MlsGroup, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn,
  ProcessedMessageContent, Proposal, Sender, StagedCommit, StagedWelcome,
};
use openmls_traits::OpenMlsProvider;
use tls_codec::{
  Deserialize as _, DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};

use super::content_keys::ContentKeyBundle;
use super::{CIPHERSUITE, GroupMember, MlsError, credential, validate_key_package};
use crate::content_key::{ContentKey, ContentKeyId};
use crate::domain;
use crate::identity::{DeviceKeyPair, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, UserId};
use crate::membership::{
  ChangeId, IdentityDirectory, MembershipAction, MembershipAuthority, MembershipChange,
  SignedMembershipChange,
};
use crate::secret::SymmetricKey;

/// Prefix of MLS group IDs; the conversation ID follows as big-endian `u64`.
const GROUP_ID_PREFIX: &[u8] = b"teamcraft/conversation/";
/// How many epochs after signing a membership change may still be committed (if nothing changed its
/// target user meanwhile). A constant rather than a setting: every member must decide the same way,
/// or the group splits.
pub const MAX_CHANGE_AGE_EPOCHS: u64 = 32;

/// This device's view of a conversation's MLS group.
#[derive(Debug)]
pub struct ConversationGroup {
  conversation_id: ConversationId,
  group: MlsGroup,
  /// Changes already committed, to refuse replays of the same signed change.
  applied_changes: BTreeSet<ChangeId>,
  /// Which users signed changes touched recently, and since when this device knows the group.
  history: ChangeHistory,
  maybe_pending: Option<PendingEffect>,
}

/// What this device's pending commit does, recorded once it is confirmed.
#[derive(Debug)]
struct PendingEffect {
  maybe_change: Option<MembershipChange>,
  removed: Vec<GroupMember>,
}

/// What this device saw of the group's signed changes, persisted with the MLS state: a change
/// signed before this device joined, or before a later change to the same user, is refused
/// (security review A3).
#[derive(Clone, Debug, Default, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct ChangeHistory {
  joined_epoch: u64,
  /// Epochs in which a commit for a signed change about the user was made, newest last.
  touched: Vec<TouchedUser>,
  /// Devices revoked (removed one by one) while this device was in the group; they never come back
  /// (a revoked device's old key packages and certificate stay valid, security review A4).
  removed_devices: Vec<GroupMember>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct TouchedUser {
  epoch: u64,
  user_id: UserId,
}

/// What to send after creating a commit: the commit to the group, the welcome to new members.
#[derive(Debug)]
pub struct CommitOutput {
  pub commit: Vec<u8>,
  pub maybe_welcome: Option<Vec<u8>>,
}

/// Result of processing an incoming MLS message.
#[derive(Debug)]
pub enum GroupEvent {
  /// Another member distributed content keys. Apply them with [`ContentKeyBundle::apply_to`].
  ContentKeys {
    sender: GroupMember,
    bundle: ContentKeyBundle,
  },
  /// An authorized commit was merged; the group is in a new epoch.
  MembershipChanged(MembershipUpdate),
}

/// Members added and removed by a merged commit.
#[derive(Debug, Default)]
pub struct MembershipUpdate {
  /// The signed change that authorized the commit; `None` for additions of devices of existing
  /// members, which only need the user's cross-signature.
  pub maybe_change: Option<MembershipChange>,
  pub added: Vec<GroupMember>,
  pub removed: Vec<GroupMember>,
  pub is_self_removed: bool,
}

/// The membership effect of a commit, with every added device verified.
#[derive(Default)]
struct ProposedMembership {
  added: Vec<VerifiedDevice>,
  removed: Vec<GroupMember>,
}

impl ConversationGroup {
  /// Creates the group for a new conversation with this device as the only member.
  pub fn create(
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    conversation_id: ConversationId,
  ) -> Result<Self, MlsError> {
    let group = MlsGroup::builder()
      .with_group_id(group_id(conversation_id))
      .ciphersuite(CIPHERSUITE)
      .use_ratchet_tree_extension(true)
      .build(provider, device, credential::credential_with_key(certificate)?)
      .map_err(|error| MlsError::openmls("group creation", error))?;
    Ok(Self::from_group(conversation_id, group, ChangeHistory::default()))
  }

  /// Joins a group from a welcome. Every member credential in the group is verified against the
  /// pinned identities before the group is accepted.
  pub fn join(
    provider: &impl OpenMlsProvider,
    welcome: &[u8],
    conversation_id: ConversationId,
    directory: &impl IdentityDirectory,
  ) -> Result<Self, MlsError> {
    let MlsMessageBodyIn::Welcome(welcome) = MlsMessageIn::tls_deserialize_exact(welcome)?.extract() else {
      return Err(MlsError::UnexpectedMessage("expected a welcome"));
    };
    let config = MlsGroupJoinConfig::builder()
      .use_ratchet_tree_extension(true)
      .build();
    // Leaves that never committed still carry the lifetime of the key package they joined with;
    // checking it would make every group unjoinable once such a member's package expired. A
    // lifetime only bounds when a package may be *added* (RFC 9420 §7.3, openmls #1810), which
    // the committer and every member checked then; credentials are verified below.
    let staged = StagedWelcome::build_from_welcome(provider, &config, welcome)
      .map_err(|error| MlsError::openmls("welcome processing", error))?
      .skip_lifetime_validation()
      .build()
      .map_err(|error| MlsError::openmls("welcome processing", error))?;
    if staged.group_context().group_id() != &group_id(conversation_id) {
      return Err(MlsError::UnexpectedMessage("welcome for another conversation"));
    }
    if staged.group_context().ciphersuite() != CIPHERSUITE {
      return Err(MlsError::UnexpectedMessage("welcome with another ciphersuite"));
    }
    for member in staged.members() {
      credential::verify(&member.credential, &member.signature_key, directory)?;
    }
    let group = staged
      .into_group(provider)
      .map_err(|error| MlsError::openmls("joining group", error))?;
    let history = ChangeHistory {
      joined_epoch: group.epoch().as_u64(),
      ..ChangeHistory::default()
    };
    Ok(Self::from_group(conversation_id, group, history))
  }

  /// Loads a group from the provider's storage, with the change IDs applied so far and the change
  /// history (persisted by the caller next to the MLS state). `None` if the provider has no state
  /// for the conversation.
  pub fn load(
    provider: &impl OpenMlsProvider,
    conversation_id: ConversationId,
    applied_changes: impl IntoIterator<Item = ChangeId>,
    history: ChangeHistory,
  ) -> Result<Option<Self>, MlsError> {
    let maybe_group = MlsGroup::load(provider.storage(), &group_id(conversation_id))
      .map_err(|error| MlsError::openmls("loading group", error))?;
    Ok(maybe_group.map(|group| {
      let mut loaded = Self::from_group(conversation_id, group, history);
      loaded.applied_changes.extend(applied_changes);
      loaded
    }))
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  /// Signed changes this group has committed or accepted; persist them with the MLS state.
  pub fn applied_changes(&self) -> impl Iterator<Item = &ChangeId> {
    self.applied_changes.iter()
  }

  /// Persist with the MLS state and pass back to [`Self::load`].
  pub fn change_history(&self) -> &ChangeHistory {
    &self.history
  }

  /// Removes the group's state from the provider's storage, e.g. after this device was removed.
  pub fn delete(mut self, provider: &impl OpenMlsProvider) -> Result<(), MlsError> {
    self
      .group
      .delete(provider.storage())
      .map_err(|error| MlsError::openmls("deleting group", error))
  }

  pub fn has_pending_commit(&self) -> bool {
    self.group.pending_commit().is_some()
  }

  pub fn epoch(&self) -> u64 {
    self.group.epoch().as_u64()
  }

  /// The content key of the current epoch, derived from the MLS exporter (RFC 9420 §8.5). Every
  /// member that merged the epoch's commit derives the same key, and nobody else can: after a
  /// removal it becomes the current key at once, so the new key can't be withheld from members and
  /// the removed device never learns it (security review A5).
  pub fn epoch_content_key(&self, provider: &impl OpenMlsProvider) -> Result<ContentKey, MlsError> {
    let mut context = self.conversation_id.get().to_be_bytes().to_vec();
    context.extend_from_slice(&self.epoch().to_be_bytes());
    let exported = zeroize::Zeroizing::new(
      self
        .group
        .export_secret(provider.crypto(), domain::EPOCH_CONTENT_KEY, &context, 48)
        .map_err(|error| MlsError::openmls("exporting the epoch key", error))?,
    );
    let (id, key) = exported.split_at(16);
    let id = <[u8; 16]>::try_from(id).map_err(|_| MlsError::UnexpectedMessage("short export"))?;
    let key = SymmetricKey::from_slice(key).ok_or(MlsError::UnexpectedMessage("short export"))?;
    Ok(ContentKey::from_parts(ContentKeyId::from_bytes(id), key))
  }

  /// All member devices.
  pub fn members(&self) -> Result<Vec<GroupMember>, MlsError> {
    self
      .group
      .members()
      .map(|member| credential::member_of(&member.credential))
      .collect()
  }

  /// All member devices with their certificates verified against the pinned identities, e.g. to
  /// check the call key bindings of a conversation's call (e2ee.md §8). Unlike the certificates the
  /// server stores (provisional, from registration), these carry the final device ids.
  pub fn verified_members(
    &self,
    directory: &impl IdentityDirectory,
  ) -> Result<Vec<VerifiedDevice>, MlsError> {
    self
      .group
      .members()
      .map(|member| credential::verify(&member.credential, &member.signature_key, directory))
      .collect()
  }

  /// Verifies a signed membership change and creates the commit for it. Adds need the key packages
  /// of every device of the added user; removes need none. The commit stays pending until
  /// [`Self::confirm_commit`] (after the server accepted it) or [`Self::discard_commit`].
  pub fn commit_membership_change(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    signed_change: &SignedMembershipChange,
    key_packages: &[Vec<u8>],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<CommitOutput, MlsError> {
    let verified = signed_change.verify(directory, authority)?;
    let change = verified.change();
    let mut validated_key_packages = Vec::with_capacity(key_packages.len());
    let mut proposed = ProposedMembership::default();
    for bytes in key_packages {
      let (key_package, added_device) = validate_key_package(provider, bytes, directory)?;
      validated_key_packages.push(key_package);
      proposed.added.push(added_device);
    }
    let removed_leaves = self.leaves_removed_by(change.action())?;
    for (_, member) in &removed_leaves {
      proposed.removed.push(*member);
    }
    self.check_change_matches(change, &proposed)?;
    self.check_not_removed(&proposed)?;

    self.group.set_aad(signed_change.to_bytes()?);
    let result = self.stage_commit(provider, device, validated_key_packages, removed_leaves);
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingEffect {
      maybe_change: Some(change.clone()),
      removed: proposed.removed,
    });
    Ok(output)
  }

  /// Checks one key package fetched from the server the way the commit functions do (signature,
  /// lifetime, credential, signed device list, not revoked from this group), so the caller can leave
  /// out one device's bad package instead of failing the whole commit. Returns whose it is.
  pub fn check_key_package(
    &self,
    provider: &impl OpenMlsProvider,
    bytes: &[u8],
    directory: &impl IdentityDirectory,
  ) -> Result<GroupMember, MlsError> {
    let (_, device) = validate_key_package(provider, bytes, directory)?;
    let member = GroupMember::from(&device);
    self.check_not_removed(&ProposedMembership {
      added: vec![device],
      removed: Vec::new(),
    })?;
    Ok(member)
  }

  /// Adds further devices of users who are already members. Needs no admin signature: each
  /// device is cross-signed by its user's identity key (e2ee.md §3).
  pub fn commit_device_additions(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    key_packages: &[Vec<u8>],
    directory: &impl IdentityDirectory,
  ) -> Result<CommitOutput, MlsError> {
    let mut validated_key_packages = Vec::with_capacity(key_packages.len());
    let mut proposed = ProposedMembership::default();
    for bytes in key_packages {
      let (key_package, added_device) = validate_key_package(provider, bytes, directory)?;
      validated_key_packages.push(key_package);
      proposed.added.push(added_device);
    }
    self.check_device_additions(&proposed)?;
    self.check_not_removed(&proposed)?;
    let output = self.stage_commit(provider, device, validated_key_packages, Vec::new())?;
    self.maybe_pending = None;
    Ok(output)
  }

  /// Merges the pending commit after the server accepted it.
  pub fn confirm_commit(&mut self, provider: &impl OpenMlsProvider) -> Result<(), MlsError> {
    if self.group.pending_commit().is_none() {
      return Err(MlsError::NoPendingCommit);
    }
    let epoch = self.epoch();
    self
      .group
      .merge_pending_commit(provider)
      .map_err(|error| MlsError::openmls("merging own commit", error))?;
    if let Some(effect) = self.maybe_pending.take() {
      self.record_commit(effect.maybe_change.as_ref(), &effect.removed, epoch);
    }
    Ok(())
  }

  /// Drops the pending commit, e.g. because another member's commit for this epoch won.
  pub fn discard_commit(&mut self, provider: &impl OpenMlsProvider) -> Result<(), MlsError> {
    self.maybe_pending = None;
    self
      .group
      .clear_pending_commit(provider.storage())
      .map_err(|error| MlsError::openmls("discarding own commit", error))
  }

  /// Sends content keys to all current members as an MLS application message.
  pub fn send_content_keys(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    bundle: &ContentKeyBundle,
  ) -> Result<Vec<u8>, MlsError> {
    self.ensure_conversation(bundle.conversation_id())?;
    let payload = bundle.to_bytes()?;
    let message = self
      .group
      .create_message(provider, device, payload.expose_secret())
      .map_err(|error| MlsError::openmls("application message", error))?;
    message
      .to_bytes()
      .map_err(|error| MlsError::openmls("message encoding", error))
  }

  /// Processes a commit or application message from the group. Commits are merged only if they
  /// pass TeamCraft's authorization rules; otherwise the group stays in its current epoch.
  pub fn process_message(
    &mut self,
    provider: &impl OpenMlsProvider,
    message: &[u8],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<GroupEvent, MlsError> {
    let protocol_message = MlsMessageIn::tls_deserialize_exact(message)?
      .try_into_protocol_message()
      .map_err(|_| MlsError::UnexpectedMessage("expected a group message"))?;
    let processed = self
      .group
      .process_message(provider, protocol_message)
      .map_err(|error| MlsError::openmls("message processing", error))?;
    let Sender::Member(sender_index) = *processed.sender() else {
      return Err(MlsError::ExternalSender);
    };
    let sender = credential::member_of(processed.credential())?;
    let authenticated_data = processed.aad().to_vec();
    match processed.into_content() {
      ProcessedMessageContent::ApplicationMessage(application) => {
        let bundle = ContentKeyBundle::from_bytes(&zeroize::Zeroizing::new(application.into_bytes()))?;
        self.ensure_conversation(bundle.conversation_id())?;
        Ok(GroupEvent::ContentKeys { sender, bundle })
      },
      ProcessedMessageContent::StagedCommitMessage(staged) => {
        let update =
          self.authorize_commit(&staged, sender_index, &authenticated_data, directory, authority)?;
        let epoch = self.epoch();
        self
          .group
          .merge_staged_commit(provider, *staged)
          .map_err(|error| MlsError::openmls("merging commit", error))?;
        self.record_commit(update.maybe_change.as_ref(), &update.removed, epoch);
        Ok(GroupEvent::MembershipChanged(update))
      },
      _ => Err(MlsError::UnexpectedMessage("standalone proposals are not used")),
    }
  }

  fn from_group(conversation_id: ConversationId, group: MlsGroup, history: ChangeHistory) -> Self {
    Self {
      conversation_id,
      group,
      applied_changes: BTreeSet::new(),
      history,
      maybe_pending: None,
    }
  }

  /// Notes a merged commit made in `epoch`: its signed change and the devices it removed.
  fn record_commit(&mut self, maybe_change: Option<&MembershipChange>, removed: &[GroupMember], epoch: u64) {
    if let Some(change) = maybe_change {
      self.applied_changes.insert(change.change_id());
      self.history.touched.push(TouchedUser {
        epoch,
        user_id: change.action().target_user(),
      });
    }
    // A removed user may be added again later; a revoked device may not.
    let is_device_revocation =
      maybe_change.is_some_and(|change| matches!(change.action(), MembershipAction::RemoveDevice { .. }));
    if is_device_revocation {
      for member in removed {
        if !self.history.removed_devices.contains(member) {
          self.history.removed_devices.push(*member);
        }
      }
    }
    let current = self.epoch();
    self
      .history
      .touched
      .retain(|touched| touched.epoch.saturating_add(MAX_CHANGE_AGE_EPOCHS) >= current);
  }

  fn ensure_conversation(&self, actual: ConversationId) -> Result<(), MlsError> {
    if actual == self.conversation_id {
      Ok(())
    } else {
      Err(MlsError::WrongConversation {
        expected: self.conversation_id,
        actual,
      })
    }
  }

  /// Leaves to remove for a change; empty for additions.
  fn leaves_removed_by(
    &self,
    action: &MembershipAction,
  ) -> Result<Vec<(LeafNodeIndex, GroupMember)>, MlsError> {
    let mut leaves = Vec::new();
    for member in self.group.members() {
      let group_member = credential::member_of(&member.credential)?;
      let is_target = match *action {
        MembershipAction::AddUser { .. } => false,
        MembershipAction::RemoveUser { user_id } => group_member.user_id == user_id,
        MembershipAction::RemoveDevice { user_id, device_id } => {
          group_member.user_id == user_id && group_member.device_id == device_id
        },
      };
      if is_target {
        leaves.push((member.index, group_member));
      }
    }
    Ok(leaves)
  }

  fn stage_commit(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    key_packages: Vec<KeyPackage>,
    removed_leaves: Vec<(LeafNodeIndex, GroupMember)>,
  ) -> Result<CommitOutput, MlsError> {
    let bundle = self
      .group
      .commit_builder()
      .propose_adds(key_packages)
      .propose_removals(removed_leaves.into_iter().map(|(index, _)| index))
      .load_psks(provider.storage())
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .build(provider.rand(), provider.crypto(), device, |_| true)
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .stage_commit(provider)
      .map_err(|error| MlsError::openmls("commit staging", error))?;
    let maybe_welcome = bundle
      .to_welcome_msg()
      .map(|welcome| welcome.to_bytes())
      .transpose()
      .map_err(|error| MlsError::openmls("welcome encoding", error))?;
    let commit = bundle
      .into_commit()
      .to_bytes()
      .map_err(|error| MlsError::openmls("commit encoding", error))?;
    Ok(CommitOutput {
      commit,
      maybe_welcome,
    })
  }

  /// TeamCraft's rules for accepting another member's commit (e2ee.md §4.6, admin.md §2.3).
  fn authorize_commit(
    &self,
    staged: &StagedCommit,
    sender_index: LeafNodeIndex,
    authenticated_data: &[u8],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<MembershipUpdate, MlsError> {
    let proposed = self.verify_commit_contents(staged, sender_index, directory)?;
    let maybe_change = if authenticated_data.is_empty() {
      if !proposed.removed.is_empty() {
        return Err(MlsError::MissingSignedChange);
      }
      self.check_device_additions(&proposed)?;
      None
    } else {
      let signed = SignedMembershipChange::from_bytes(authenticated_data)?;
      let verified = signed.verify(directory, authority)?;
      self.check_change_matches(verified.change(), &proposed)?;
      Some(verified.change().clone())
    };
    Ok(MembershipUpdate {
      maybe_change,
      added: proposed.added.iter().map(GroupMember::from).collect(),
      removed: proposed.removed,
      is_self_removed: staged.self_removed(),
    })
  }

  /// Verifies every credential the commit introduces and collects its membership effect.
  fn verify_commit_contents(
    &self,
    staged: &StagedCommit,
    sender_index: LeafNodeIndex,
    directory: &impl IdentityDirectory,
  ) -> Result<ProposedMembership, MlsError> {
    for queued in staged.queued_proposals() {
      if !matches!(
        queued.proposal(),
        Proposal::Add(_) | Proposal::Remove(_) | Proposal::Update(_)
      ) {
        return Err(MlsError::UnsupportedProposal);
      }
      if !matches!(queued.sender(), Sender::Member(_)) {
        return Err(MlsError::ExternalSender);
      }
    }
    let mut proposed = ProposedMembership::default();
    for add in staged.add_proposals() {
      let leaf = add.add_proposal().key_package().leaf_node();
      let device = credential::verify(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
      if !directory.is_active_device(&device) {
        return Err(MlsError::InactiveDevice(device.device_id()));
      }
      proposed.added.push(device);
    }
    self.check_not_removed(&proposed)?;
    for remove in staged.remove_proposals() {
      let removed = self
        .group
        .member(remove.remove_proposal().removed())
        .ok_or(MlsError::ChangeDoesNotMatchCommit)?;
      proposed.removed.push(credential::member_of(removed)?);
    }
    for update in staged.update_proposals() {
      let Sender::Member(index) = *update.sender() else {
        return Err(MlsError::ExternalSender);
      };
      self.verify_same_identity(index, update.update_proposal().leaf_node(), directory)?;
    }
    if let Some(leaf) = staged.update_path_leaf_node() {
      self.verify_same_identity(sender_index, leaf, directory)?;
    }
    Ok(proposed)
  }

  /// A member may refresh its leaf but not turn into another user or device.
  fn verify_same_identity(
    &self,
    index: LeafNodeIndex,
    leaf: &openmls::prelude::LeafNode,
    directory: &impl IdentityDirectory,
  ) -> Result<(), MlsError> {
    let current = self
      .group
      .member(index)
      .ok_or(MlsError::ChangeDoesNotMatchCommit)?;
    let before = credential::member_of(current)?;
    let after = credential::verify(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
    if GroupMember::from(&after) == before {
      Ok(())
    } else {
      Err(MlsError::IdentityChangedInUpdate)
    }
  }

  /// Checks that the proposals do exactly what the signed change says, no more and no less.
  fn check_change_matches(
    &self,
    change: &MembershipChange,
    proposed: &ProposedMembership,
  ) -> Result<(), MlsError> {
    self.ensure_conversation(change.conversation_id())?;
    if self.applied_changes.contains(&change.change_id()) {
      return Err(MlsError::ReplayedChange(change.change_id()));
    }
    self.ensure_fresh(change)?;
    let expected_removed: Vec<GroupMember> = self
      .leaves_removed_by(change.action())?
      .into_iter()
      .map(|(_, member)| member)
      .collect();
    let matches = match *change.action() {
      MembershipAction::AddUser { user_id, .. } => {
        proposed.removed.is_empty()
          && !proposed.added.is_empty()
          && proposed.added.iter().all(|device| device.user_id() == user_id)
      },
      MembershipAction::RemoveUser { .. } | MembershipAction::RemoveDevice { .. } => {
        proposed.added.is_empty()
          && !expected_removed.is_empty()
          && same_members(&proposed.removed, &expected_removed)
      },
    };
    if matches {
      Ok(())
    } else {
      Err(MlsError::ChangeDoesNotMatchCommit)
    }
  }

  /// A change is valid in the epoch it was signed for and up to [`MAX_CHANGE_AGE_EPOCHS`] later,
  /// unless this device joined after it was signed or another signed change about the same user
  /// was committed since (then it may be stale: an add replayed after a removal).
  fn ensure_fresh(&self, change: &MembershipChange) -> Result<(), MlsError> {
    let current = self.epoch();
    let signed = change.epoch();
    let target = change.action().target_user();
    let is_fresh = signed <= current
      && signed >= self.history.joined_epoch
      && current - signed <= MAX_CHANGE_AGE_EPOCHS
      && !self
        .history
        .touched
        .iter()
        .any(|touched| touched.user_id == target && touched.epoch >= signed);
    if is_fresh {
      Ok(())
    } else {
      Err(MlsError::StaleChange {
        signed_epoch: signed,
        current_epoch: current,
      })
    }
  }

  /// Devices removed from the group before stay out, whatever the server serves for them.
  fn check_not_removed(&self, proposed: &ProposedMembership) -> Result<(), MlsError> {
    match proposed
      .added
      .iter()
      .find(|device| self.history.removed_devices.contains(&GroupMember::from(*device)))
    {
      Some(device) => Err(MlsError::RemovedDevice(device.device_id())),
      None => Ok(()),
    }
  }

  /// Unsigned additions are only allowed for further devices of users who are already members.
  fn check_device_additions(&self, proposed: &ProposedMembership) -> Result<(), MlsError> {
    let members = self.members()?;
    let all_existing_users = proposed
      .added
      .iter()
      .all(|device| members.iter().any(|member| member.user_id == device.user_id()));
    if !proposed.added.is_empty() && all_existing_users {
      Ok(())
    } else {
      Err(MlsError::MissingSignedChange)
    }
  }
}

impl ChangeHistory {
  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
  }
}

fn group_id(conversation_id: ConversationId) -> GroupId {
  let mut bytes = GROUP_ID_PREFIX.to_vec();
  bytes.extend_from_slice(&conversation_id.get().to_be_bytes());
  GroupId::from_slice(&bytes)
}

fn same_members(left: &[GroupMember], right: &[GroupMember]) -> bool {
  let left: BTreeSet<_> = left.iter().collect();
  let right: BTreeSet<_> = right.iter().collect();
  left == right
}

#[cfg(test)]
mod tests;
