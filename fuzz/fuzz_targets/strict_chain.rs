//! A strict channel's roles chain as the server hands it to a device that fell out (e2ee.md
//! §4.6.2, "Falling out in a strict channel", `GET …/mls/strict?after=`): verifying it never
//! panics, and no chain of random links moves the known roles, however the server numbers them.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_crypto::channel_anchor::{Judged, Standing};
use teamcraft_crypto::identity::{IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::ids::{ConversationId, UserId};
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};
use teamcraft_crypto::strict_roles::{ChainEnd, ChainLink, StrictRoles, verify_chain};

/// Every user has the same pinned key, every device is listed and every signer is a workspace
/// admin, so fuzzed links reach the signature checks.
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

fuzz_target!(|input: (Vec<u8>, Vec<(u64, Vec<u8>)>, [u8; 32])| {
  let (known_bytes, links, key) = input;
  let Ok(known) = StrictRoles::from_bytes(&known_bytes) else {
    return;
  };
  let everyone = Everyone(key);
  let chain: Vec<ChainLink<'_>> = links
    .iter()
    .map(|(version, change)| ChainLink {
      version: *version,
      change,
    })
    .collect();
  // Random links are passed over or refuse the chain: none verifies, so it never moves.
  match verify_chain(&known, &chain, &everyone, &everyone) {
    Ok(end) => assert_eq!(end, ChainEnd::Roles(known), "random links moved the roles"),
    Err(_) => assert!(!links.is_empty(), "an empty chain was refused"),
  }
});
