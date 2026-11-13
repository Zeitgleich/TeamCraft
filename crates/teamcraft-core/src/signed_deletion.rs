//! Signed deletions of channels (issue #1135, e2ee.md §4.6). A device drops a channel's history at
//! once only for a deletion it verifies itself, against signed data alone: the workspace roster and
//! the channel's signed roles, as this device verified them, and its own MLS group. Anything else,
//! whatever the server says, is a claim, which hides the conversation and keeps its history
//! (`client::server_deletions`).
//!
//! Who may sign:
//!
//! - **An owner or admin of the verified roster**, whose newest signed roster carries a signing time
//!   and is fresh now and when the deletion was signed.
//! - **An admin of the signed snapshot** this device accepted last, verified against the key that
//!   snapshot names, who is still in this device's group, if that snapshot goes back to a workspace
//!   admin's entry. A snapshot that goes back only to a founder's claim doesn't count: a member and
//!   the server together can make one up, and that may not cost everybody's history at once. And
//!   only if somebody else signed an entry this device accepted within `channel_roles_max_age`: a
//!   server could hold back the signer's demotion, and refusing costs only speed (e2ee.md §4.6.3).
//!
//! The signing device must be on its user's signed device list, which the caller checks
//! ([`VerifiedDeletion::device`]): the certificate alone outlives a device's removal, and it has no
//! expiry.
//!
//! On an anchored channel (e2ee.md §4.6.5, D3) the deletion must also name the channel's name as the
//! anchor signed it, by a founder, a strict channel's admin or a workspace signer, and the server
//! must list that name: a member's rename, a swapped or missing name, or a forked anchor take the
//! retention.
//!
//! A space's deletion (e2ee.md §4.6.5, D7, #1727) is signed once for the space by a workspace owner
//! or admin and names every channel the deleter was shown, with its epoch and name hash. It purges
//! a channel at once only where the channel's anchor places it in that space, confirmed, its name
//! counts as above, and everything a channel's deletion needs holds; every other channel stays a
//! claim. A space admin's deletion is never signed.
//!
//! The deletion names the channel, the group's epoch, which the server takes only as the current one,
//! and when it was signed. So a signature the server held back, for example after refusing it, is
//! refused here once the group has moved on or it got too old. A device without the group judges
//! nothing, and neither does one that sees a sign of being shown different logs.

use teamcraft_crypto::channel_anchor::{AnchorHash, Confidence, VerifiedAnchor};
use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::mls::MAX_CHANGE_AGE_EPOCHS;
use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry, verify_log_entry};
use teamcraft_proto::{
  Base64Bytes, ChannelDeletionDocument, ChannelRole, ConversationId, SignedChannelDeletion,
  SignedSpaceDeletion, SpaceDeletionChannel, SpaceDeletionDocument, SpaceId, UserId,
};

use crate::anchoring::name_counts_for_deletion;
use crate::channel_roles::{ChannelRoleState, KeyDirectory, SignerAuthority, roster_admin_keys};
use crate::convert;
use crate::error::CoreError;
use crate::workspace::{RosterRole, WorkspaceState};

/// What verifying a signed deletion takes, all of it this device's own verified state.
pub struct DeletionVerifier<'a> {
  pub conversation: ConversationId,
  /// Whether this device knows the conversation as a channel. A signature never purges a direct
  /// message: nobody holds a role there.
  pub is_channel: bool,
  /// This device's group of the conversation.
  pub maybe_group: Option<GroupFacts<'a>>,
  pub roles: &'a ChannelRoleState,
  pub workspace: &'a WorkspaceState,
  /// How old the newest signed roster may be for its admins to count.
  pub roster_max_age_ms: i64,
  /// How old a signature may be, and how far ahead of `now_ms` it may say it was made.
  pub max_age_ms: i64,
  pub clock_skew_ms: i64,
  /// This device's time.
  pub now_ms: i64,
  /// How long ago somebody other than a channel admin who signs a deletion must have signed one of
  /// the channel's role entries for the deletion to purge at once (e2ee.md §4.6.3, part 3).
  pub channel_roles_max_age_ms: i64,
  /// The newest server time this device saw; a time ahead of `now_ms` can only refuse.
  pub newest_server_ms: i64,
  /// The channel's anchor as this device holds it (D3).
  pub anchor: AnchorFacts<'a>,
}

/// What this device knows of the channel's anchor when it judges a deletion.
#[derive(Clone, Copy, Debug)]
pub enum AnchorFacts<'a> {
  /// No anchor, and none pinned: the deletion is judged as before anchors.
  Unanchored,
  /// The user's devices pinned the channel to an anchor this device can't verify right now: the
  /// anchor grants nothing, and the name can't be checked.
  Unverifiable,
  Anchored {
    anchor: &'a VerifiedAnchor,
    /// The pin and floor show no fork, and the anchor this device holds isn't below the floor its
    /// user's devices recorded (a move or rename it hasn't seen yet).
    floor_grants: bool,
    /// The name the server lists for the channel.
    maybe_listed_name: Option<&'a str>,
  },
}

/// What this device's MLS group says.
#[derive(Clone, Copy, Debug)]
pub struct GroupFacts<'a> {
  pub epoch: u64,
  pub users: &'a [UserId],
}

/// A deletion whose signature holds: who signed it, on what authority.
#[derive(Clone, Debug)]
pub struct VerifiedDeletion {
  pub signer: UserId,
  pub is_workspace_admin: bool,
  /// The key the signature verified under, for the signer's device list.
  pub identity: IdentityPublicKey,
  /// The device, which must be on that list for the deletion to count.
  pub device: VerifiedDevice,
}

/// Why a signed deletion doesn't count. It then stays the server's word.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignedDeletionError {
  #[error("the signed deletion has an unreadable payload")]
  Payload,
  #[error("the signed deletion is for another conversation")]
  OtherConversation,
  #[error("this device doesn't know the conversation as a channel")]
  NotAChannel,
  #[error("this device has no group of the conversation, so it can't tell how old the deletion is")]
  NoGroup,
  #[error("the signed deletion is for epoch {signed}, and the group has moved on to {local}")]
  OldEpoch { signed: u64, local: u64 },
  #[error("the signed deletion is too old, or dated too far ahead")]
  OutOfDate,
  #[error("this device has signs of being shown different logs than its colleagues")]
  Alarm,
  #[error("the signature doesn't verify for anybody allowed to delete this channel")]
  Unauthorized,
  #[error("nobody but the deleting channel admin signed this channel's roles lately")]
  NoRecentOtherSigner,
  /// D3: the deletion doesn't name the anchored name, the name is a member's or doesn't verify,
  /// or the server lists another one.
  #[error("the signed deletion doesn't name the channel's anchored name")]
  NameMismatch,
  /// A space's deletion that doesn't name this channel.
  #[error("the signed space deletion doesn't name this channel")]
  NotListed,
  /// D7: the channel has no anchor that verifies here, its anchor places it in another space or
  /// none, or a move this device can't confirm.
  #[error("the channel's anchor doesn't place it in the deleted space")]
  NotInSpace,
}

/// Checks a signed deletion against what this device verified.
pub fn verify_signed_deletion(
  signed: &SignedChannelDeletion,
  verifier: &DeletionVerifier<'_>,
) -> Result<VerifiedDeletion, SignedDeletionError> {
  let document: ChannelDeletionDocument =
    serde_json::from_slice(signed.payload.as_slice()).map_err(|_| SignedDeletionError::Payload)?;
  if document.conversation_id != verifier.conversation {
    return Err(SignedDeletionError::OtherConversation);
  }
  if !verifier.is_channel {
    return Err(SignedDeletionError::NotAChannel);
  }
  let group = verifier.maybe_group.ok_or(SignedDeletionError::NoGroup)?;
  if group.epoch > document.epoch {
    return Err(SignedDeletionError::OldEpoch {
      signed: document.epoch,
      local: group.epoch,
    });
  }
  let age_ms = verifier.now_ms.saturating_sub(document.signed_at_ms);
  if age_ms > verifier.max_age_ms || age_ms < verifier.clock_skew_ms.saturating_neg() {
    return Err(SignedDeletionError::OutOfDate);
  }
  if verifier.roles.maybe_alarm.is_some()
    || verifier.roles.maybe_ahead.is_some()
    || verifier.workspace.maybe_alarm.is_some()
    || verifier.workspace.maybe_ahead.is_some()
  {
    return Err(SignedDeletionError::Alarm);
  }
  if !is_name_as_anchored(document.maybe_name_hash.as_ref(), verifier.anchor) {
    return Err(SignedDeletionError::NameMismatch);
  }
  if let Some(verified) = verified_by_workspace_admin(
    SignedEntry::of_channel(signed, &document),
    document.signed_at_ms,
    verifier,
  ) {
    return Ok(verified);
  }
  let verified =
    verified_by_channel_admin(signed, &document, verifier, group).ok_or(SignedDeletionError::Unauthorized)?;
  if !is_vouched_for_lately(&verified, &document, verifier) {
    return Err(SignedDeletionError::NoRecentOtherSigner);
  }
  Ok(verified)
}

/// Checks a space's signed deletion for the one channel `verifier` is about (D7): it must name the
/// channel, the channel's anchor must place it in that space (confirmed, the floor granting), its
/// name count as for a channel's deletion, and the signer be an owner or admin of the fresh roster,
/// at or after the roster version the document names. Group epoch, age and alarms as for a
/// channel's deletion. A channel admin's authority doesn't reach a space.
pub fn verify_signed_space_deletion(
  signed: &SignedSpaceDeletion,
  verifier: &DeletionVerifier<'_>,
) -> Result<VerifiedDeletion, SignedDeletionError> {
  let document: SpaceDeletionDocument =
    serde_json::from_slice(signed.payload.as_slice()).map_err(|_| SignedDeletionError::Payload)?;
  let mut named = document
    .channels
    .iter()
    .filter(|channel| channel.conversation_id == verifier.conversation);
  let entry = named.next().ok_or(SignedDeletionError::NotListed)?;
  // A channel named twice was named with two epochs or names: which one the deleter saw is unclear.
  if named.next().is_some() {
    return Err(SignedDeletionError::Payload);
  }
  if !verifier.is_channel {
    return Err(SignedDeletionError::NotAChannel);
  }
  let group = verifier.maybe_group.ok_or(SignedDeletionError::NoGroup)?;
  if group.epoch > entry.epoch {
    return Err(SignedDeletionError::OldEpoch {
      signed: entry.epoch,
      local: group.epoch,
    });
  }
  let version = u64::try_from(document.signed_at_ms).map_err(|_| SignedDeletionError::OutOfDate)?;
  let age_ms = verifier.now_ms.saturating_sub(document.signed_at_ms);
  if age_ms > verifier.max_age_ms || age_ms < verifier.clock_skew_ms.saturating_neg() {
    return Err(SignedDeletionError::OutOfDate);
  }
  // The channel's roles grant nothing here, and its log isn't kept for a space's deletion: a
  // member's newer head noted for it is news this device can't read any more, not a sign of a
  // fork. A fork it saw, and anything about the roster, still refuses.
  if verifier.roles.maybe_alarm.is_some()
    || verifier.workspace.maybe_alarm.is_some()
    || verifier.workspace.maybe_ahead.is_some()
  {
    return Err(SignedDeletionError::Alarm);
  }
  if !is_in_space(document.space_id, verifier.anchor) {
    return Err(SignedDeletionError::NotInSpace);
  }
  if !is_name_as_anchored(entry.maybe_name_hash.as_ref(), verifier.anchor) {
    return Err(SignedDeletionError::NameMismatch);
  }
  if verifier.workspace.roster.version < document.roster_version {
    return Err(SignedDeletionError::Unauthorized);
  }
  verified_by_workspace_admin(
    SignedEntry {
      signature: signed.signature.as_slice(),
      log: WorkspaceLog::SpaceDeletion,
      version,
      payload: signed.payload.as_slice(),
    },
    document.signed_at_ms,
    verifier,
  )
  .ok_or(SignedDeletionError::Unauthorized)
}

/// The signed deletion of `space` with the channels the deleter was shown, by a workspace owner or
/// admin of `roster_version` (D7). Who may sign is up to the members' devices.
pub(crate) fn sign_space_deletion(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  space: SpaceId,
  roster_version: u64,
  channels: Vec<SpaceDeletionChannel>,
  signed_at_ms: i64,
) -> Result<SignedSpaceDeletion, CoreError> {
  let version =
    u64::try_from(signed_at_ms).map_err(|_| CoreError::Corrupt("a signing time before 1970".into()))?;
  let document = SpaceDeletionDocument {
    space_id: space,
    roster_version,
    channels,
    signed_at_ms,
  };
  let payload = serde_json::to_vec(&document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  let signature = sign_log_entry(
    device,
    certificate,
    WorkspaceLog::SpaceDeletion,
    version,
    &payload,
  )?;
  Ok(SignedSpaceDeletion {
    payload: Base64Bytes::new(payload),
    signature: Base64Bytes::new(signature),
  })
}

/// Which channels a space's signed deletion names, without verifying anything: what to judge it
/// for. Empty if the payload doesn't read.
pub fn space_deletion_channels(signed: &SignedSpaceDeletion) -> Vec<ConversationId> {
  serde_json::from_slice::<SpaceDeletionDocument>(signed.payload.as_slice())
    .map(|document| {
      document
        .channels
        .into_iter()
        .map(|channel| channel.conversation_id)
        .collect()
    })
    .unwrap_or_default()
}

/// D7: the channel's verified anchor places it in `space`, confirmed, and the floor grants. A
/// channel without an anchor, or one this device can't verify, is never purged by a space.
fn is_in_space(space: SpaceId, anchor: AnchorFacts<'_>) -> bool {
  match anchor {
    AnchorFacts::Unanchored | AnchorFacts::Unverifiable => false,
    AnchorFacts::Anchored {
      anchor, floor_grants, ..
    } => {
      floor_grants
        && anchor.space_confidence() == Confidence::Confirmed
        && anchor.properties().maybe_space_id == Some(convert::space(space))
    },
  }
}

/// The signed deletion of `conversation` for the group's `epoch`, naming the hash of the name the
/// deleter was shown (D3). Who may sign is up to the members' devices.
pub(crate) fn sign_channel_deletion(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  conversation: ConversationId,
  epoch: u64,
  signed_at_ms: i64,
  maybe_name_hash: Option<AnchorHash>,
) -> Result<SignedChannelDeletion, CoreError> {
  let document = ChannelDeletionDocument {
    conversation_id: conversation,
    epoch,
    signed_at_ms,
    maybe_name_hash: maybe_name_hash.map(|hash| Base64Bytes::new(hash.as_bytes().to_vec())),
  };
  let payload = serde_json::to_vec(&document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  let signature = sign_log_entry(
    device,
    certificate,
    WorkspaceLog::ChannelDeletion,
    epoch,
    &payload,
  )?;
  Ok(SignedChannelDeletion {
    payload: Base64Bytes::new(payload),
    signature: Base64Bytes::new(signature),
  })
}

/// D3: on an anchored channel the deletion names the anchor's name, which counts for deletions
/// (`anchoring::name_counts_for_deletion`); a pinned anchor this device can't verify refuses. A
/// channel without an anchor is judged as before.
fn is_name_as_anchored(maybe_name_hash: Option<&Base64Bytes>, anchor: AnchorFacts<'_>) -> bool {
  match anchor {
    AnchorFacts::Unanchored => true,
    AnchorFacts::Unverifiable => false,
    AnchorFacts::Anchored {
      anchor,
      floor_grants,
      maybe_listed_name,
    } => {
      let maybe_hash = maybe_name_hash
        .and_then(|hash| <[u8; 32]>::try_from(hash.as_slice()).ok())
        .map(AnchorHash::from_bytes);
      name_counts_for_deletion(anchor, floor_grants, maybe_listed_name, maybe_hash)
    },
  }
}

/// Whether a member signs for the epoch the server told it: not behind its own group, and no further
/// ahead than a change may be old (a device further behind signs nothing, and its deletion takes the
/// retention).
pub(crate) fn is_told_epoch_plausible(own: u64, told: u64) -> bool {
  told >= own && told - own <= MAX_CHANGE_AGE_EPOCHS
}

/// The roster has to be fresh at the later of this device's time and the signer's: one of the two
/// being ahead can only refuse. A roster without a signing time has no freshness to judge, whatever
/// else may accept it, so it purges nothing.
fn verified_by_workspace_admin(
  entry: SignedEntry<'_>,
  signed_at_ms: i64,
  verifier: &DeletionVerifier<'_>,
) -> Option<VerifiedDeletion> {
  let at_ms = verifier.now_ms.max(signed_at_ms);
  if verifier.workspace.heads.maybe_roster_signed_at_ms.is_none()
    || !verifier
      .workspace
      .is_roster_fresh(at_ms, verifier.roster_max_age_ms)
  {
    return None;
  }
  let admins = KeyDirectory::from_keys(roster_admin_keys(verifier.workspace));
  let verified = verified_signer(entry, &admins, true)?;
  verifier
    .workspace
    .roster
    .members
    .iter()
    .any(|member| {
      member.user_id == verified.signer && matches!(member.role, RosterRole::Owner | RosterRole::Admin)
    })
    .then_some(verified)
}

fn verified_by_channel_admin(
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  verifier: &DeletionVerifier<'_>,
  group: GroupFacts<'_>,
) -> Option<VerifiedDeletion> {
  if !verifier.roles.is_workspace_anchored {
    return None;
  }
  let admins = KeyDirectory::from_assignments(
    verifier
      .roles
      .assignments
      .iter()
      .filter(|assignment| assignment.role == ChannelRole::Admin),
  );
  let verified = verified_signer(SignedEntry::of_channel(signed, document), &admins, false)?;
  group.users.contains(&verified.signer).then_some(verified)
}

/// Whether somebody other than the deleting channel admin signed a role entry this device accepted
/// within `channel_roles_max_age` (e2ee.md §4.6.3, part 3). Each device judges by itself: the end is
/// the latest of its clock, the deletion's signing time and the newest server time it saw, so each
/// of them being ahead can only refuse. A demotion the server holds back then purges for at most that
/// long; past it, the deletion takes the retention.
fn is_vouched_for_lately(
  verified: &VerifiedDeletion,
  document: &ChannelDeletionDocument,
  verifier: &DeletionVerifier<'_>,
) -> bool {
  let end_ms = verifier
    .now_ms
    .max(document.signed_at_ms)
    .max(verifier.newest_server_ms);
  let since_ms = end_ms.saturating_sub(verifier.channel_roles_max_age_ms);
  verifier.roles.has_other_signer_since(verified.signer, since_ms)
}

/// A signed log entry of a deletion, to verify.
#[derive(Clone, Copy)]
struct SignedEntry<'a> {
  signature: &'a [u8],
  log: WorkspaceLog,
  version: u64,
  payload: &'a [u8],
}

impl<'a> SignedEntry<'a> {
  fn of_channel(signed: &'a SignedChannelDeletion, document: &ChannelDeletionDocument) -> Self {
    Self {
      signature: signed.signature.as_slice(),
      log: WorkspaceLog::ChannelDeletion,
      version: document.epoch,
      payload: signed.payload.as_slice(),
    }
  }
}

fn verified_signer(
  entry: SignedEntry<'_>,
  directory: &KeyDirectory,
  is_workspace_admin: bool,
) -> Option<VerifiedDeletion> {
  let device = verify_log_entry(
    entry.signature,
    entry.log,
    entry.version,
    entry.payload,
    directory,
  )
  .ok()?;
  Some(VerifiedDeletion {
    signer: convert::user_from_crypto(device.user_id()).ok()?,
    is_workspace_admin,
    identity: directory.pinned_identity(device.user_id())?,
    device,
  })
}

/// What this device can sign: only an authority a member can check. A channel nothing was signed for
/// has no admin yet, and founding its roles in the moment of deleting it would be worth nothing. An
/// admin of a snapshot that goes back only to a founder signs all the same: it counts on the devices
/// that have a workspace admin's checkpoint by then.
pub(crate) fn can_sign_deletion(authorities: &[SignerAuthority]) -> bool {
  authorities
    .iter()
    .any(|authority| !matches!(authority, SignerAuthority::Founder))
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::identity::IdentityKeyPair;
  use teamcraft_crypto::ids as crypto;
  use teamcraft_proto::{ChannelRoleAssignment, Id, Snowflake};

  use super::*;
  use crate::channel_roles::{ChannelRolesHead, RecentSigner};
  use crate::workspace::{LogHeads, RosterDocument, RosterMember};

  const CHANNEL: i64 = 900;
  const SPACE: i64 = 40;
  const OTHER_SPACE: i64 = 41;
  const EPOCH: u64 = 5;
  const NOW_MS: i64 = 1_000_000;
  const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
  const MAX_SIGNATURE_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;
  const CLOCK_SKEW_MS: i64 = 60 * 60 * 1000;
  const ROLES_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
  /// Who signed the entry that made the channel admin one, in the test worlds.
  const APPOINTER: i64 = 9;

  /// D3 (e2ee.md §4.6.5): on an anchored channel the deletion must name the anchored name, which
  /// the server lists, and which a member didn't set.
  mod anchored_names {
    use super::*;

    #[test]
    fn a_deletion_naming_the_founders_name_purges() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_founded_as("general", "general");
      assert!(world.verify(&admin.deletion_named(EPOCH, "general")).is_ok());
    }

    #[test]
    fn a_deletion_without_a_name_hash_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_founded_as("general", "general");
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
    }

    /// The server showed the deleter this channel as "old-test".
    #[test]
    fn a_swapped_name_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_founded_as("general", "old-test");
      assert_eq!(
        world
          .verify(&admin.deletion_named(EPOCH, "old-test"))
          .unwrap_err(),
        SignedDeletionError::NameMismatch
      );
      world.anchor_founded_as("general", "general");
      assert_eq!(
        world
          .verify(&admin.deletion_named(EPOCH, "old-test"))
          .unwrap_err(),
        SignedDeletionError::NameMismatch
      );
    }

    #[test]
    fn a_forked_or_unverifiable_anchor_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_founded_as("general", "general");
      world.is_anchor_forked = true;
      assert_eq!(
        world.verify(&admin.deletion_named(EPOCH, "general")).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
      world.is_anchor_forked = false;
      world.is_anchor_unverifiable = true;
      assert_eq!(
        world.verify(&admin.deletion_named(EPOCH, "general")).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
    }

    /// A channel without an anchor is judged as before: the name hash isn't needed.
    #[test]
    fn an_unanchored_channel_needs_no_name() {
      let (admin, world) = world_with_workspace_admin();
      assert!(world.verify(&admin.deletion(EPOCH)).is_ok());
    }

    fn world_with_workspace_admin() -> (Person, World) {
      let admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&admin, RosterRole::Admin);
      (admin, world)
    }
  }

  /// D7 (e2ee.md §4.6.5, #1727): a workspace admin's signed deletion of a space purges a channel
  /// only where its anchor places it in that space, confirmed, under a name that counts.
  mod space_deletions {
    use super::*;

    #[test]
    fn a_workspace_admins_deletion_of_the_anchored_space_purges() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let verified = world
        .verify_space(&admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]))
        .unwrap();
      assert_eq!(
        (verified.signer, verified.is_workspace_admin),
        (admin.user_id, true)
      );
    }

    #[test]
    fn a_channel_the_deletion_doesnt_name_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let signed = admin.space_deletion(SPACE, &[(CHANNEL + 1, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotListed
      );
    }

    /// The server relabelled a channel of another space, or one outside any, into the deleted
    /// space: the admin signed it, the anchor says otherwise.
    #[test]
    fn a_channel_anchored_in_another_space_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(OTHER_SPACE, "general", "general", None);
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotInSpace
      );
    }

    #[test]
    fn an_unanchored_or_unverifiable_channel_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotInSpace
      );
      world.anchor_in(SPACE, "general", "general", None);
      world.is_anchor_unverifiable = true;
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotInSpace
      );
    }

    /// A pin that marks a fork, or an anchor below the floor the user's devices recorded.
    #[test]
    fn a_forked_anchor_or_one_below_the_floor_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      world.is_anchor_forked = true;
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotInSpace
      );
    }

    /// D4: a member's move doesn't count for the space; a deletion of the space it names takes
    /// the retention.
    #[test]
    fn a_space_a_member_moved_the_channel_into_is_stale() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(OTHER_SPACE, "general", "general", Some((Some(SPACE), "general")));
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotInSpace
      );
    }

    /// D5 after the review: a member's name counts for display, never for a purge.
    #[test]
    fn a_name_a_member_set_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "old-test", Some((Some(SPACE), "old-test")));
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("old-test"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
    }

    #[test]
    fn a_missing_or_swapped_name_takes_the_retention() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let without = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, None)]);
      assert_eq!(
        world.verify_space(&without).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
      let swapped = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("old-test"))]);
      assert_eq!(
        world.verify_space(&swapped).unwrap_err(),
        SignedDeletionError::NameMismatch
      );
    }

    /// Space admins' and channel admins' deletions of a space stay claims.
    #[test]
    fn nobody_but_a_workspace_admin_counts() {
      let (channel_admin, mut world) = world_with_channel_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let signed = channel_admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::Unauthorized
      );
    }

    /// The document names a roster this device doesn't have yet (or a withheld one).
    #[test]
    fn a_roster_older_than_the_one_named_refuses() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let signed = admin.space_deletion_with(SPACE, 2, NOW_MS, &[(CHANNEL, EPOCH, Some("general"))]);
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::Unauthorized
      );
    }

    #[test]
    fn an_old_epoch_an_old_signature_an_alarm_or_no_group_refuses() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let behind = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH - 1, Some("general"))]);
      assert!(matches!(
        world.verify_space(&behind).unwrap_err(),
        SignedDeletionError::OldEpoch { .. }
      ));
      // Read past its age: the signature is older than `signed_deletion_max_age` now.
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      world.now_ms = NOW_MS + MAX_SIGNATURE_AGE_MS + 1;
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::OutOfDate
      );
      world.now_ms = NOW_MS;
      world.roles.maybe_alarm = Some("forked".into());
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::Alarm
      );
      world.roles.maybe_alarm = None;
      // A member's newer role head is only news here: the roles grant nothing for a space.
      world.roles.maybe_ahead = Some(crate::channel_roles::ChannelRolesHead {
        version: 9,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      });
      assert!(world.verify_space(&signed).is_ok());
      world.roles.maybe_ahead = None;
      world.is_channel = false;
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NotAChannel
      );
      world.is_channel = true;
      world.has_group = false;
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::NoGroup
      );
    }

    /// A channel named twice, with two epochs or names, is ambiguous.
    #[test]
    fn a_channel_named_twice_refuses() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let signed = admin.space_deletion(
        SPACE,
        &[
          (CHANNEL, EPOCH, Some("general")),
          (CHANNEL, EPOCH + 1, Some("general")),
        ],
      );
      assert_eq!(
        world.verify_space(&signed).unwrap_err(),
        SignedDeletionError::Payload
      );
    }

    /// A channel's deletion can't pass for a space's, and a space document signed as a channel's
    /// deletion doesn't verify as a space's.
    #[test]
    fn a_signature_of_another_kind_refuses() {
      let (admin, mut world) = world_with_workspace_admin();
      world.anchor_in(SPACE, "general", "general", None);
      let channel = admin.deletion_named(EPOCH, "general");
      let as_space = SignedSpaceDeletion {
        payload: channel.payload,
        signature: channel.signature,
      };
      assert_eq!(
        world.verify_space(&as_space).unwrap_err(),
        SignedDeletionError::Payload
      );
      let space = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, Some("general"))]);
      let document: SpaceDeletionDocument = serde_json::from_slice(space.payload.as_slice()).unwrap();
      let wrong_kind = sign_log_entry(
        &admin.device,
        &admin.certificate,
        WorkspaceLog::ChannelDeletion,
        u64::try_from(document.signed_at_ms).unwrap(),
        space.payload.as_slice(),
      )
      .unwrap();
      let forged = SignedSpaceDeletion {
        payload: space.payload,
        signature: Base64Bytes::new(wrong_kind),
      };
      assert_eq!(
        world.verify_space(&forged).unwrap_err(),
        SignedDeletionError::Unauthorized
      );
    }

    #[test]
    fn the_channels_a_deletion_names_are_read_without_verifying() {
      let admin = Person::new(9);
      let signed = admin.space_deletion(SPACE, &[(CHANNEL, EPOCH, None), (CHANNEL + 1, 0, None)]);
      assert_eq!(
        space_deletion_channels(&signed),
        vec![conversation(CHANNEL), conversation(CHANNEL + 1)]
      );
      let garbage = SignedSpaceDeletion {
        payload: Base64Bytes::new(b"{".to_vec()),
        signature: Base64Bytes::new(Vec::new()),
      };
      assert!(space_deletion_channels(&garbage).is_empty());
    }

    fn world_with_workspace_admin() -> (Person, World) {
      let admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&admin, RosterRole::Admin);
      (admin, world)
    }
  }

  mod accepted {
    use super::*;

    #[test]
    fn a_signed_channel_admin_in_the_group() {
      let (admin, world) = world_with_channel_admin();
      let verified = world.verify(&admin.deletion(EPOCH)).unwrap();
      assert_eq!(
        (verified.signer, verified.is_workspace_admin),
        (admin.user_id, false)
      );
      assert_eq!(verified.device.device_id(), admin.device.public_keys().device_id);
    }

    #[test]
    fn a_workspace_admin_outside_the_group() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      let verified = world.verify(&workspace_admin.deletion(EPOCH)).unwrap();
      assert_eq!(
        (verified.signer, verified.is_workspace_admin),
        (workspace_admin.user_id, true)
      );
    }

    #[test]
    fn the_workspace_owner() {
      let owner = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&owner, RosterRole::Owner);
      assert!(world.verify(&owner.deletion(EPOCH)).is_ok());
    }

    /// A device behind the group (it slept) still takes a deletion for a later epoch.
    #[test]
    fn a_device_behind_the_signed_epoch() {
      let (admin, world) = world_with_channel_admin();
      assert!(world.verify(&admin.deletion(EPOCH + 40)).is_ok());
    }
  }

  mod refused {
    use super::*;

    /// A member and the server made up a founding entry (a channel from before signed roles, or a
    /// device with no role state): a founder's admins count for roles, not for everybody's history.
    #[test]
    fn an_admin_of_a_snapshot_only_a_founder_vouches_for() {
      let (admin, mut world) = world_with_channel_admin();
      world.roles.is_workspace_anchored = false;
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    #[test]
    fn an_old_signature_and_one_dated_ahead() {
      let (admin, mut world) = world_with_channel_admin();
      let old = admin.deletion_at(EPOCH, NOW_MS - MAX_SIGNATURE_AGE_MS - 1);
      assert_eq!(
        world.verify(&old).map(|_| ()),
        Err(SignedDeletionError::OutOfDate)
      );
      let ahead = admin.deletion_at(EPOCH, NOW_MS + CLOCK_SKEW_MS + 1);
      assert_eq!(
        world.verify(&ahead).map(|_| ()),
        Err(SignedDeletionError::OutOfDate),
        "or it would never get old"
      );
      // A clock that is a bit off is no reason.
      assert!(
        world
          .verify(&admin.deletion_at(EPOCH, NOW_MS + CLOCK_SKEW_MS))
          .is_ok()
      );
      assert!(
        world
          .verify(&admin.deletion_at(EPOCH, NOW_MS - MAX_SIGNATURE_AGE_MS))
          .is_ok()
      );
      world.now_ms = NOW_MS + 1;
      assert!(world.verify(&admin.deletion(EPOCH)).is_ok());
    }

    #[test]
    fn any_alarm_or_unexplained_head() {
      let (admin, world) = world_with_channel_admin();
      let head = ChannelRolesHead {
        version: 9,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      };
      let mut cases = Vec::new();
      let mut with = World::with_channel_admin_of(&admin, &world);
      with.roles.maybe_alarm = Some("forked".into());
      cases.push(with);
      let mut with = World::with_channel_admin_of(&admin, &world);
      with.roles.maybe_ahead = Some(head);
      cases.push(with);
      let mut with = World::with_channel_admin_of(&admin, &world);
      with.workspace.maybe_alarm = Some("forked".into());
      cases.push(with);
      for world in cases {
        assert_eq!(
          world.verify(&admin.deletion(EPOCH)).map(|_| ()),
          Err(SignedDeletionError::Alarm)
        );
      }
    }

    /// Unchained, with no signing time: other uses take it as fresh, a purge doesn't.
    #[test]
    fn a_workspace_admin_on_a_roster_without_a_signing_time() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      world.workspace.heads.maybe_roster_signed_at_ms = None;
      world.workspace.heads.is_roster_chained = false;
      assert!(world.workspace.is_roster_fresh(NOW_MS, MAX_AGE_MS));
      assert_eq!(
        world.verify(&workspace_admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    #[test]
    fn a_member_and_a_stranger_sign_nothing() {
      let (_, world) = world_with_channel_admin();
      for signer in [Person::new(2), Person::new(3)] {
        assert_eq!(
          world.verify(&signer.deletion(EPOCH)).map(|_| ()),
          Err(SignedDeletionError::Unauthorized)
        );
      }
    }

    #[test]
    fn a_channel_nobody_signed_roles_for_has_no_admin() {
      let somebody = Person::new(1);
      assert_eq!(
        World::default().verify(&somebody.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// The server's view of roles doesn't enter: the snapshot this device accepted decides.
    #[test]
    fn a_demoted_admin() {
      let (admin, mut world) = world_with_channel_admin();
      let successor = Person::new(2);
      world.roles.assignments = vec![successor.assignment(ChannelRole::Admin)];
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    #[test]
    fn a_moderator() {
      let (_, mut world) = world_with_channel_admin();
      let moderator = Person::new(2);
      world
        .roles
        .assignments
        .push(moderator.assignment(ChannelRole::Moderator));
      assert_eq!(
        world.verify(&moderator.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// A demotion the server holds back, of an admin who left the group: the group decides too.
    #[test]
    fn a_channel_admin_who_is_no_longer_in_the_group() {
      let (admin, mut world) = world_with_channel_admin();
      world.group_users.retain(|user| *user != admin.user_id);
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// The same user with another key: the snapshot's key decides, not what the certificate says.
    #[test]
    fn an_impostor_with_the_admins_user_id() {
      let (admin, world) = world_with_channel_admin();
      let impostor = Person::new(admin.user_id.snowflake().get());
      assert_eq!(
        world.verify(&impostor.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    #[test]
    fn a_workspace_admin_on_a_stale_roster() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      world.now_ms = NOW_MS + MAX_AGE_MS + 1;
      assert_eq!(
        world
          .verify(&workspace_admin.deletion_at(EPOCH, world.now_ms))
          .map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
      // A clock set back here doesn't help when the signer's clock shows the roster was stale.
      world.now_ms = NOW_MS + MAX_AGE_MS + 1 - CLOCK_SKEW_MS;
      let signed_later = workspace_admin.deletion_at(EPOCH, NOW_MS + MAX_AGE_MS + 1);
      assert_eq!(
        world.verify(&signed_later).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    #[test]
    fn a_workspace_admin_gone_from_the_roster() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      world.workspace.roster.members.clear();
      assert_eq!(
        world.verify(&workspace_admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// Replayed: held back by the server, and the group moved on since.
    #[test]
    fn an_old_action_from_before_the_groups_epoch() {
      let (admin, world) = world_with_channel_admin();
      assert_eq!(
        world.verify(&admin.deletion(EPOCH - 1)).map(|_| ()),
        Err(SignedDeletionError::OldEpoch {
          signed: EPOCH - 1,
          local: EPOCH,
        })
      );
    }

    #[test]
    fn a_device_without_the_group() {
      let (admin, mut world) = world_with_channel_admin();
      world.has_group = false;
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::NoGroup)
      );
    }

    #[test]
    fn an_action_for_another_conversation() {
      let (admin, world) = world_with_channel_admin();
      let other = admin.deletion_in(CHANNEL + 1, EPOCH, NOW_MS);
      assert_eq!(
        world.verify(&other).map(|_| ()),
        Err(SignedDeletionError::OtherConversation)
      );
    }

    /// Direct messages and notes to self have no signed roles, so no signature purges them: not
    /// even a workspace admin's, which a server might have tricked out of a client.
    #[test]
    fn a_direct_message() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      world.is_channel = false;
      assert_eq!(
        world.verify(&workspace_admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::NotAChannel)
      );
    }

    #[test]
    fn unsigned_tampered_and_moved_signatures() {
      let (admin, world) = world_with_channel_admin();
      let mut unsigned = admin.deletion(EPOCH);
      unsigned.signature = Base64Bytes::new(Vec::new());
      assert_eq!(
        world.verify(&unsigned).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );

      let mut raised = admin.deletion(EPOCH);
      let mut document: ChannelDeletionDocument = serde_json::from_slice(raised.payload.as_slice()).unwrap();
      document.epoch += 1;
      raised.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
      assert_eq!(
        world.verify(&raised).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );

      let mut junk = admin.deletion(EPOCH);
      junk.payload = Base64Bytes::new(b"not a document".to_vec());
      assert_eq!(world.verify(&junk).map(|_| ()), Err(SignedDeletionError::Payload));

      // Signed by the admin, for the channel's role log: no deletion.
      let mut other_log = admin.deletion(EPOCH);
      other_log.signature = Base64Bytes::new(
        sign_log_entry(
          &admin.device,
          &admin.certificate,
          WorkspaceLog::ChannelRoles,
          EPOCH,
          other_log.payload.as_slice(),
        )
        .unwrap(),
      );
      assert_eq!(
        world.verify(&other_log).map(|_| ()),
        Err(SignedDeletionError::Unauthorized)
      );
    }
  }

  #[test]
  fn a_member_signs_only_near_its_own_epoch() {
    assert!(is_told_epoch_plausible(5, 5));
    assert!(is_told_epoch_plausible(5, 5 + MAX_CHANGE_AGE_EPOCHS));
    assert!(
      !is_told_epoch_plausible(5, 5 + MAX_CHANGE_AGE_EPOCHS + 1),
      "a token for later"
    );
    assert!(!is_told_epoch_plausible(5, u64::MAX));
    assert!(!is_told_epoch_plausible(5, 4), "below what this device merged");
  }

  /// The time limit on a channel admin's purge (e2ee.md §4.6.3, part 3): a demotion the server holds
  /// back purges for at most `channel_roles_max_age` after somebody else last signed.
  mod time_limit {
    use super::*;

    #[test]
    fn somebody_elses_entry_within_the_limit_purges() {
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![appointed_at(NOW_MS - ROLES_MAX_AGE_MS)];
      assert!(world.verify(&admin.deletion(EPOCH)).is_ok());
    }

    /// The admin's own signature says nothing about whether they are still one.
    #[test]
    fn only_the_deleting_admins_own_entries_take_the_retention() {
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![RecentSigner {
        user_id: admin.user_id,
        from_ms: NOW_MS,
      }];
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::NoRecentOtherSigner)
      );
    }

    /// A quiet channel, or another admin away for longer than the limit.
    #[test]
    fn a_stale_other_signature_takes_the_retention() {
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![appointed_at(NOW_MS - ROLES_MAX_AGE_MS - 1)];
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::NoRecentOtherSigner)
      );
    }

    /// A state from before signers were recorded.
    #[test]
    fn no_record_takes_the_retention() {
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers.clear();
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)).map(|_| ()),
        Err(SignedDeletionError::NoRecentOtherSigner)
      );
    }

    /// Each of the three times being ahead can only refuse.
    #[test]
    fn each_clock_ahead_refuses() {
      let edge = appointed_at(NOW_MS - ROLES_MAX_AGE_MS);
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![edge.clone()];
      world.now_ms = NOW_MS + 1;
      assert!(
        world.verify(&admin.deletion(EPOCH)).is_err(),
        "this device's clock"
      );
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![edge.clone()];
      assert!(
        world.verify(&admin.deletion_at(EPOCH, NOW_MS + 1)).is_err(),
        "the deletion's signing time"
      );
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![edge];
      world.newest_server_ms = NOW_MS + 1;
      assert!(world.verify(&admin.deletion(EPOCH)).is_err(), "the server's time");
      // A server holding its time back gains nothing.
      let (admin, mut world) = world_with_channel_admin();
      world.roles.recent_signers = vec![appointed_at(NOW_MS - ROLES_MAX_AGE_MS - 1)];
      world.newest_server_ms = NOW_MS - ROLES_MAX_AGE_MS;
      assert!(world.verify(&admin.deletion(EPOCH)).is_err());
    }

    /// A workspace admin's signature stays under the roster's window only.
    #[test]
    fn a_workspace_admin_needs_no_other_signer() {
      let workspace_admin = Person::new(8);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      assert!(world.verify(&workspace_admin.deletion(EPOCH)).is_ok());
    }
  }

  #[test]
  fn only_an_authority_every_member_checks_may_sign() {
    assert!(!can_sign_deletion(&[]));
    assert!(!can_sign_deletion(&[SignerAuthority::Founder]));
    assert!(can_sign_deletion(&[
      SignerAuthority::ChannelAdmin,
      SignerAuthority::Founder
    ]));
    assert!(can_sign_deletion(&[SignerAuthority::Workspace {
      is_owner: false
    }]));
  }

  /// The record of the appointer's entry, accepted at `from_ms`.
  fn appointed_at(from_ms: i64) -> RecentSigner {
    RecentSigner {
      user_id: user_id(APPOINTER),
      from_ms,
    }
  }

  fn world_with_channel_admin() -> (Person, World) {
    let admin = Person::new(1);
    let mut world = World::default();
    world.roles.version = 1;
    world.roles.is_workspace_anchored = true;
    world.roles.assignments = vec![admin.assignment(ChannelRole::Admin)];
    world.roles.recent_signers = vec![appointed_at(NOW_MS)];
    world.group_users = vec![admin.user_id, user_id(2)];
    (admin, world)
  }

  /// What a verifying device holds.
  struct World {
    roles: ChannelRoleState,
    workspace: WorkspaceState,
    group_users: Vec<UserId>,
    has_group: bool,
    is_channel: bool,
    now_ms: i64,
    newest_server_ms: i64,
    /// The channel's verified anchor and the name the server lists.
    maybe_anchor: Option<(VerifiedAnchor, String)>,
    is_anchor_forked: bool,
    is_anchor_unverifiable: bool,
  }

  impl Default for World {
    fn default() -> Self {
      Self {
        roles: ChannelRoleState::default(),
        workspace: WorkspaceState {
          heads: LogHeads {
            is_roster_chained: true,
            maybe_roster_signed_at_ms: Some(NOW_MS),
            ..LogHeads::default()
          },
          ..WorkspaceState::default()
        },
        group_users: Vec::new(),
        has_group: true,
        is_channel: true,
        now_ms: NOW_MS,
        newest_server_ms: i64::MIN,
        maybe_anchor: None,
        is_anchor_forked: false,
        is_anchor_unverifiable: false,
      }
    }
  }

  impl World {
    fn with_channel_admin_of(admin: &Person, like: &World) -> Self {
      let mut world = World::default();
      world.roles.version = 1;
      world.roles.is_workspace_anchored = true;
      world.roles.assignments = vec![admin.assignment(ChannelRole::Admin)];
      world.roles.recent_signers = like.roles.recent_signers.clone();
      world.group_users = like.group_users.clone();
      world
    }

    fn roster_admin(&mut self, person: &Person, role: RosterRole) {
      self.workspace.roster = RosterDocument {
        version: 1,
        members: vec![RosterMember {
          user_id: person.user_id,
          role,
          identity_key: person.key(),
        }],
        ..RosterDocument::default()
      };
    }

    /// Founds the channel anchored under `name` (by a founder of its own) and lists it as `listed`.
    fn anchor_founded_as(&mut self, name: &str, listed: &str) {
      use teamcraft_crypto::channel_anchor::{
        AnchorExpectation, AnchorKind, ChannelAnchor, ExpectedKind, Founding, Judged, Standing,
      };
      use teamcraft_crypto::membership::{MembershipAction, MembershipAuthority};

      struct Founder(crypto::UserId, IdentityPublicKey);
      impl IdentityDirectory for Founder {
        fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
          (user_id == self.0).then_some(self.1)
        }
        fn device_listing(&self, _: &VerifiedDevice, _: u64, _: Judged) -> Standing {
          Standing::Confirmed
        }
      }
      struct Nobody;
      impl MembershipAuthority for Nobody {
        fn is_authorized(&self, _: crypto::UserId, _: crypto::ConversationId, _: &MembershipAction) -> bool {
          false
        }
      }

      let founder = Person::new(3);
      let conversation_id = convert::conversation(conversation(CHANNEL));
      let anchor = ChannelAnchor::found(
        &founder.device,
        &founder.certificate,
        &Founding {
          conversation_id,
          kind: AnchorKind::Channel,
          device_list_version: 1,
          signed_at_ms: 1,
          maybe_space_id: None,
          name,
        },
      )
      .unwrap();
      let directory = Founder(convert::user(founder.user_id), founder.identity.public_key());
      let verified = anchor
        .verify(
          &AnchorExpectation {
            conversation_id,
            kind: ExpectedKind::Channel,
          },
          EPOCH,
          &Default::default(),
          &directory,
          &Nobody,
        )
        .unwrap();
      self.maybe_anchor = Some((verified, listed.to_owned()));
    }

    /// Founds the channel anchored in `space` under `name` (by a founder of its own), lists it as
    /// `listed`, and with `maybe_change` has a member (no workspace admin) move or rename it.
    fn anchor_in(&mut self, space: i64, name: &str, listed: &str, maybe_change: Option<(Option<i64>, &str)>) {
      use std::collections::BTreeSet;

      use teamcraft_crypto::channel_anchor::{
        AnchorExpectation, AnchorKind, ChannelAnchor, ExpectedKind, Founding, Judged, NameAuthority,
        PropertiesTarget, Standing,
      };
      use teamcraft_crypto::membership::{MembershipAction, MembershipAuthority};

      struct People(Vec<(crypto::UserId, IdentityPublicKey)>);
      impl IdentityDirectory for People {
        fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
          self.0.iter().find(|(id, _)| *id == user_id).map(|(_, key)| *key)
        }
        fn device_listing(&self, _: &VerifiedDevice, _: u64, _: Judged) -> Standing {
          Standing::Confirmed
        }
      }
      struct Nobody;
      impl MembershipAuthority for Nobody {
        fn is_authorized(&self, _: crypto::UserId, _: crypto::ConversationId, _: &MembershipAction) -> bool {
          false
        }
      }

      let founder = Person::new(3);
      let member = Person::new(4);
      let conversation_id = convert::conversation(conversation(CHANNEL));
      let mut anchor = ChannelAnchor::found(
        &founder.device,
        &founder.certificate,
        &Founding {
          conversation_id,
          kind: AnchorKind::Channel,
          device_list_version: 1,
          signed_at_ms: 1,
          maybe_space_id: Some(convert::space(space_id(space))),
          name,
        },
      )
      .unwrap();
      if let Some((maybe_space, new_name)) = maybe_change {
        let change = anchor
          .sign_change(
            &member.device,
            &member.certificate,
            NameAuthority::Member,
            &PropertiesTarget {
              maybe_space_id: maybe_space.map(|space| convert::space(space_id(space))),
              name: new_name,
            },
            EPOCH,
            0,
          )
          .unwrap();
        anchor = anchor.with_change(change);
      }
      let people = People(vec![
        (convert::user(founder.user_id), founder.identity.public_key()),
        (convert::user(member.user_id), member.identity.public_key()),
      ]);
      let tree: BTreeSet<crypto::UserId> = people.0.iter().map(|(id, _)| *id).collect();
      let verified = anchor
        .verify(
          &AnchorExpectation {
            conversation_id,
            kind: ExpectedKind::Channel,
          },
          EPOCH,
          &tree,
          &people,
          &Nobody,
        )
        .unwrap();
      self.maybe_anchor = Some((verified, listed.to_owned()));
    }

    fn verify_space(&self, signed: &SignedSpaceDeletion) -> Result<VerifiedDeletion, SignedDeletionError> {
      verify_signed_space_deletion(
        signed,
        &DeletionVerifier {
          conversation: conversation(CHANNEL),
          is_channel: self.is_channel,
          maybe_group: self.has_group.then_some(GroupFacts {
            epoch: EPOCH,
            users: &self.group_users,
          }),
          roles: &self.roles,
          workspace: &self.workspace,
          roster_max_age_ms: MAX_AGE_MS,
          max_age_ms: MAX_SIGNATURE_AGE_MS,
          clock_skew_ms: CLOCK_SKEW_MS,
          now_ms: self.now_ms,
          channel_roles_max_age_ms: ROLES_MAX_AGE_MS,
          newest_server_ms: self.newest_server_ms,
          anchor: self.anchor_facts(),
        },
      )
    }

    fn anchor_facts(&self) -> AnchorFacts<'_> {
      match &self.maybe_anchor {
        _ if self.is_anchor_unverifiable => AnchorFacts::Unverifiable,
        None => AnchorFacts::Unanchored,
        Some((anchor, listed)) => AnchorFacts::Anchored {
          anchor,
          floor_grants: !self.is_anchor_forked,
          maybe_listed_name: Some(listed),
        },
      }
    }

    fn verify(&self, signed: &SignedChannelDeletion) -> Result<VerifiedDeletion, SignedDeletionError> {
      verify_signed_deletion(
        signed,
        &DeletionVerifier {
          conversation: conversation(CHANNEL),
          is_channel: self.is_channel,
          maybe_group: self.has_group.then_some(GroupFacts {
            epoch: EPOCH,
            users: &self.group_users,
          }),
          roles: &self.roles,
          workspace: &self.workspace,
          roster_max_age_ms: MAX_AGE_MS,
          max_age_ms: MAX_SIGNATURE_AGE_MS,
          clock_skew_ms: CLOCK_SKEW_MS,
          now_ms: self.now_ms,
          channel_roles_max_age_ms: ROLES_MAX_AGE_MS,
          newest_server_ms: self.newest_server_ms,
          anchor: self.anchor_facts(),
        },
      )
    }
  }

  struct Person {
    user_id: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  impl Person {
    fn new(id: i64) -> Self {
      let user_id = user_id(id);
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(
        convert::user(user_id),
        crypto::DeviceId::new(id.unsigned_abs() * 100),
        &mut OsRng,
      );
      let certificate = identity
        .certify_device(convert::user(user_id), &device.public_keys())
        .unwrap();
      Self {
        user_id,
        identity,
        device,
        certificate,
      }
    }

    fn key(&self) -> Base64Bytes {
      Base64Bytes::new(self.identity.public_key().as_bytes().to_vec())
    }

    fn assignment(&self, role: ChannelRole) -> ChannelRoleAssignment {
      ChannelRoleAssignment {
        user_id: self.user_id,
        role,
        identity_key: self.key(),
      }
    }

    fn deletion(&self, epoch: u64) -> SignedChannelDeletion {
      self.deletion_at(epoch, NOW_MS)
    }

    fn deletion_at(&self, epoch: u64, signed_at_ms: i64) -> SignedChannelDeletion {
      self.deletion_in(CHANNEL, epoch, signed_at_ms)
    }

    fn deletion_in(&self, channel: i64, epoch: u64, signed_at_ms: i64) -> SignedChannelDeletion {
      sign_channel_deletion(
        &self.device,
        &self.certificate,
        conversation(channel),
        epoch,
        signed_at_ms,
        None,
      )
      .unwrap()
    }

    /// A space's deletion naming `channels` (ID, epoch, name shown), at roster version 1.
    fn space_deletion(&self, space: i64, channels: &[(i64, u64, Option<&str>)]) -> SignedSpaceDeletion {
      self.space_deletion_with(space, 1, NOW_MS, channels)
    }

    fn space_deletion_with(
      &self,
      space: i64,
      roster_version: u64,
      signed_at_ms: i64,
      channels: &[(i64, u64, Option<&str>)],
    ) -> SignedSpaceDeletion {
      let channels = channels
        .iter()
        .map(|(channel, epoch, maybe_name)| SpaceDeletionChannel {
          conversation_id: conversation(*channel),
          epoch: *epoch,
          maybe_name_hash: maybe_name.map(|name| {
            Base64Bytes::new(
              teamcraft_crypto::channel_anchor::name_hash(name)
                .as_bytes()
                .to_vec(),
            )
          }),
        })
        .collect();
      sign_space_deletion(
        &self.device,
        &self.certificate,
        space_id(space),
        roster_version,
        channels,
        signed_at_ms,
      )
      .unwrap()
    }

    /// A deletion naming the name the deleter was shown (D3).
    fn deletion_named(&self, epoch: u64, name: &str) -> SignedChannelDeletion {
      sign_channel_deletion(
        &self.device,
        &self.certificate,
        conversation(CHANNEL),
        epoch,
        NOW_MS,
        Some(teamcraft_crypto::channel_anchor::name_hash(name)),
      )
      .unwrap()
    }
  }

  fn user_id(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn conversation(id: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn space_id(id: i64) -> SpaceId {
    SpaceId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
