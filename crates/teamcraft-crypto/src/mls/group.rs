//! One MLS group per conversation, with TeamCraft's commit authorization rules.

use std::collections::BTreeSet;

use openmls::prelude::{
  GroupId, KeyPackage, LeafNodeIndex, MlsGroup, MlsGroupJoinConfig, MlsMessageBodyIn, MlsMessageIn,
  MlsMessageOut, ProcessedMessageContent, Proposal, Sender, StagedCommit, StagedWelcome,
};
use openmls_traits::OpenMlsProvider;
use tls_codec::{
  Deserialize as _, DeserializeBytes as _, Serialize as _, TlsDeserializeBytes, TlsSerialize, TlsSize,
};
use zeroize::Zeroizing;

use super::content_keys::{ApplicationPayload, ContentKeyBundle, SealedContentKeys};
use super::rejoin::{SignedRejoinRequest, VerifiedRejoinRequest};
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
  pub added: Vec<GroupMember>,
  /// Includes a rejoining device's old leaf.
  pub removed: Vec<GroupMember>,
  /// A device that asked to be added back and was ([`ConversationGroup::commit_rejoin`]): it is in
  /// both `removed` (its old leaf) and `added`, and stays a member.
  pub maybe_rejoined: Option<GroupMember>,
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
      .map_err(|error| MlsError::openmls("welcome processing", error))?
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
    let config = MlsGroupJoinConfig::builder()
      .use_ratchet_tree_extension(true)
      .build();
    // Lifetimes of existing leaves are not checked, as when joining from a welcome (§4.3.1).
    let (group, bundle) = MlsGroup::external_commit_builder()
      .with_config(config)
      .skip_lifetime_validation()
      .build_group(provider, info, credential::credential_with_key(certificate)?)
      .map_err(|error| MlsError::openmls("external join", error))?
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
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: Some(change.clone()),
      removed: proposed.removed,
      maybe_rejoin: None,
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
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: Vec::new(),
      maybe_rejoin: None,
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
    if GroupMember::from(&added) != member {
      return Err(MlsError::ChangeDoesNotMatchCommit);
    }
    let mut aad = REJOIN_AAD_MAGIC.to_vec();
    aad.extend_from_slice(&signed.to_bytes()?);
    self.group.set_aad(aad);
    let result = self.stage_commit(provider, device, vec![key_package], vec![(old_leaf, member)]);
    self.group.set_aad(Vec::new());
    let output = result?;
    self.maybe_pending = Some(PendingCommit {
      commit: output.commit.clone(),
      maybe_change: None,
      removed: vec![member],
      maybe_rejoin: Some((member, verified.request().request_id())),
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
      added: pending.maybe_rejoin.iter().map(|(member, _)| *member).collect(),
      removed: pending.removed,
      maybe_rejoined: pending.maybe_rejoin.map(|(member, _)| member),
      is_self_removed: false,
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
        let (update, maybe_request_id) = match maybe_sender_index {
          Some(sender_index) => {
            self.authorize_commit(&staged, sender_index, &authenticated_data, directory, authority)?
          },
          None => (
            self.authorize_external_join(&staged, &authenticated_data, directory)?,
            None,
          ),
        };
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
    let proposed = self.verify_commit_contents(staged, sender_index, directory)?;
    if let Some(request) = authenticated_data.strip_prefix(REJOIN_AAD_MAGIC) {
      let verified = SignedRejoinRequest::from_bytes(request)?.verify(directory)?;
      let (_, member) = self.check_verified_rejoin(&verified)?;
      let added: Vec<GroupMember> = proposed.added.iter().map(GroupMember::from).collect();
      if added != [member] || proposed.removed != [member] {
        return Err(MlsError::ChangeDoesNotMatchCommit);
      }
      let update = MembershipUpdate {
        maybe_change: None,
        added,
        removed: proposed.removed,
        maybe_rejoined: Some(member),
        is_self_removed: staged.self_removed(),
      };
      return Ok((update, Some(verified.request().request_id())));
    }
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
    let update = MembershipUpdate {
      maybe_change,
      added: proposed.added.iter().map(GroupMember::from).collect(),
      removed: proposed.removed,
      maybe_rejoined: None,
      is_self_removed: staged.self_removed(),
    };
    Ok((update, None))
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
      added: vec![joiner],
      removed: Vec::new(),
      maybe_rejoined: None,
      is_self_removed: false,
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
  pub fn to_bytes(&self) -> Result<Vec<u8>, MlsError> {
    Ok(self.tls_serialize_detached()?)
  }

  pub fn from_bytes(bytes: &[u8]) -> Result<Self, MlsError> {
    Ok(Self::tls_deserialize_exact_bytes(bytes)?)
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
