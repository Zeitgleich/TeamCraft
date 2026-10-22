//! Trust decisions the crypto layer asks for: whose identity keys are pinned, and who may change a
//! group's membership.

use std::collections::{BTreeMap, HashSet};

use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};
use teamcraft_proto::UserId;

use crate::convert;
use crate::records::PinnedIdentity;

/// Identity keys this device pinned (trust on first use, e2ee.md §3). A key the server changed later
/// is *not* used until the user accepts the change.
pub(crate) struct PinnedDirectory {
  keys: BTreeMap<crypto::UserId, IdentityPublicKey>,
}

/// Interim rule until the signed admin roster exists (admin.md §2.3): a membership change is
/// authorized if its signer is a current member of the MLS group. Members can't be added by the
/// server (their key packages must carry pinned certificates), but any member may add or remove
/// others; the server's permission check is the only finer-grained control for now.
pub(crate) struct GroupMemberAuthority {
  members: HashSet<crypto::UserId>,
}

impl PinnedDirectory {
  pub(crate) fn new(pinned: &BTreeMap<UserId, PinnedIdentity>) -> Self {
    let keys = pinned
      .iter()
      .map(|(user, identity)| {
        (
          convert::user(*user),
          IdentityPublicKey::from_bytes(identity.public_key),
        )
      })
      .collect();
    Self { keys }
  }
}

impl IdentityDirectory for PinnedDirectory {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    self.keys.get(&user_id).copied()
  }
}

impl GroupMemberAuthority {
  pub(crate) fn new(members: impl IntoIterator<Item = crypto::UserId>) -> Self {
    Self {
      members: members.into_iter().collect(),
    }
  }
}

impl MembershipAuthority for GroupMemberAuthority {
  fn is_authorized(
    &self,
    actor: crypto::UserId,
    _conversation_id: crypto::ConversationId,
    _action: &MembershipAction,
  ) -> bool {
    self.members.contains(&actor)
  }
}
