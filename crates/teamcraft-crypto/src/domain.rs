//! Domain separation for signatures and AEAD associated data.
//!
//! Every signed or authenticated byte string is `len(label) || label || body`, with a distinct label
//! per purpose, so a value produced for one purpose never verifies for another.
//!
//! The device key signs both TeamCraft structures and MLS messages (e2ee.md §2). MLS signs
//! `SignContent { label = "MLS 1.0 " + label, content }` (RFC 9420 §5.1.2); our labels all start
//! with `TeamCraft-v1 `, so the two encodings differ right after the length prefix and can't collide.

pub(crate) const DEVICE_CERTIFICATE: &str = "TeamCraft-v1 device certificate";
pub(crate) const IDENTITY_FINGERPRINT: &str = "TeamCraft-v1 identity fingerprint";
pub(crate) const MESSAGE_AAD: &str = "TeamCraft-v1 message";
pub(crate) const MESSAGE_SIGNATURE: &str = "TeamCraft-v1 message signature";
pub(crate) const ATTACHMENT_AAD: &str = "TeamCraft-v1 attachment chunk";
pub(crate) const KEY_BAG_AAD: &str = "TeamCraft-v1 key bag";
pub(crate) const BACKUP_KEY_WRAP_AAD: &str = "TeamCraft-v1 backup key wrap";
pub(crate) const PASSKEY_PRF_KDF: &str = "TeamCraft-v1 passkey prf backup key wrap";
pub(crate) const ACCOUNT_SECRET_AAD: &str = "TeamCraft-v1 account secret";
pub(crate) const ACCOUNT_SECRET_KEY: &str = "TeamCraft-v1 account secret key";
pub(crate) const RECOVERY_REQUEST_FINGERPRINT: &str = "TeamCraft-v1 recovery request fingerprint";
pub(crate) const MEMBERSHIP_CHANGE: &str = "TeamCraft-v1 membership change";
pub(crate) const PUSH_WAKEUP_AAD: &str = "TeamCraft-v1 push wake-up";
pub(crate) const PUSH_GATEWAY_REQUEST: &str = "TeamCraft-v1 push gateway request";
pub(crate) const PUSH_GATEWAY_SERVER_ID: &str = "TeamCraft-v1 push gateway server id";
pub(crate) const WORKSPACE_LOG: &str = "TeamCraft-v1 workspace log";
pub(crate) const ORG_RECOVERY_WRAP: &str = "TeamCraft-v1 org recovery wrap";
pub(crate) const ORG_RECOVERY_SEAL: &str = "TeamCraft-v1 org recovery seal";
pub(crate) const RENDEZVOUS_INITIATOR_AUTH: &str = "TeamCraft-v1 rendezvous initiator auth";
pub(crate) const RENDEZVOUS_RESPONDER_AUTH: &str = "TeamCraft-v1 rendezvous responder auth";
pub(crate) const RENDEZVOUS_INITIATOR_TRANSPORT: &str = "TeamCraft-v1 rendezvous initiator transport";
pub(crate) const RENDEZVOUS_RESPONDER_TRANSPORT: &str = "TeamCraft-v1 rendezvous responder transport";
pub(crate) const RENDEZVOUS_PATH_HASH: &str = "TeamCraft-v1 rendezvous path hash";
pub(crate) const RENDEZVOUS_COMMITMENT: &str = "TeamCraft-v1 rendezvous key commitment";
pub(crate) const LOCAL_STORE_LOOKUP_KEY: &str = "TeamCraft-v1 local store lookup key";
pub(crate) const LOCAL_STORE_LOOKUP: &str = "TeamCraft-v1 local store lookup";
pub(crate) const LOCAL_STORE_RECORD: &str = "TeamCraft-v1 local store record";
pub(crate) const SHARE_LINK_KEY_WRAP: &str = "TeamCraft-v1 share link key wrap";
pub(crate) const SHARE_LINK_NAME: &str = "TeamCraft-v1 share link name";
/// MLS exporter label (RFC 9420 §8.5) for the content key of an epoch.
pub(crate) const EPOCH_CONTENT_KEY: &str = "TeamCraft-v1 epoch content key";
pub(crate) const DEVICE_LIST: &str = "TeamCraft-v1 device list";
pub(crate) const WORKSPACE_LOG_HASH: &str = "TeamCraft-v1 workspace log entry hash";

/// Returns `len(label) || label || body`.
pub(crate) fn labeled(label: &str, body: &[u8]) -> Vec<u8> {
  // Invariant: all labels are compile-time constants shorter than 256 bytes (checked in tests).
  let label_len = u8::try_from(label.len()).unwrap_or(u8::MAX);
  let mut out = Vec::with_capacity(1 + label.len() + body.len());
  out.push(label_len);
  out.extend_from_slice(label.as_bytes());
  out.extend_from_slice(body);
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  const ALL_LABELS: [&str; 29] = [
    DEVICE_CERTIFICATE,
    IDENTITY_FINGERPRINT,
    MESSAGE_AAD,
    MESSAGE_SIGNATURE,
    ATTACHMENT_AAD,
    KEY_BAG_AAD,
    BACKUP_KEY_WRAP_AAD,
    PASSKEY_PRF_KDF,
    MEMBERSHIP_CHANGE,
    PUSH_WAKEUP_AAD,
    PUSH_GATEWAY_REQUEST,
    PUSH_GATEWAY_SERVER_ID,
    WORKSPACE_LOG,
    ORG_RECOVERY_WRAP,
    ORG_RECOVERY_SEAL,
    RENDEZVOUS_INITIATOR_AUTH,
    RENDEZVOUS_RESPONDER_AUTH,
    RENDEZVOUS_INITIATOR_TRANSPORT,
    RENDEZVOUS_RESPONDER_TRANSPORT,
    RENDEZVOUS_PATH_HASH,
    RENDEZVOUS_COMMITMENT,
    EPOCH_CONTENT_KEY,
    DEVICE_LIST,
    WORKSPACE_LOG_HASH,
    LOCAL_STORE_LOOKUP_KEY,
    LOCAL_STORE_LOOKUP,
    LOCAL_STORE_RECORD,
    SHARE_LINK_KEY_WRAP,
    SHARE_LINK_NAME,
  ];

  #[test]
  fn labels_are_short_distinct_and_not_mls() {
    for (index, label) in ALL_LABELS.iter().enumerate() {
      assert!(label.len() < 64, "{label} too long for a one-byte length");
      assert!(label.starts_with("TeamCraft-v1 "));
      assert!(!label.starts_with("MLS 1.0 "));
      for other in &ALL_LABELS[index + 1..] {
        assert_ne!(label, other);
      }
    }
  }

  #[test]
  fn labeled_prefixes_length_and_label() {
    let out = labeled("TeamCraft-v1 x", b"body");
    assert_eq!(out[0] as usize, "TeamCraft-v1 x".len());
    assert_eq!(&out[1..15], b"TeamCraft-v1 x");
    assert_eq!(&out[15..], b"body");
  }
}
