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
//! The deletion names the channel, the group's epoch, which the server takes only as the current one,
//! and when it was signed. So a signature the server held back, for example after refusing it, is
//! refused here once the group has moved on or it got too old. A device without the group judges
//! nothing, and neither does one that sees a sign of being shown different logs.

use teamcraft_crypto::identity::{DeviceKeyPair, IdentityPublicKey, SignedDeviceCertificate, VerifiedDevice};
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::mls::MAX_CHANGE_AGE_EPOCHS;
use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry, verify_log_entry};
use teamcraft_proto::{
  Base64Bytes, ChannelDeletionDocument, ChannelRole, ConversationId, SignedChannelDeletion, UserId,
};

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
  if let Some(verified) = verified_by_workspace_admin(signed, &document, verifier) {
    return Ok(verified);
  }
  let verified =
    verified_by_channel_admin(signed, &document, verifier, group).ok_or(SignedDeletionError::Unauthorized)?;
  if !is_vouched_for_lately(&verified, &document, verifier) {
    return Err(SignedDeletionError::NoRecentOtherSigner);
  }
  Ok(verified)
}

/// The signed deletion of `conversation` for the group's `epoch`. Who may sign is up to the members'
/// devices.
pub(crate) fn sign_channel_deletion(
  device: &DeviceKeyPair,
  certificate: &SignedDeviceCertificate,
  conversation: ConversationId,
  epoch: u64,
  signed_at_ms: i64,
) -> Result<SignedChannelDeletion, CoreError> {
  let document = ChannelDeletionDocument {
    conversation_id: conversation,
    epoch,
    signed_at_ms,
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
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  verifier: &DeletionVerifier<'_>,
) -> Option<VerifiedDeletion> {
  let at_ms = verifier.now_ms.max(document.signed_at_ms);
  if verifier.workspace.heads.maybe_roster_signed_at_ms.is_none()
    || !verifier
      .workspace
      .is_roster_fresh(at_ms, verifier.roster_max_age_ms)
  {
    return None;
  }
  let admins = KeyDirectory::from_keys(roster_admin_keys(verifier.workspace));
  let verified = verified_signer(signed, document, &admins, true)?;
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
  let verified = verified_signer(signed, document, &admins, false)?;
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

fn verified_signer(
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  directory: &KeyDirectory,
  is_workspace_admin: bool,
) -> Option<VerifiedDeletion> {
  let device = verify_log_entry(
    signed.signature.as_slice(),
    WorkspaceLog::ChannelDeletion,
    document.epoch,
    signed.payload.as_slice(),
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
  const EPOCH: u64 = 5;
  const NOW_MS: i64 = 1_000_000;
  const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
  const MAX_SIGNATURE_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;
  const CLOCK_SKEW_MS: i64 = 60 * 60 * 1000;
  const ROLES_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
  /// Who signed the entry that made the channel admin one, in the test worlds.
  const APPOINTER: i64 = 9;

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
}
