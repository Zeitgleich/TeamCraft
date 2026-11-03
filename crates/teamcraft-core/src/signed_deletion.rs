//! Signed deletions of channels (issue #1135, e2ee.md §4.6). A device drops a channel's history at
//! once only for a deletion it verifies itself, against signed data alone: the workspace roster and
//! the channel's signed roles, as this device verified them, and its own MLS group. Anything else,
//! whatever the server says, is a claim, which hides the conversation and keeps its history
//! (`client::server_deletions`).
//!
//! Who may sign:
//!
//! - **An owner or admin of the verified roster**, fresh now and when the deletion was signed.
//! - **An admin of the signed snapshot** this device accepted last, verified against the key that
//!   snapshot names, who is still in this device's group.
//!
//! The deletion names the channel and the group's epoch, which the server takes only as the current
//! one. So a signed deletion the server held back, for example after refusing it, is refused here
//! as soon as the group has moved on; the group's epoch is also why a device without the group
//! judges nothing (it can't tell how old the deletion is).

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::{DeviceKeyPair, SignedDeviceCertificate};
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
  /// This device's time.
  pub now_ms: i64,
}

/// What this device's MLS group says.
#[derive(Clone, Copy, Debug)]
pub struct GroupFacts<'a> {
  pub epoch: u64,
  pub users: &'a [UserId],
}

/// A deletion that holds: who signed it, on what authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedDeletion {
  pub signer: UserId,
  pub is_workspace_admin: bool,
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
  #[error("the signature doesn't verify for anybody allowed to delete this channel")]
  Unauthorized,
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
  verified_by_workspace_admin(signed, &document, verifier)
    .or_else(|| verified_by_channel_admin(signed, &document, verifier, group))
    .ok_or(SignedDeletionError::Unauthorized)
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

/// The roster has to be fresh at the later of this device's time and the signer's: a clock wrong
/// either way can only refuse.
fn verified_by_workspace_admin(
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  verifier: &DeletionVerifier<'_>,
) -> Option<VerifiedDeletion> {
  let at_ms = verifier.now_ms.max(document.signed_at_ms);
  if !verifier
    .workspace
    .is_roster_fresh(at_ms, verifier.roster_max_age_ms)
  {
    return None;
  }
  let admins = KeyDirectory::from_keys(roster_admin_keys(verifier.workspace));
  let signer = verified_signer(signed, document, &admins)?;
  verifier
    .workspace
    .roster
    .members
    .iter()
    .any(|member| member.user_id == signer && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .then_some(VerifiedDeletion {
      signer,
      is_workspace_admin: true,
    })
}

fn verified_by_channel_admin(
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  verifier: &DeletionVerifier<'_>,
  group: GroupFacts<'_>,
) -> Option<VerifiedDeletion> {
  let admins = KeyDirectory::from_assignments(
    verifier
      .roles
      .assignments
      .iter()
      .filter(|assignment| assignment.role == ChannelRole::Admin),
  );
  let signer = verified_signer(signed, document, &admins)?;
  group.users.contains(&signer).then_some(VerifiedDeletion {
    signer,
    is_workspace_admin: false,
  })
}

fn verified_signer(
  signed: &SignedChannelDeletion,
  document: &ChannelDeletionDocument,
  directory: &KeyDirectory,
) -> Option<UserId> {
  let device = verify_log_entry(
    signed.signature.as_slice(),
    WorkspaceLog::ChannelDeletion,
    document.epoch,
    signed.payload.as_slice(),
    directory,
  )
  .ok()?;
  convert::user_from_crypto(device.user_id()).ok()
}

/// What this device can sign: only an authority every member can check. A channel nothing was signed
/// for has no admin yet, and founding its roles in the moment of deleting it would be worth nothing.
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
  use crate::workspace::{LogHeads, RosterDocument, RosterMember};

  const CHANNEL: i64 = 900;
  const EPOCH: u64 = 5;
  const NOW_MS: i64 = 1_000_000;
  const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

  mod accepted {
    use super::*;

    #[test]
    fn a_signed_channel_admin_in_the_group() {
      let (admin, world) = world_with_channel_admin();
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)),
        Ok(VerifiedDeletion {
          signer: admin.user_id,
          is_workspace_admin: false,
        })
      );
    }

    #[test]
    fn a_workspace_admin_outside_the_group() {
      let workspace_admin = Person::new(9);
      let mut world = World::default();
      world.roster_admin(&workspace_admin, RosterRole::Admin);
      assert_eq!(
        world.verify(&workspace_admin.deletion(EPOCH)),
        Ok(VerifiedDeletion {
          signer: workspace_admin.user_id,
          is_workspace_admin: true,
        })
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

    #[test]
    fn a_member_and_a_stranger_sign_nothing() {
      let (_, world) = world_with_channel_admin();
      for signer in [Person::new(2), Person::new(3)] {
        assert_eq!(
          world.verify(&signer.deletion(EPOCH)),
          Err(SignedDeletionError::Unauthorized)
        );
      }
    }

    #[test]
    fn a_channel_nobody_signed_roles_for_has_no_admin() {
      let somebody = Person::new(1);
      assert_eq!(
        World::default().verify(&somebody.deletion(EPOCH)),
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
        world.verify(&admin.deletion(EPOCH)),
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
        world.verify(&moderator.deletion(EPOCH)),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// A demotion the server holds back, of an admin who left the group: the group decides too.
    #[test]
    fn a_channel_admin_who_is_no_longer_in_the_group() {
      let (admin, mut world) = world_with_channel_admin();
      world.group_users.retain(|user| *user != admin.user_id);
      assert_eq!(
        world.verify(&admin.deletion(EPOCH)),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// The same user with another key: the snapshot's key decides, not what the certificate says.
    #[test]
    fn an_impostor_with_the_admins_user_id() {
      let (admin, world) = world_with_channel_admin();
      let impostor = Person::new(admin.user_id.snowflake().get());
      assert_eq!(
        world.verify(&impostor.deletion(EPOCH)),
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
        world.verify(&workspace_admin.deletion(EPOCH)),
        Err(SignedDeletionError::Unauthorized)
      );
      // A clock set back here doesn't help when the signer's clock shows the roster was stale.
      world.now_ms = NOW_MS;
      let signed_later = workspace_admin.deletion_at(EPOCH, NOW_MS + MAX_AGE_MS + 1);
      assert_eq!(
        world.verify(&signed_later),
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
        world.verify(&workspace_admin.deletion(EPOCH)),
        Err(SignedDeletionError::Unauthorized)
      );
    }

    /// Replayed: held back by the server, and the group moved on since.
    #[test]
    fn an_old_action_from_before_the_groups_epoch() {
      let (admin, world) = world_with_channel_admin();
      assert_eq!(
        world.verify(&admin.deletion(EPOCH - 1)),
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
        world.verify(&admin.deletion(EPOCH)),
        Err(SignedDeletionError::NoGroup)
      );
    }

    #[test]
    fn an_action_for_another_conversation() {
      let (admin, world) = world_with_channel_admin();
      let other = admin.deletion_in(CHANNEL + 1, EPOCH, NOW_MS);
      assert_eq!(world.verify(&other), Err(SignedDeletionError::OtherConversation));
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
        world.verify(&workspace_admin.deletion(EPOCH)),
        Err(SignedDeletionError::NotAChannel)
      );
    }

    #[test]
    fn unsigned_tampered_and_moved_signatures() {
      let (admin, world) = world_with_channel_admin();
      let mut unsigned = admin.deletion(EPOCH);
      unsigned.signature = Base64Bytes::new(Vec::new());
      assert_eq!(world.verify(&unsigned), Err(SignedDeletionError::Unauthorized));

      let mut raised = admin.deletion(EPOCH);
      let mut document: ChannelDeletionDocument = serde_json::from_slice(raised.payload.as_slice()).unwrap();
      document.epoch += 1;
      raised.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
      assert_eq!(world.verify(&raised), Err(SignedDeletionError::Unauthorized));

      let mut junk = admin.deletion(EPOCH);
      junk.payload = Base64Bytes::new(b"not a document".to_vec());
      assert_eq!(world.verify(&junk), Err(SignedDeletionError::Payload));

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
      assert_eq!(world.verify(&other_log), Err(SignedDeletionError::Unauthorized));
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

  fn world_with_channel_admin() -> (Person, World) {
    let admin = Person::new(1);
    let mut world = World::default();
    world.roles.version = 1;
    world.roles.assignments = vec![admin.assignment(ChannelRole::Admin)];
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
      }
    }
  }

  impl World {
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
          now_ms: self.now_ms,
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
