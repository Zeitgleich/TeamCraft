//! One MLS group per conversation, with TeamCraft's commit authorization rules.

use std::collections::BTreeSet;

use openmls::messages::group_info::VerifiableGroupInfo;
use openmls::prelude::CommitMessageBundle;
use openmls::prelude::{
  Extension, ExtensionType, Extensions, GroupContext, GroupEpoch, GroupId, KeyPackage, LeafNodeIndex,
  LeafNodeParameters, MlsGroup, MlsGroupJoinConfig, MlsGroupState, MlsMessageBodyIn, MlsMessageIn,
  MlsMessageOut, ProcessedMessageContent, Proposal, RequiredCapabilitiesExtension, Sender, StagedCommit,
  StagedWelcome, UnknownExtension, WelcomeError,
};
use openmls_traits::OpenMlsProvider;
use openmls_traits::storage::StorageProvider as _;
use tls_codec::{
  Deserialize as _, DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};
use zeroize::Zeroizing;

use super::content_keys::{ApplicationPayload, ContentKeyBundle, SealedContentKeys};
use super::rejoin::{SignedRejoinRequest, VerifiedRejoinRequest};
use super::{CIPHERSUITE, GroupMember, MlsError, credential, leaf_capabilities, validate_key_package};
use crate::channel_anchor::{
  AnchorExpectation, AnchorKind, CHANNEL_ANCHOR_EXTENSION_TYPE, ChannelAnchor, DEFAULT_MAX_ANCHOR_BYTES,
  Judged, PROPERTIES_CHANGE_AAD_MAGIC, SignedPropertiesChange, Standing, VerifiedAnchor,
};
use crate::content_key::{ContentKey, ContentKeyId};
use crate::domain;
use crate::identity::{DeviceKeyPair, SignedDeviceCertificate, VerifiedDevice};
use crate::ids::{ConversationId, UserId};
use crate::membership::{
  ChangeId, IdentityDirectory, MembershipAction, MembershipAuthority, MembershipChange, MembershipError,
  SignedMembershipChange,
};
use crate::secret::SymmetricKey;
use crate::strict_roles::{
  STRICT_ROLES_AAD_MAGIC, STRICT_ROLES_EXTENSION_TYPE, SignedStrictRolesChange, StrictRoleKind, StrictRoles,
  StrictRolesError, StrictRolesVerdict, StrictSigner, TreeLeaf, membership_verdict,
};

/// Prefix of MLS group IDs; the conversation ID follows as big-endian `u64`.
const GROUP_ID_PREFIX: &[u8] = b"teamcraft/conversation/";
/// Marks a commit's authenticated data as a [`SignedRejoinRequest`] (a signed membership change
/// starts with its version byte, never with this).
const REJOIN_AAD_MAGIC: &[u8] = b"TCRJ";
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
  maybe_pending: Option<PendingCommit>,
}

/// What the provider's storage says about a conversation's group without loading it
/// ([`ConversationGroup::stored_state`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredGroupState {
  Operational,
  /// This device's commit waits for the server's answer.
  PendingCommit,
  /// This device was removed from the group.
  Inactive,
}

/// This device's commit that waits for the server's answer, and what it does once merged. Persist
/// it with the MLS state ([`ConversationGroup::pending_commit`], back through
/// [`ConversationGroup::load`]): if the answer is lost, the commit is merged when it shows up in
/// the group's queue, which takes its bytes (e2ee.md §4.6).
#[derive(Clone, Debug, PartialEq, Eq, TlsSerialize, TlsDeserializeBytes, TlsSize)]
pub struct PendingCommit {
  commit: Vec<u8>,
  maybe_change: Option<MembershipChange>,
  removed: Vec<GroupMember>,
  /// The device a rejoin commit adds back, and the request it answers.
  maybe_rejoin: Option<(GroupMember, ChangeId)>,
  /// The move or rename a properties commit makes, so the committer raises its anchor floor too.
  /// Encoded only when set, so older apps keep reading every other pending commit.
  maybe_properties_change: Option<SignedPropertiesChange>,
}

/// [`PendingCommit`] as apps before the anchor stored it.
#[derive(TlsSerialize, TlsDeserializeBytes, TlsSize)]
struct LegacyPendingCommit {
  commit: Vec<u8>,
  maybe_change: Option<MembershipChange>,
  removed: Vec<GroupMember>,
  maybe_rejoin: Option<(GroupMember, ChangeId)>,
}

/// A decrypted welcome whose leaves aren't verified yet ([`ConversationGroup::stage_join`]).
pub struct StagedJoin {
  conversation_id: ConversationId,
  staged: StagedWelcome,
}

/// The verified members of a staged welcome's group ([`StagedJoin::verify`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedWelcome {
  /// Every device in the group, this one included.
  pub members: Vec<GroupMember>,
  /// The member whose commit made the welcome.
  pub sender: GroupMember,
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
  /// The group's information for the new epoch, signed by this device (ratchet tree and external
  /// public key included): a new device of a member joins with it ([`ConversationGroup::join_external`]).
  pub maybe_group_info: Option<Vec<u8>>,
}

/// Result of processing an incoming MLS message.
#[derive(Debug)]
pub enum GroupEvent {
  /// Another member distributed content keys. Apply them with [`ContentKeyBundle::apply_to`].
  ContentKeys {
    sender: GroupMember,
    bundle: ContentKeyBundle,
  },
  /// A member sealed content keys to one device ([`SealedContentKeys`]); only that device opens
  /// them, the others ignore the event.
  SealedContentKeys {
    sender: GroupMember,
    sealed: SealedContentKeys,
  },
  /// An authorized commit was merged; the group is in a new epoch.
  MembershipChanged(MembershipUpdate),
}

/// Members added and removed by a merged commit.
#[derive(Debug, Default)]
pub struct MembershipUpdate {
  /// The signed change that authorized the commit; `None` for additions of devices of existing
  /// members, which only need the user's cross-signature, and for rejoins.
  pub maybe_change: Option<MembershipChange>,
  /// Who signed `maybe_change`, verified with it; `None` for this device's own commits.
  pub maybe_signer: Option<UserId>,
  pub added: Vec<GroupMember>,
  /// Includes a rejoining device's old leaf.
  pub removed: Vec<GroupMember>,
  /// A device that asked to be added back and was ([`ConversationGroup::commit_rejoin`]): it is in
  /// both `removed` (its old leaf) and `added`, and stays a member.
  pub maybe_rejoined: Option<GroupMember>,
  pub is_self_removed: bool,
  /// The member whose commit it was, from its credential; `None` for this device's own commits.
  pub maybe_committer: Option<GroupMember>,
  /// A move or rename of the channel (e2ee.md §4.6.5) that the commit made, checked: the anchor
  /// now carries it. Raise the anchor floor with [`ConversationGroup::verified_anchor`].
  pub maybe_properties_change: Option<Box<SignedPropertiesChange>>,
  /// A strict channel's roles change (e2ee.md §4.6.2) that another member's commit made, checked:
  /// the context now carries its roles, or none if it turned strict off. `None` for this device's
  /// own commits: read [`ConversationGroup::strict_roles`] after confirming.
  pub maybe_strict_roles_change: Option<Box<SignedStrictRolesChange>>,
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
    Self::create_with(
      provider,
      device,
      certificate,
      conversation_id,
      Extensions::empty(),
    )
  }

  /// Creates the group for a new channel or meeting chat with its signed anchor in the group
  /// context of epoch 0 (e2ee.md §4.6.5, "Founding"), required of every leaf: an app that doesn't
  /// announce the anchor can't be added or open a welcome. The anchor must be for this
  /// conversation; the caller verifies it like every other ([`Self::verified_anchor`]).
  pub fn create_anchored(
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    conversation_id: ConversationId,
    anchor: &ChannelAnchor,
  ) -> Result<Self, MlsError> {
    if anchor.founding().conversation_id != conversation_id {
      return Err(MlsError::WrongConversation {
        expected: conversation_id,
        actual: anchor.founding().conversation_id,
      });
    }
    let extensions = Extensions::from_vec(vec![
      Extension::Unknown(
        CHANNEL_ANCHOR_EXTENSION_TYPE,
        UnknownExtension(anchor.to_bytes()?),
      ),
      Extension::RequiredCapabilities(RequiredCapabilitiesExtension::new(
        &[ExtensionType::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE)],
        &[],
        &[],
      )),
    ])
    .map_err(|error| MlsError::openmls("anchor extension", error))?;
    Self::create_with(provider, device, certificate, conversation_id, extensions)
  }

  fn create_with(
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    conversation_id: ConversationId,
    extensions: Extensions<GroupContext>,
  ) -> Result<Self, MlsError> {
    let group = MlsGroup::builder()
      .with_group_id(group_id(conversation_id))
      .ciphersuite(CIPHERSUITE)
      .use_ratchet_tree_extension(true)
      .with_capabilities(leaf_capabilities(&[]))
      .with_group_context_extensions(extensions)
      .build(provider, device, credential::credential_with_key(certificate)?)
      .map_err(|error| MlsError::openmls("group creation", error))?;
    Ok(Self::from_group(conversation_id, group, ChangeHistory::default()))
  }

  /// Founds a conversation's group anew, with this device as the only member, when no device is
  /// left that could add it to the old one (e2ee.md §4.6, "Re-founding the notes"). The new group
  /// is advanced on this device alone, with empty commits nobody else sees, to `epoch`, the epoch
  /// the server's queue for the conversation is in. Its first real commit, returned here and
  /// pending until the server accepts it ([`Self::confirm_commit`]), then takes the queue over
  /// like any commit: first commit per epoch wins, so a device of the old group that is still
  /// alive and commits first makes the server refuse it. Old devices can't read the new group; one
  /// that comes back falls out and asks to be added back like any device that missed a commit.
  pub fn refound(
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    conversation_id: ConversationId,
    epoch: u64,
  ) -> Result<(Self, CommitOutput), MlsError> {
    let mut founded = Self::create(provider, device, certificate, conversation_id)?;
    while founded.epoch() < epoch {
      // Through the one commit path, so the web bundle carries no second copy of it.
      founded.stage_commit(provider, device, Vec::new(), Vec::new(), true)?;
      founded
        .group
        .merge_pending_commit(provider)
        .map_err(|error| MlsError::openmls("merging own commit", error))?;
    }
    founded.history.joined_epoch = epoch;
    let output = founded.stage_commit(provider, device, Vec::new(), Vec::new(), true)?;
    founded.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
      maybe_properties_change: None,
    });
    Ok((founded, output))
  }

  /// Joins a group from a welcome. Every member credential in the group is verified against the
  /// pinned identities before the group is accepted.
  pub fn join(
    provider: &impl OpenMlsProvider,
    welcome: &[u8],
    conversation_id: ConversationId,
    directory: &impl IdentityDirectory,
  ) -> Result<Self, MlsError> {
    Self::stage_join(provider, welcome, conversation_id)?.complete(provider, directory)
  }

  /// Decrypts a welcome without trusting it yet: [`StagedJoin::claimed_users`] says whose
  /// identities to pin before [`StagedJoin::complete`] verifies every leaf. The welcome's tree may
  /// hold users the server no longer lists (removed after the welcome was made). This takes the
  /// device's key package, so a failed join can't be retried with the same welcome.
  pub fn stage_join(
    provider: &impl OpenMlsProvider,
    welcome: &[u8],
    conversation_id: ConversationId,
  ) -> Result<StagedJoin, MlsError> {
    Self::stage_join_inner(provider, welcome, conversation_id, false)
  }

  /// Like [`Self::stage_join`] while this device still holds a group for the conversation (one it
  /// founded that lost the founding race). That group is left as it is until the caller
  /// [`StagedJoin::verify`]s the welcome and decides: [`StagedJoin::complete`] then replaces the
  /// stored state of the old group (same group ID; the old [`ConversationGroup`] must not be
  /// used or [`Self::delete`]d afterwards), while dropping the staged join keeps it.
  pub fn stage_join_replacing(
    provider: &impl OpenMlsProvider,
    welcome: &[u8],
    conversation_id: ConversationId,
  ) -> Result<StagedJoin, MlsError> {
    Self::stage_join_inner(provider, welcome, conversation_id, true)
  }

  fn stage_join_inner(
    provider: &impl OpenMlsProvider,
    welcome: &[u8],
    conversation_id: ConversationId,
    is_replacing: bool,
  ) -> Result<StagedJoin, MlsError> {
    let MlsMessageBodyIn::Welcome(welcome) = MlsMessageIn::tls_deserialize_exact(welcome)?.extract() else {
      return Err(MlsError::UnexpectedMessage("expected a welcome"));
    };
    let config = MlsGroupJoinConfig::builder()
      .use_ratchet_tree_extension(true)
      .build();
    // Leaves that never committed still carry the lifetime of the key package they joined with;
    // checking it would make every group unjoinable once such a member's package expired. A
    // lifetime only bounds when a package may be *added* (RFC 9420 §7.3, openmls #1810), which
    // the committer and every member checked then; credentials are verified on completion.
    let mut builder = StagedWelcome::build_from_welcome(provider, &config, welcome)
      .map_err(|error| match error {
        WelcomeError::NoMatchingKeyPackage => MlsError::NoMatchingKeyPackage,
        other => MlsError::openmls("welcome processing", other),
      })?
      .skip_lifetime_validation();
    if is_replacing {
      // Only lifts openmls' check that no group with this ID is stored; nothing is written
      // until the join completes.
      builder = builder.replace_old_group();
    }
    let staged = builder
      .build()
      .map_err(|error| MlsError::openmls("welcome processing", error))?;
    if staged.group_context().group_id() != &group_id(conversation_id) {
      return Err(MlsError::UnexpectedMessage("welcome for another conversation"));
    }
    if staged.group_context().ciphersuite() != CIPHERSUITE {
      return Err(MlsError::UnexpectedMessage("welcome with another ciphersuite"));
    }
    Ok(StagedJoin {
      conversation_id,
      staged,
    })
  }

  /// Loads a group from the provider's storage, with the change IDs applied so far and the change
  /// history (persisted by the caller next to the MLS state). `None` if the provider has no state
  /// for the conversation.
  /// `maybe_pending` is what [`Self::pending_commit`] returned before; it is dropped if the
  /// provider's state has no pending commit.
  pub fn load(
    provider: &impl OpenMlsProvider,
    conversation_id: ConversationId,
    applied_changes: impl IntoIterator<Item = ChangeId>,
    history: ChangeHistory,
    maybe_pending: Option<PendingCommit>,
  ) -> Result<Option<Self>, MlsError> {
    let maybe_group = MlsGroup::load(provider.storage(), &group_id(conversation_id))
      .map_err(|error| MlsError::openmls("loading group", error))?;
    Ok(maybe_group.map(|group| {
      let mut loaded = Self::from_group(conversation_id, group, history);
      loaded.applied_changes.extend(applied_changes);
      if loaded.group.pending_commit().is_some() {
        loaded.maybe_pending = maybe_pending;
      }
      loaded
    }))
  }

  /// The state of the conversation's stored group, `None` without one: one small storage entry
  /// instead of the whole group, for checks over many conversations (issue #714). The provider
  /// writes a group's entries together, so a stored state means [`Self::load`] finds the group.
  pub fn stored_state(
    provider: &impl OpenMlsProvider,
    conversation_id: ConversationId,
  ) -> Result<Option<StoredGroupState>, MlsError> {
    let maybe_state: Option<MlsGroupState> = provider
      .storage()
      .group_state(&group_id(conversation_id))
      .map_err(|error| MlsError::openmls("reading the group state", error))?;
    Ok(maybe_state.map(|state| match state {
      MlsGroupState::PendingCommit(_) => StoredGroupState::PendingCommit,
      MlsGroupState::Operational => StoredGroupState::Operational,
      MlsGroupState::Inactive => StoredGroupState::Inactive,
    }))
  }

  /// Joins the group of a conversation whose user already has a device in it, from the group's
  /// information, with an MLS external commit (e2ee.md §4.6). Every leaf of the group is verified
  /// against the pinned identities, as when joining from a welcome; this device must not be in the
  /// group yet and its user must be. `min_epoch` is the newest epoch this user's devices saw the
  /// content key switch in: the signed epoch of the group information must not be older (a server
  /// could hand out an old one whose tree still holds a removed member). openmls merges an external
  /// commit at once, so the group is in the new epoch already: if the server doesn't take the
  /// commit, [`Self::delete`] the group and try again.
  pub fn join_external(
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    certificate: &SignedDeviceCertificate,
    group_info: &[u8],
    conversation_id: ConversationId,
    min_epoch: u64,
    directory: &impl IdentityDirectory,
  ) -> Result<(Self, CommitOutput), MlsError> {
    let MlsMessageBodyIn::GroupInfo(info) = MlsMessageIn::tls_deserialize_exact(group_info)?.extract() else {
      return Err(MlsError::UnexpectedMessage("expected a group info"));
    };
    if info.group_id() != &group_id(conversation_id) {
      return Err(MlsError::UnexpectedMessage("group info for another conversation"));
    }
    if info.ciphersuite() != CIPHERSUITE {
      return Err(MlsError::UnexpectedMessage("group info with another ciphersuite"));
    }
    // Before the commit is built: openmls would remove this device's old leaf in it, a resync the
    // members refuse (`authorize_external_join`), and the device would be out of step with them.
    let credential_with_key = credential::credential_with_key(certificate)?;
    if holds_signature_key(&info, credential_with_key.signature_key.as_slice()) {
      return Err(MlsError::AlreadyInGroup);
    }
    let config = MlsGroupJoinConfig::builder()
      .use_ratchet_tree_extension(true)
      .build();
    // Lifetimes of existing leaves are not checked, as when joining from a welcome (§4.3.1).
    let (group, bundle) = MlsGroup::external_commit_builder()
      .with_config(config)
      .skip_lifetime_validation()
      .build_group(provider, info, credential_with_key)
      .map_err(|error| MlsError::openmls("external join", error))?
      .leaf_node_parameters(
        LeafNodeParameters::builder()
          .with_capabilities(leaf_capabilities(&[]))
          .build(),
      )
      .load_psks(provider.storage())
      .map_err(|error| MlsError::openmls("external join", error))?
      .build(provider.rand(), provider.crypto(), device, |_| true)
      .map_err(|error| MlsError::openmls("external join", error))?
      .finalize(provider)
      .map_err(|error| MlsError::openmls("external join", error))?;
    let history = ChangeHistory {
      joined_epoch: group.epoch().as_u64(),
      ..ChangeHistory::default()
    };
    let joined = Self::from_group(conversation_id, group, history);
    // The epoch from the signed group context (openmls checked the group info's signature and the
    // tree against it); the joiner is one past it.
    let info_epoch = joined.epoch().saturating_sub(1);
    let checked = if info_epoch < min_epoch {
      Err(MlsError::StaleGroupInfo {
        epoch: info_epoch,
        floor: min_epoch,
      })
    } else {
      joined.check_external_join(certificate, directory)
    };
    if let Err(error) = checked {
      // Nothing was sent yet: forget the group again.
      joined.delete(provider)?;
      return Err(error);
    }
    let maybe_welcome = bundle
      .to_welcome_msg()
      .map(|welcome| welcome.to_bytes())
      .transpose()
      .map_err(|error| MlsError::openmls("welcome encoding", error))?;
    let maybe_group_info = bundle
      .group_info()
      .cloned()
      .map(|info| MlsMessageOut::from(info).to_bytes())
      .transpose()
      .map_err(|error| MlsError::openmls("group info encoding", error))?;
    let commit = bundle
      .into_commit()
      .to_bytes()
      .map_err(|error| MlsError::openmls("commit encoding", error))?;
    Ok((
      joined,
      CommitOutput {
        commit,
        maybe_welcome,
        maybe_group_info,
      },
    ))
  }

  /// The group's information for its current epoch, signed by this device, for a new device of a
  /// member to join with when none is on the server yet.
  pub fn export_group_info(
    &self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
  ) -> Result<Vec<u8>, MlsError> {
    self
      .group
      .export_group_info(provider.crypto(), device, true)
      .map_err(|error| MlsError::openmls("group info export", error))?
      .to_bytes()
      .map_err(|error| MlsError::openmls("group info encoding", error))
  }

  pub fn conversation_id(&self) -> ConversationId {
    self.conversation_id
  }

  /// The anchor extension's bytes in the current group context, unparsed; `None` for a group
  /// without one (a DM, a channel from before anchors).
  pub fn anchor_bytes(&self) -> Option<&[u8]> {
    anchor_bytes_of(self.group.extensions())
  }

  /// Verifies the group's anchor (checks 1 to 5, e2ee.md §4.6.5) against the tree and epoch this
  /// device holds; `Ok(None)` for a group without one. The pin and the floor are the caller's.
  pub fn verified_anchor(
    &self,
    expected: &AnchorExpectation,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<Option<VerifiedAnchor>, MlsError> {
    let Some(bytes) = self.anchor_bytes() else {
      return Ok(None);
    };
    let anchor = ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES)?;
    let tree_users = self.members()?.into_iter().map(|member| member.user_id).collect();
    Ok(Some(anchor.verify(
      expected,
      self.epoch(),
      &tree_users,
      directory,
      authority,
    )?))
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
    // openmls deletes the leaf keys of the current epoch only. A commit that removed this device
    // moved the group to the next epoch without deleting the previous epoch's (openmls 0.9), so
    // its private keys would stay in the storage for good (issue #424).
    if let Some(previous) = self.group.epoch().as_u64().checked_sub(1) {
      provider
        .storage()
        .delete_encryption_epoch_key_pairs(
          self.group.group_id(),
          &GroupEpoch::from(previous),
          self.group.own_leaf_index().u32(),
        )
        .map_err(|error| MlsError::openmls("deleting group", error))?;
    }
    self
      .group
      .delete(provider.storage())
      .map_err(|error| MlsError::openmls("deleting group", error))
  }

  pub fn has_pending_commit(&self) -> bool {
    self.group.pending_commit().is_some()
  }

  /// The pending commit and its effect; persist it with the MLS state.
  pub fn pending_commit(&self) -> Option<&PendingCommit> {
    self
      .maybe_pending
      .as_ref()
      .filter(|_| self.group.pending_commit().is_some())
  }

  pub fn epoch(&self) -> u64 {
    self.group.epoch().as_u64()
  }

  /// The epoch this device joined the group in (by welcome or external commit); persisted with
  /// the change history.
  pub fn joined_epoch(&self) -> u64 {
    self.history.joined_epoch
  }

  /// The content key of the current epoch, derived from the MLS exporter (RFC 9420 §8.5). Every
  /// member that merged the epoch's commit derives the same key, and nobody else can: after a
  /// removal it becomes the current key at once, so the new key can't be withheld from members and
  /// the removed device never learns it (security review A5).
  pub fn epoch_content_key(&self, provider: &impl OpenMlsProvider) -> Result<ContentKey, MlsError> {
    let mut context = self.conversation_id.get().to_be_bytes().to_vec();
    context.extend_from_slice(&self.epoch().to_be_bytes());
    let exported = Zeroizing::new(
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

  /// One member device with its certificate verified against the pinned identity; `None` if it
  /// isn't in the group. Other members' leaves aren't looked at, so one bad leaf doesn't keep
  /// keys from reaching the device.
  pub fn verified_member(
    &self,
    member: GroupMember,
    directory: &impl IdentityDirectory,
  ) -> Result<Option<VerifiedDevice>, MlsError> {
    for leaf in self.group.members() {
      if credential::member_of(&leaf.credential)? == member {
        return credential::verify(&leaf.credential, &leaf.signature_key, directory).map(Some);
      }
    }
    Ok(None)
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
    // Honest clients never commit what they would refuse (e2ee.md §4.6.2).
    let change = &self.authorize_signed_change(signed_change, directory, authority)?;
    let mut validated_key_packages = Vec::with_capacity(key_packages.len());
    let mut proposed = ProposedMembership::default();
    for bytes in key_packages {
      let (key_package, added_device) = validate_key_package(provider, bytes, directory)?;
      self.ensure_capable(&key_package, &added_device)?;
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
    let result = self.stage_commit(provider, device, validated_key_packages, removed_leaves, false);
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: Some(change.clone()),
      removed: proposed.removed,
      maybe_rejoin: None,
      maybe_properties_change: None,
    });
    Ok(output)
  }

  /// Commits a move or rename of the channel (e2ee.md §4.6.5, "Moves and renames"): one group
  /// context extensions proposal that replaces the anchor's last change and keeps every other
  /// extension byte for byte (also those this version doesn't know), with the signed change in the
  /// authenticated data. Checked as the members will check it first. Pending until
  /// [`Self::confirm_commit`].
  pub fn commit_properties_change(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    signed: &SignedPropertiesChange,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<CommitOutput, MlsError> {
    let current = self.current_anchor()?;
    let tree_users = self.members()?.into_iter().map(|member| member.user_id).collect();
    current.check_change(signed, self.epoch(), &tree_users, directory, authority)?;
    let changed = current.with_change(signed.clone()).to_bytes()?;
    let extensions = with_anchor(self.group.extensions(), changed)?;
    let mut aad = PROPERTIES_CHANGE_AAD_MAGIC.to_vec();
    aad.extend_from_slice(&signed.to_bytes()?);
    self.group.set_aad(aad);
    let result = self.stage_context_commit(provider, device, extensions);
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
      maybe_properties_change: Some(signed.clone()),
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
    let (key_package, device) = validate_key_package(provider, bytes, directory)?;
    self.ensure_capable(&key_package, &device)?;
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
      self.ensure_capable(&key_package, &added_device)?;
      validated_key_packages.push(key_package);
      proposed.added.push(added_device);
    }
    self.check_device_additions(&proposed)?;
    self.check_not_removed(&proposed)?;
    let output = self.stage_commit(provider, device, validated_key_packages, Vec::new(), false)?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
      maybe_properties_change: None,
    });
    Ok(output)
  }

  /// Checks a device's request to be added back ([`super::rejoin`]): validly signed by that
  /// device, which is a member here with the same signing key, for this group, fresh and not
  /// answered before. Returns the device.
  pub fn check_rejoin_request(
    &self,
    signed: &SignedRejoinRequest,
    directory: &impl IdentityDirectory,
  ) -> Result<GroupMember, MlsError> {
    let verified = signed.verify(directory)?;
    self.check_verified_rejoin(&verified)?;
    Ok(verified.request().device())
  }

  /// Answers a device's rejoin request: one commit removes its old leaf and adds it from a fresh
  /// key package, carrying the request as authenticated data. Membership stays the same.
  pub fn commit_rejoin(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    signed: &SignedRejoinRequest,
    key_package: &[u8],
    directory: &impl IdentityDirectory,
  ) -> Result<CommitOutput, MlsError> {
    let verified = signed.verify(directory)?;
    let (old_leaf, member) = self.check_verified_rejoin(&verified)?;
    let (key_package, added) = validate_key_package(provider, key_package, directory)?;
    self.ensure_capable(&key_package, &added)?;
    if GroupMember::from(&added) != member {
      return Err(MlsError::ChangeDoesNotMatchCommit);
    }
    let mut aad = REJOIN_AAD_MAGIC.to_vec();
    aad.extend_from_slice(&signed.to_bytes()?);
    self.group.set_aad(aad);
    let result = self.stage_commit(
      provider,
      device,
      vec![key_package],
      vec![(old_leaf, member)],
      false,
    );
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: vec![member],
      maybe_rejoin: Some((member, verified.request().request_id())),
      maybe_properties_change: None,
    });
    Ok(output)
  }

  /// Merges the pending commit after the server accepted it (or it showed up in the group's
  /// queue, [`PendingCommit::is_commit`]). Returns what it changed.
  pub fn confirm_commit(&mut self, provider: &impl OpenMlsProvider) -> Result<MembershipUpdate, MlsError> {
    if self.group.pending_commit().is_none() {
      return Err(MlsError::NoPendingCommit);
    }
    let epoch = self.epoch();
    self
      .group
      .merge_pending_commit(provider)
      .map_err(|error| MlsError::openmls("merging own commit", error))?;
    let Some(pending) = self.maybe_pending.take() else {
      // Invariant: every commit function records its effect; a commit staged by an older version
      // only lacks the bookkeeping.
      return Ok(MembershipUpdate::default());
    };
    self.record_commit(pending.maybe_change.as_ref(), &pending.removed, epoch);
    if let Some((_, request_id)) = pending.maybe_rejoin {
      self.applied_changes.insert(request_id);
    }
    Ok(MembershipUpdate {
      maybe_change: pending.maybe_change,
      maybe_signer: None,
      added: pending.maybe_rejoin.iter().map(|(member, _)| *member).collect(),
      removed: pending.removed,
      maybe_rejoined: pending.maybe_rejoin.map(|(member, _)| member),
      is_self_removed: false,
      maybe_committer: None,
      maybe_properties_change: pending.maybe_properties_change.map(Box::new),
      maybe_strict_roles_change: None,
    })
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

  /// Sends content keys to one member device only, sealed to the agreement key its verified
  /// certificate names (e2ee.md §4.6): the other members see that keys went to it, not which.
  pub fn send_sealed_content_keys(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    recipient: &VerifiedDevice,
    bundle: &ContentKeyBundle,
  ) -> Result<Vec<u8>, MlsError> {
    self.ensure_conversation(bundle.conversation_id())?;
    if !self.members()?.contains(&GroupMember::from(recipient)) {
      return Err(MlsError::UnexpectedMessage(
        "sealed content keys for a device outside the group",
      ));
    }
    let sender = GroupMember {
      user_id: device.user_id(),
      device_id: device.public_keys().device_id,
    };
    let payload = Zeroizing::new(bundle.seal_for(recipient, sender, self.epoch())?);
    let message = self
      .group
      .create_message(provider, device, &payload)
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
    // Only members send, except a new device of a member joining by external commit.
    let maybe_sender_index = match *processed.sender() {
      Sender::Member(index) => Some(index),
      Sender::NewMemberCommit => None,
      _ => return Err(MlsError::ExternalSender),
    };
    let sender = credential::member_of(processed.credential())?;
    let authenticated_data = processed.aad().to_vec();
    let message_epoch = processed.epoch().as_u64();
    match processed.into_content() {
      ProcessedMessageContent::ApplicationMessage(application) => {
        if maybe_sender_index.is_none() {
          return Err(MlsError::ExternalSender);
        }
        match ApplicationPayload::from_bytes(&Zeroizing::new(application.into_bytes()))? {
          ApplicationPayload::Bundle(bundle) => {
            self.ensure_conversation(bundle.conversation_id())?;
            Ok(GroupEvent::ContentKeys { sender, bundle })
          },
          ApplicationPayload::Sealed(wire) => {
            let sealed = SealedContentKeys::received(wire, sender, message_epoch);
            self.ensure_conversation(sealed.conversation_id())?;
            Ok(GroupEvent::SealedContentKeys { sender, sealed })
          },
        }
      },
      ProcessedMessageContent::StagedCommitMessage(staged) => {
        let (mut update, maybe_request_id) = match maybe_sender_index {
          Some(sender_index) => {
            self.authorize_commit(&staged, sender_index, &authenticated_data, directory, authority)?
          },
          None => (
            self.authorize_external_join(&staged, &authenticated_data, directory)?,
            None,
          ),
        };
        update.maybe_committer = Some(sender);
        let epoch = self.epoch();
        self
          .group
          .merge_staged_commit(provider, *staged)
          .map_err(|error| MlsError::openmls("merging commit", error))?;
        self.record_commit(update.maybe_change.as_ref(), &update.removed, epoch);
        if let Some(request_id) = maybe_request_id {
          self.applied_changes.insert(request_id);
        }
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
    is_self_update: bool,
  ) -> Result<CommitOutput, MlsError> {
    let bundle = self
      .group
      .commit_builder()
      .force_self_update(is_self_update)
      .propose_adds(key_packages)
      .propose_removals(removed_leaves.into_iter().map(|(index, _)| index))
      .load_psks(provider.storage())
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .build(provider.rand(), provider.crypto(), device, |_| true)
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .stage_commit(provider)
      .map_err(|error| MlsError::openmls("commit staging", error))?;
    commit_output(bundle)
  }

  /// TeamCraft's rules for accepting another member's commit (e2ee.md §4.6, admin.md §2.3). Also
  /// returns the ID of the rejoin request a rejoin commit answers.
  fn authorize_commit(
    &self,
    staged: &StagedCommit,
    sender_index: LeafNodeIndex,
    authenticated_data: &[u8],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<(MembershipUpdate, Option<ChangeId>), MlsError> {
    if let Some(signed) = authenticated_data.strip_prefix(STRICT_ROLES_AAD_MAGIC) {
      let update = self.authorize_strict_roles_change(staged, sender_index, signed, directory, authority)?;
      return Ok((update, None));
    }
    if let Some(signed) = authenticated_data.strip_prefix(PROPERTIES_CHANGE_AAD_MAGIC) {
      let update = self.authorize_properties_change(staged, sender_index, signed, directory, authority)?;
      return Ok((update, None));
    }
    // In a strict channel the server's word about guests decides nothing: who signed matters, not
    // who committed, and guests hold no signed role (e2ee.md §4.6.2).
    let is_strict = self.is_strict();
    let proposed = self.verify_commit_contents(staged, sender_index, directory)?;
    if let Some(request) = authenticated_data.strip_prefix(REJOIN_AAD_MAGIC) {
      if !is_strict {
        self.check_guest_commit(sender_index, &proposed, None, directory)?;
      }
      let verified = SignedRejoinRequest::from_bytes(request)?.verify(directory)?;
      let (_, member) = self.check_verified_rejoin(&verified)?;
      let added: Vec<GroupMember> = proposed.added.iter().map(GroupMember::from).collect();
      if added != [member] || proposed.removed != [member] {
        return Err(MlsError::ChangeDoesNotMatchCommit);
      }
      let update = MembershipUpdate {
        maybe_change: None,
        maybe_signer: None,
        added,
        removed: proposed.removed,
        maybe_rejoined: Some(member),
        is_self_removed: staged.self_removed(),
        maybe_committer: None,
        maybe_properties_change: None,
        maybe_strict_roles_change: None,
      };
      return Ok((update, Some(verified.request().request_id())));
    }
    let (maybe_change, maybe_signer) = if authenticated_data.is_empty() {
      if !proposed.removed.is_empty() {
        return Err(MlsError::MissingSignedChange);
      }
      // No addition either: a leaf refresh, the committer's own leaf and nobody's membership
      // (e2ee.md §4.6.2, "Turning it on", step 3). Its path leaf keeps its identity (checked).
      if !proposed.added.is_empty() {
        if !is_strict {
          self.check_guest_commit(sender_index, &proposed, None, directory)?;
        }
        self.check_device_additions(&proposed)?;
      }
      (None, None)
    } else {
      let signed = SignedMembershipChange::from_bytes(authenticated_data)?;
      let change = self.authorize_signed_change(&signed, directory, authority)?;
      self.check_change_matches(&change, &proposed)?;
      let actor = signed.signer_user();
      if !is_strict {
        let maybe_self_leave = match change.action() {
          MembershipAction::RemoveUser { user_id } if *user_id == actor => Some(actor),
          _ => None,
        };
        self.check_guest_commit(sender_index, &proposed, maybe_self_leave, directory)?;
      }
      (Some(change), Some(actor))
    };
    let update = MembershipUpdate {
      maybe_change,
      maybe_signer,
      added: proposed.added.iter().map(GroupMember::from).collect(),
      removed: proposed.removed,
      maybe_rejoined: None,
      is_self_removed: staged.self_removed(),
      maybe_committer: None,
      maybe_properties_change: None,
      maybe_strict_roles_change: None,
    };
    Ok((update, None))
  }

  /// A move or rename (e2ee.md §4.6.5): the commit carries one group context extensions proposal
  /// and nothing else (its path leaf keeps its identity), the signed change passes
  /// [`ChannelAnchor::check_change`], and the new context is the current one with exactly that
  /// change as the anchor's last change: the founding and every other extension byte for byte.
  fn authorize_properties_change(
    &self,
    staged: &StagedCommit,
    sender_index: LeafNodeIndex,
    signed: &[u8],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<MembershipUpdate, MlsError> {
    let mut proposals = 0usize;
    for queued in staged.queued_proposals() {
      if !matches!(queued.proposal(), Proposal::GroupContextExtensions(_)) {
        return Err(MlsError::UnsupportedProposal);
      }
      if !matches!(queued.sender(), Sender::Member(_)) {
        return Err(MlsError::ExternalSender);
      }
      proposals += 1;
    }
    if proposals != 1 {
      return Err(MlsError::ContextChangeMismatch);
    }
    // A guest manages nothing in a group (§4.6, "Meeting guests"), not even by committing
    // somebody else's signed change.
    let committer = self
      .group
      .member(sender_index)
      .ok_or(MlsError::ChangeDoesNotMatchCommit)?;
    if directory.is_guest(credential::member_of(committer)?.user_id) {
      return Err(MlsError::GuestOverreach);
    }
    if let Some(leaf) = staged.update_path_leaf_node() {
      self.verify_same_identity(sender_index, leaf, directory)?;
    }
    let signed = SignedPropertiesChange::from_bytes(signed)?;
    let current = self.current_anchor()?;
    let tree_users = self.members()?.into_iter().map(|member| member.user_id).collect();
    current.check_change(&signed, self.epoch(), &tree_users, directory, authority)?;
    let expected = with_anchor(
      self.group.extensions(),
      current.with_change(signed.clone()).to_bytes()?,
    )?;
    if staged.group_context().extensions() != &expected {
      return Err(MlsError::ContextChangeMismatch);
    }
    Ok(MembershipUpdate {
      maybe_properties_change: Some(Box::new(signed)),
      ..MembershipUpdate::default()
    })
  }

  /// The anchor of the current context, parsed; a group without one has nothing to change.
  fn current_anchor(&self) -> Result<ChannelAnchor, MlsError> {
    let bytes = self.anchor_bytes().ok_or(MlsError::NotAnchored)?;
    Ok(ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES)?)
  }

  /// Verifies a signed membership change and judges its signer: in a strict channel by the
  /// context's roles (e2ee.md §4.6.2, "Who may do what"), otherwise by `authority` (§4.6).
  /// Returns the change; its signer is [`SignedMembershipChange::signer_user`], now verified.
  fn authorize_signed_change(
    &self,
    signed: &SignedMembershipChange,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<MembershipChange, MlsError> {
    let Some(strict) = self.strict_roles()? else {
      return Ok(signed.verify(directory, authority)?.change().clone());
    };
    let change = signed.unverified();
    let action = change.action();
    let signer = signed.signer_user();
    let maybe_target_role = strict.role_of(action.target_user()).map(|role| role.role);
    // A role counts only with a leaf, and under the identity key the context names.
    if let Some(role) = strict.role_of(signer)
      && let Ok(device) = signed.verify_signed_by(&role.identity_key)
    {
      let class = strict.signer_class(&device, &self.tree_leaves()?);
      if membership_verdict(signer, class, action, maybe_target_role) {
        return Ok(change.clone());
      }
    }
    // Leaving, and workspace signers: under the signer's pinned identity key.
    let pinned = directory
      .pinned_identity(signer)
      .ok_or(MembershipError::UnknownSigner(signer))?;
    signed.verify_signed_by(&pinned)?;
    if membership_verdict(signer, StrictSigner::Other, action, maybe_target_role) {
      return Ok(change.clone());
    }
    // Only now the roster: at most one read, for a signer the context doesn't allow.
    match authority.workspace_signer(signer, 0, Judged::Newest) {
      Standing::Confirmed if authority.is_authorized(signer, change.conversation_id(), action) => {
        Ok(change.clone())
      },
      Standing::Unknown => Err(MlsError::StrictRosterNeeded(signer)),
      Standing::Confirmed | Standing::Refused => Err(MlsError::StrictRefused {
        signer,
        action: *action,
      }),
    }
  }

  /// Whether the channel is strict: its context carries the roles extension (e2ee.md §4.6.2).
  /// Nothing else switches it.
  pub fn is_strict(&self) -> bool {
    strict_bytes_of(self.group.extensions()).is_some()
  }

  /// The strict roles of the current context; `None` for a channel that isn't strict. They were
  /// verified when the commit that set them was merged; for a group this device joined, the
  /// welcome's sender was checked against what it knew ([`StagedJoin::check_strict_sender`]).
  pub fn strict_roles(&self) -> Result<Option<StrictRoles>, MlsError> {
    strict_bytes_of(self.group.extensions())
      .map(StrictRoles::from_bytes)
      .transpose()
      .map_err(MlsError::from)
  }

  /// Every leaf of the tree with its certificate and signature key, unverified.
  fn tree_leaves(&self) -> Result<Vec<TreeLeaf>, MlsError> {
    self
      .group
      .members()
      .map(|member| {
        Ok(TreeLeaf {
          certificate: credential::decode(&member.credential)?,
          signature_key: member.signature_key,
        })
      })
      .collect()
  }

  /// Turns the channel strict, changes its roles or turns it off (e2ee.md §4.6.2): one group
  /// context extensions proposal that sets the roles extension and requires it of every leaf,
  /// every other extension byte for byte, with the signed change in the authenticated data.
  /// Checked as the members will check it first. Pending until [`Self::confirm_commit`].
  ///
  /// Turning it on fails while a leaf doesn't announce the capability
  /// ([`Self::leaves_without_strict`]): every device has to have refreshed its leaf first
  /// ([`Self::commit_leaf_refresh`]).
  pub fn commit_strict_roles(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    signed: &SignedStrictRolesChange,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<CommitOutput, MlsError> {
    let verdict = self.judge_strict_roles_change(signed, directory, authority)?;
    let extensions = with_strict(self.group.extensions(), &verdict)?;
    let mut aad = STRICT_ROLES_AAD_MAGIC.to_vec();
    aad.extend_from_slice(&signed.to_bytes()?);
    self.group.set_aad(aad);
    let result = self.stage_context_commit(provider, device, extensions);
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
      maybe_properties_change: None,
    });
    Ok(output)
  }

  /// Refreshes this device's leaf with the capabilities this version announces, in an otherwise
  /// empty commit (e2ee.md §4.6.2, "Turning it on", step 3). Apps from before #1197 refuse an
  /// empty commit, so it is only sent once every device of the group answered "ready".
  pub fn commit_leaf_refresh(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
  ) -> Result<CommitOutput, MlsError> {
    let parameters = LeafNodeParameters::builder()
      .with_capabilities(leaf_capabilities(&[]))
      .build();
    let bundle = self
      .group
      .commit_builder()
      .force_self_update(true)
      .leaf_node_parameters(parameters)
      .load_psks(provider.storage())
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .build(provider.rand(), provider.crypto(), device, |_| true)
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .stage_commit(provider)
      .map_err(|error| MlsError::openmls("commit staging", error))?;
    let output = commit_output(bundle)?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
      maybe_properties_change: None,
    });
    Ok(output)
  }

  /// The devices whose leaf doesn't announce strict channels: while there is one, the channel
  /// can't be turned strict (e2ee.md §4.6.2, "Turning it on", D6).
  pub fn leaves_without_strict(&self) -> Result<Vec<GroupMember>, MlsError> {
    let strict_type = ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE);
    let mut lacking = Vec::new();
    for member in self.group.members() {
      let leaf = self
        .group
        .public_group()
        .leaf(member.index)
        .ok_or(MlsError::ChangeDoesNotMatchCommit)?;
      if !leaf.capabilities().extensions().contains(&strict_type) {
        lacking.push(credential::member_of(&member.credential)?);
      }
    }
    Ok(lacking)
  }

  /// The verdict on a roles change for this group as it stands: a channel's group (never a
  /// meeting chat's), and [`StrictRoles::check_change`].
  fn judge_strict_roles_change(
    &self,
    signed: &SignedStrictRolesChange,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<StrictRolesVerdict, MlsError> {
    let maybe_current = self.strict_roles()?;
    if maybe_current.is_none()
      && let Some(bytes) = self.anchor_bytes()
    {
      // Only a meeting chat's anchor keeps strict out; DMs and the notes have no anchor and are
      // the app's to keep out.
      let is_meeting_chat = ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES)
        .is_ok_and(|anchor| matches!(anchor.founding().kind, AnchorKind::MeetingChat { .. }));
      if is_meeting_chat {
        return Err(StrictRolesError::NotAChannel.into());
      }
    }
    Ok(StrictRoles::check_change(
      maybe_current.as_ref(),
      signed,
      self.conversation_id,
      self.epoch(),
      &self.tree_leaves()?,
      directory,
      authority,
    )?)
  }

  /// A strict roles commit: one group context extensions proposal and nothing else (its path
  /// leaf keeps its identity), the signed change passes [`StrictRoles::check_change`], and the
  /// new context is exactly [`with_strict`] of the current one.
  fn authorize_strict_roles_change(
    &self,
    staged: &StagedCommit,
    sender_index: LeafNodeIndex,
    signed: &[u8],
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<MembershipUpdate, MlsError> {
    let mut proposals = 0usize;
    for queued in staged.queued_proposals() {
      if !matches!(queued.proposal(), Proposal::GroupContextExtensions(_)) {
        return Err(MlsError::UnsupportedProposal);
      }
      if !matches!(queued.sender(), Sender::Member(_)) {
        return Err(MlsError::ExternalSender);
      }
      proposals += 1;
    }
    if proposals != 1 {
      return Err(MlsError::ContextChangeMismatch);
    }
    if let Some(leaf) = staged.update_path_leaf_node() {
      self.verify_same_identity(sender_index, leaf, directory)?;
    }
    let signed = SignedStrictRolesChange::from_bytes(signed)?;
    let verdict = self.judge_strict_roles_change(&signed, directory, authority)?;
    let expected = with_strict(self.group.extensions(), &verdict)?;
    if staged.group_context().extensions() != &expected {
      return Err(MlsError::ContextChangeMismatch);
    }
    Ok(MembershipUpdate {
      maybe_strict_roles_change: Some(Box::new(signed)),
      ..MembershipUpdate::default()
    })
  }

  fn stage_context_commit(
    &mut self,
    provider: &impl OpenMlsProvider,
    device: &DeviceKeyPair,
    extensions: Extensions<GroupContext>,
  ) -> Result<CommitOutput, MlsError> {
    let bundle = self
      .group
      .commit_builder()
      .propose_group_context_extensions(extensions)
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .load_psks(provider.storage())
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .build(provider.rand(), provider.crypto(), device, |_| true)
      .map_err(|error| MlsError::openmls("commit creation", error))?
      .stage_commit(provider)
      .map_err(|error| MlsError::openmls("commit staging", error))?;
    commit_output(bundle)
  }

  /// TeamCraft's rule for an external commit: only a further device of a user who is already a
  /// member joins this way, like an unsigned device addition (e2ee.md §4.6). It carries nothing but
  /// the external init; no removals (no resync), no authenticated data.
  fn authorize_external_join(
    &self,
    staged: &StagedCommit,
    authenticated_data: &[u8],
    directory: &impl IdentityDirectory,
  ) -> Result<MembershipUpdate, MlsError> {
    if !authenticated_data.is_empty() {
      return Err(MlsError::UnexpectedMessage(
        "external commit with authenticated data",
      ));
    }
    for queued in staged.queued_proposals() {
      if !matches!(queued.proposal(), Proposal::ExternalInit(_)) {
        return Err(MlsError::UnsupportedProposal);
      }
    }
    let leaf = staged
      .update_path_leaf_node()
      .ok_or(MlsError::UnexpectedMessage("external commit without a leaf"))?;
    let device = credential::verify(leaf.credential(), leaf.signature_key().as_slice(), directory)?;
    // Fails closed: nobody vouches for this device but its user's signed device list.
    if !directory.is_listed_device(&device) {
      return Err(MlsError::InactiveDevice(device.device_id()));
    }
    let joiner = GroupMember::from(&device);
    let members = self.members()?;
    if members.contains(&joiner) {
      return Err(MlsError::UnexpectedMessage(
        "external commit by a device already in the group",
      ));
    }
    if !members.iter().any(|member| member.user_id == joiner.user_id) {
      // A new user needs a signed change.
      return Err(MlsError::MissingSignedChange);
    }
    self.check_not_removed(&ProposedMembership {
      added: vec![device],
      removed: Vec::new(),
    })?;
    Ok(MembershipUpdate {
      maybe_change: None,
      maybe_signer: None,
      added: vec![joiner],
      removed: Vec::new(),
      maybe_rejoined: None,
      is_self_removed: false,
      maybe_committer: None,
      maybe_properties_change: None,
      maybe_strict_roles_change: None,
    })
  }

  /// Checks a group this device is joining by external commit, before the commit is sent: every
  /// leaf verifies against the pinned identities, this device's user is in it, this device wasn't.
  fn check_external_join(
    &self,
    certificate: &SignedDeviceCertificate,
    directory: &impl IdentityDirectory,
  ) -> Result<(), MlsError> {
    let own = certificate.unverified();
    let own_leaf = self.group.own_leaf_index();
    let mut has_own_user = false;
    // The tree already holds this device's new leaf; the others are what the group was.
    for leaf in self.group.members().filter(|leaf| leaf.index != own_leaf) {
      let member = credential::verify(&leaf.credential, &leaf.signature_key, directory)?;
      if member.device_id() == own.device_id {
        return Err(MlsError::AlreadyInGroup);
      }
      has_own_user |= member.user_id() == own.user_id;
    }
    if has_own_user {
      Ok(())
    } else {
      Err(MlsError::MissingSignedChange)
    }
  }

  /// The group-side checks of a verified rejoin request; returns the device's current leaf.
  fn check_verified_rejoin(
    &self,
    verified: &VerifiedRejoinRequest,
  ) -> Result<(LeafNodeIndex, GroupMember), MlsError> {
    let request = verified.request();
    self.ensure_conversation(request.conversation_id())?;
    if self.applied_changes.contains(&request.request_id()) {
      return Err(MlsError::ReplayedChange(request.request_id()));
    }
    let current = self.epoch();
    if request.epoch() > current || current - request.epoch() > MAX_CHANGE_AGE_EPOCHS {
      return Err(MlsError::StaleChange {
        signed_epoch: request.epoch(),
        current_epoch: current,
      });
    }
    let member = request.device();
    for leaf in self.group.members() {
      if credential::member_of(&leaf.credential)? == member {
        // The very key the device signs its leaf with signed the request.
        verified
          .device()
          .ensure_signing_key(leaf.signature_key.as_slice())
          .map_err(MlsError::InvalidCredential)?;
        return Ok((leaf.index, member));
      }
    }
    Err(MlsError::ChangeDoesNotMatchCommit)
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

  /// A device whose leaf doesn't announce every extension the group requires (an app from before
  /// the anchor, e2ee.md §4.6.5) can't be added: openmls would refuse the commit. Checked first, so
  /// the adder can say that person has to update before anything is sent.
  fn ensure_capable(&self, key_package: &KeyPackage, device: &VerifiedDevice) -> Result<(), MlsError> {
    let Some(required) = self.group.extensions().required_capabilities() else {
      return Ok(());
    };
    let announced = key_package.leaf_node().capabilities().extensions();
    if required
      .extension_types()
      .iter()
      .all(|extension| announced.contains(extension))
    {
      Ok(())
    } else {
      Err(MlsError::MustUpdate(device.device_id()))
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

  /// A guest's commit may add or remove the guest's own devices and nothing else (e2ee.md §4.6,
  /// "Meeting guests"): no signed change and no role makes a guest a member who manages others.
  /// One exception: somebody's verified signed leave (`maybe_self_leave`, the user who signed
  /// their own removal), which only takes that user out. Without it a guest left alone with them
  /// (a guest's notes the import account leaves) could never see them go (#1074).
  fn check_guest_commit(
    &self,
    sender_index: LeafNodeIndex,
    proposed: &ProposedMembership,
    maybe_self_leave: Option<UserId>,
    directory: &impl IdentityDirectory,
  ) -> Result<(), MlsError> {
    let sender = self
      .group
      .member(sender_index)
      .ok_or(MlsError::ChangeDoesNotMatchCommit)?;
    let sender = credential::member_of(sender)?.user_id;
    if !directory.is_guest(sender) {
      return Ok(());
    }
    let is_signed_leave = maybe_self_leave.is_some_and(|leaver| {
      proposed.added.is_empty() && proposed.removed.iter().all(|member| member.user_id == leaver)
    });
    let touches_others = proposed.added.iter().any(|device| device.user_id() != sender)
      || proposed.removed.iter().any(|member| member.user_id != sender);
    if touches_others && !is_signed_leave {
      return Err(MlsError::GuestOverreach);
    }
    Ok(())
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

impl StagedJoin {
  /// The users the welcome's tree claims to hold, unverified: whose identities to pin.
  pub fn claimed_users(&self) -> Result<Vec<UserId>, MlsError> {
    let mut users = Vec::new();
    for member in self.staged.members() {
      let user = credential::member_of(&member.credential)?.user_id;
      if !users.contains(&user) {
        users.push(user);
      }
    }
    Ok(users)
  }

  /// The member whose commit made the welcome, as its leaf claims; [`Self::complete`] verifies
  /// every leaf, this one included.
  pub fn sender(&self) -> Result<GroupMember, MlsError> {
    let sender_index = self.staged.welcome_sender_index();
    for member in self.staged.members() {
      if member.index == sender_index {
        return credential::member_of(&member.credential);
      }
    }
    Err(MlsError::UnexpectedMessage(
      "the welcome's sender isn't in its group",
    ))
  }

  /// The anchor extension's bytes in the welcome's group context, unparsed.
  pub fn anchor_bytes(&self) -> Option<&[u8]> {
    anchor_bytes_of(self.staged.group_context().extensions())
  }

  /// The strict roles of the welcome's group context; `None` for a channel that isn't strict.
  /// Parsed, not trusted: who made the welcome is checked against what this device knew
  /// ([`Self::check_strict_sender`]).
  pub fn strict_roles(&self) -> Result<Option<StrictRoles>, MlsError> {
    strict_bytes_of(self.staged.group_context().extensions())
      .map(StrictRoles::from_bytes)
      .transpose()
      .map_err(MlsError::from)
  }

  /// A device that fell out of a strict channel opens a welcome only from a channel admin of the
  /// newest roles it verified (`known`), or from a workspace owner or admin (e2ee.md §4.6.2,
  /// "Falling out in a strict channel"): anybody else's welcome could carry a change it refused.
  /// The admin is judged by `known`, not by the welcome's own context, which its maker chose.
  /// Taking a member's welcome after `strict_rejoin_wait` on the user's word is the app's
  /// decision, not this check's. Returns the sender.
  pub fn check_strict_sender(
    &self,
    known: &StrictRoles,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<GroupMember, MlsError> {
    let sender_index = self.staged.welcome_sender_index();
    let leaf = self
      .staged
      .members()
      .find(|member| member.index == sender_index)
      .ok_or(MlsError::UnexpectedMessage(
        "the welcome's sender isn't in its group",
      ))?;
    let certificate = credential::decode(&leaf.credential)?;
    let claimed = credential::member_of(&leaf.credential)?;
    let is_known_admin = known.role_of(claimed.user_id).is_some_and(|role| {
      role.role == StrictRoleKind::Admin
        && certificate
          .verify(&role.identity_key)
          .is_ok_and(|device| device.ensure_signing_key(&leaf.signature_key).is_ok())
    });
    if is_known_admin {
      return Ok(claimed);
    }
    credential::verify(&leaf.credential, &leaf.signature_key, directory)?;
    match authority.workspace_signer(claimed.user_id, 0, Judged::Newest) {
      Standing::Confirmed => Ok(claimed),
      Standing::Unknown => Err(MlsError::StrictRosterNeeded(claimed.user_id)),
      Standing::Refused => Err(MlsError::StrictWelcomeRefused(claimed.user_id)),
    }
  }

  /// Verifies the welcome's anchor (checks 1 to 5, e2ee.md §4.6.5) before the join completes;
  /// `Ok(None)` for a group without one. Check the pin and floor before completing.
  pub fn verified_anchor(
    &self,
    expected: &AnchorExpectation,
    directory: &impl IdentityDirectory,
    authority: &impl MembershipAuthority,
  ) -> Result<Option<VerifiedAnchor>, MlsError> {
    let Some(bytes) = self.anchor_bytes() else {
      return Ok(None);
    };
    let anchor = ChannelAnchor::from_bytes(bytes, DEFAULT_MAX_ANCHOR_BYTES)?;
    let tree_users = self
      .staged
      .members()
      .map(|member| credential::member_of(&member.credential).map(|member| member.user_id))
      .collect::<Result<BTreeSet<_>, _>>()?;
    let epoch = self.staged.group_context().epoch().as_u64();
    Ok(Some(anchor.verify(
      expected,
      epoch,
      &tree_users,
      directory,
      authority,
    )?))
  }

  /// Verifies every leaf against its user's pinned identity without joining yet: who is in the
  /// welcome's group and which member made the welcome. For a decision that has to be made
  /// before this device gives up a group it holds for the same conversation (e2ee.md §4.6).
  ///
  /// Stricter than [`Self::complete`]: every device must be active by its user's signed device
  /// list, and the sender, on whose word alone the group replaces another, must be named by one
  /// (fails closed), so a revoked device can't hand out the group (membership.rs).
  pub fn verify(&self, directory: &impl IdentityDirectory) -> Result<VerifiedWelcome, MlsError> {
    let sender_index = self.staged.welcome_sender_index();
    let mut members = Vec::new();
    let mut maybe_sender = None;
    for member in self.staged.members() {
      let device = credential::verify(&member.credential, &member.signature_key, directory)?;
      if !directory.is_active_device(&device) {
        return Err(MlsError::InactiveDevice(device.device_id()));
      }
      if member.index == sender_index {
        if !directory.is_listed_device(&device) {
          return Err(MlsError::InactiveDevice(device.device_id()));
        }
        maybe_sender = Some(GroupMember::from(&device));
      }
      members.push(GroupMember::from(&device));
    }
    let sender = maybe_sender.ok_or(MlsError::UnexpectedMessage(
      "the welcome's sender isn't in its group",
    ))?;
    Ok(VerifiedWelcome { members, sender })
  }

  /// Verifies every leaf against its user's pinned identity and joins the group.
  pub fn complete(
    self,
    provider: &impl OpenMlsProvider,
    directory: &impl IdentityDirectory,
  ) -> Result<ConversationGroup, MlsError> {
    for member in self.staged.members() {
      credential::verify(&member.credential, &member.signature_key, directory)?;
    }
    let group = self
      .staged
      .into_group(provider)
      .map_err(|error| MlsError::openmls("joining group", error))?;
    let history = ChangeHistory {
      joined_epoch: group.epoch().as_u64(),
      ..ChangeHistory::default()
    };
    Ok(ConversationGroup::from_group(
      self.conversation_id,
      group,
      history,
    ))
  }
}

impl PendingCommit {
  /// The encoding older apps read too, unless the commit is a move or rename.
  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    if self.maybe_properties_change.is_some() {
      return Ok(self.tls_serialize_detached()?);
    }
    Ok(
      LegacyPendingCommit {
        commit: self.commit.clone(),
        maybe_change: self.maybe_change.clone(),
        removed: self.removed.clone(),
        maybe_rejoin: self.maybe_rejoin,
      }
      .tls_serialize_detached()?,
    )
  }

  /// Reads both encodings: the legacy one lacks the trailing properties change.
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    if let Ok(pending) = Self::tls_deserialize_exact_bytes(bytes) {
      return Ok(pending);
    }
    let legacy = LegacyPendingCommit::tls_deserialize_exact_bytes(bytes)?;
    Ok(Self {
      commit: legacy.commit,
      maybe_change: legacy.maybe_change,
      removed: legacy.removed,
      maybe_rejoin: legacy.maybe_rejoin,
      maybe_properties_change: None,
    })
  }

  /// Whether `bytes`, an entry of the group's queue, is this commit (the server accepted it, even
  /// if its answer never arrived).
  pub fn is_commit(&self, bytes: &[u8]) -> bool {
    self.commit == bytes
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

/// What to send for a staged commit.
fn commit_output(bundle: CommitMessageBundle) -> Result<CommitOutput, MlsError> {
  let maybe_welcome = bundle
    .to_welcome_msg()
    .map(|welcome| welcome.to_bytes())
    .transpose()
    .map_err(|error| MlsError::openmls("welcome encoding", error))?;
  let maybe_group_info = bundle
    .group_info()
    .cloned()
    .map(|info| MlsMessageOut::from(info).to_bytes())
    .transpose()
    .map_err(|error| MlsError::openmls("group info encoding", error))?;
  let commit = bundle
    .into_commit()
    .to_bytes()
    .map_err(|error| MlsError::openmls("commit encoding", error))?;
  Ok(CommitOutput {
    commit,
    maybe_welcome,
    maybe_group_info,
  })
}

fn strict_bytes_of(extensions: &Extensions<GroupContext>) -> Option<&[u8]> {
  extensions
    .unknown(STRICT_ROLES_EXTENSION_TYPE)
    .map(|extension| extension.0.as_slice())
}

/// `extensions` after a roles change: the roles extension set (replaced in place, or appended)
/// and required of every leaf, or removed with its requirement for [`StrictRolesVerdict::Off`].
/// Every other extension, and every other requirement, stays byte for byte and in its place. The
/// committer and every member compute it alike, and the commit's context must equal it.
fn with_strict(
  extensions: &Extensions<GroupContext>,
  verdict: &StrictRolesVerdict,
) -> Result<Extensions<GroupContext>, MlsError> {
  let strict_type = ExtensionType::Unknown(STRICT_ROLES_EXTENSION_TYPE);
  let maybe_bytes = match verdict {
    StrictRolesVerdict::Roles(roles) => Some(roles.to_bytes()?),
    StrictRolesVerdict::Off => None,
  };
  let mut result = Vec::new();
  let mut has_roles = false;
  let mut has_requirement = false;
  for extension in extensions.iter() {
    match extension {
      Extension::Unknown(STRICT_ROLES_EXTENSION_TYPE, _) => {
        if let Some(bytes) = &maybe_bytes {
          result.push(Extension::Unknown(
            STRICT_ROLES_EXTENSION_TYPE,
            UnknownExtension(bytes.clone()),
          ));
          has_roles = true;
        }
      },
      Extension::RequiredCapabilities(required) => {
        has_requirement = true;
        let mut types: Vec<ExtensionType> = required
          .extension_types()
          .iter()
          .copied()
          .filter(|extension_type| *extension_type != strict_type)
          .collect();
        if maybe_bytes.is_some() {
          types.push(strict_type);
        }
        result.push(Extension::RequiredCapabilities(
          RequiredCapabilitiesExtension::new(&types, required.proposal_types(), required.credential_types()),
        ));
      },
      other => result.push(other.clone()),
    }
  }
  if let Some(bytes) = maybe_bytes {
    if !has_roles {
      result.push(Extension::Unknown(
        STRICT_ROLES_EXTENSION_TYPE,
        UnknownExtension(bytes),
      ));
    }
    if !has_requirement {
      result.push(Extension::RequiredCapabilities(
        RequiredCapabilitiesExtension::new(&[strict_type], &[], &[]),
      ));
    }
  }
  Extensions::from_vec(result).map_err(|error| MlsError::openmls("strict roles extension", error))
}

fn anchor_bytes_of(extensions: &Extensions<GroupContext>) -> Option<&[u8]> {
  extensions
    .unknown(CHANNEL_ANCHOR_EXTENSION_TYPE)
    .map(|extension| extension.0.as_slice())
}

/// `extensions` with the anchor's content replaced by `anchor`, every other extension byte for
/// byte and in its place (e2ee.md §4.6.5: a proposal keeps what it doesn't change, unknown ones
/// included).
fn with_anchor(
  extensions: &Extensions<GroupContext>,
  anchor: Vec<u8>,
) -> Result<Extensions<GroupContext>, MlsError> {
  let mut anchor = Some(anchor);
  let replaced: Vec<Extension> = extensions
    .iter()
    .map(|extension| match extension {
      Extension::Unknown(CHANNEL_ANCHOR_EXTENSION_TYPE, _) => Extension::Unknown(
        CHANNEL_ANCHOR_EXTENSION_TYPE,
        UnknownExtension(anchor.take().unwrap_or_default()),
      ),
      other => other.clone(),
    })
    .collect();
  if anchor.is_some() {
    return Err(MlsError::NotAnchored);
  }
  Extensions::from_vec(replaced).map_err(|error| MlsError::openmls("anchor extension", error))
}

/// Whether the tree in a group information holds a leaf with this signature key. Read as it comes,
/// without checking lifetimes or signatures: an old leaf must not hide this device's own, and a
/// made-up one only makes the device ask to be added.
fn holds_signature_key(info: &VerifiableGroupInfo, signature_key: &[u8]) -> bool {
  info.extensions().ratchet_tree().is_some_and(|tree| {
    tree
      .ratchet_tree()
      .leaves()
      .any(|leaf| leaf.signature_key().as_slice() == signature_key)
  })
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
