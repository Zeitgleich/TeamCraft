//! A strict channel's roles (e2ee.md §4.6.2) as a device reads them from an untrusted group
//! context, and a roles change from a commit's authenticated data: parsing never panics, stays
//! within the size limit, re-encodes to the same bytes, and random signatures never verify.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_crypto::channel_anchor::{Judged, Standing};
use teamcraft_crypto::identity::{IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::ids::{ConversationId, UserId};
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};
use teamcraft_crypto::strict_roles::{
  MAX_STRICT_ROLES, MAX_STRICT_ROLES_BYTES, SignedStrictRolesChange, StrictRoles,
};

/// Every user has the same pinned key and every signer is a workspace admin, so fuzzed input
/// reaches the signature checks.
struct Everyone([u8; 32]);

impl IdentityDirectory for Everyone {
  fn pinned_identity(&self, _: UserId) -> Option<IdentityPublicKey> {
    Some(IdentityPublicKey::from_bytes(self.0))
  }

  fn device_listing(&self, _: &VerifiedDevice, _: u64, _: Judged) -> Standing {
    Standing::Confirmed
  }
}

impl MembershipAuthority for Everyone {
  fn is_authorized(&self, _: UserId, _: ConversationId, _: &MembershipAction) -> bool {
    true
  }

  fn workspace_signer(&self, _: UserId, _: u64, _: Judged) -> Standing {
    Standing::Confirmed
  }
}

fuzz_target!(|input: (Vec<u8>, [u8; 32], u64)| {
  let (bytes, key, epoch) = input;
  let everyone = Everyone(key);
  if let Ok(roles) = StrictRoles::from_bytes(&bytes) {
    assert!(bytes.len() <= MAX_STRICT_ROLES_BYTES);
    assert!(!roles.roles().is_empty() && roles.roles().len() <= MAX_STRICT_ROLES);
    assert_eq!(
      roles.to_bytes().ok().as_deref(),
      Some(bytes.as_slice()),
      "roles re-encode differently"
    );
    let _ = roles.hash();
    let change = roles.last_change();
    let verdict = StrictRoles::check_change(
      None,
      change,
      change.change().conversation_id,
      epoch,
      &[],
      &everyone,
      &everyone,
    );
    assert!(verdict.is_err(), "random bytes verified as a signed roles change");
  }
  if let Ok(change) = SignedStrictRolesChange::from_bytes(&bytes) {
    assert_eq!(
      change.to_bytes().ok().as_deref(),
      Some(bytes.as_slice()),
      "change re-encodes differently"
    );
    let _ = change.claimed_signer();
    let _ = change.change().hash();
  }
});
