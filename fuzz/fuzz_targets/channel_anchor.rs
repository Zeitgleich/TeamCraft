//! A channel's anchor (e2ee.md §4.6.5) as a device reads it from an untrusted group context, and a
//! properties change from a commit's authenticated data: parsing never panics, stays within the
//! size limit, re-encodes to the same bytes, and random signatures never verify.
#![no_main]

use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;
use teamcraft_crypto::channel_anchor::{
  AnchorExpectation, ChannelAnchor, DEFAULT_MAX_ANCHOR_BYTES, ExpectedKind, Judged, SignedPropertiesChange,
  Standing,
};
use teamcraft_crypto::identity::{IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::ids::{ConversationId, UserId};
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};

/// Every user has the same pinned key and every device and admin is confirmed, so fuzzed input
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
  if let Ok(anchor) = ChannelAnchor::from_bytes(&bytes, DEFAULT_MAX_ANCHOR_BYTES) {
    assert!(bytes.len() <= DEFAULT_MAX_ANCHOR_BYTES);
    assert_eq!(anchor.to_bytes().ok().as_deref(), Some(bytes.as_slice()), "anchor re-encodes differently");
    let expected = AnchorExpectation {
      conversation_id: anchor.founding().conversation_id,
      kind: ExpectedKind::Channel,
    };
    let tree: BTreeSet<UserId> = BTreeSet::new();
    assert!(
      anchor.verify(&expected, epoch, &tree, &everyone, &everyone).is_err(),
      "random bytes verified as a signed founding"
    );
    let _ = anchor.founding_hash();
  }
  if let Ok(change) = SignedPropertiesChange::from_bytes(&bytes) {
    assert_eq!(change.to_bytes().ok().as_deref(), Some(bytes.as_slice()), "change re-encodes differently");
    let _ = change.claimed_signer();
  }
});
