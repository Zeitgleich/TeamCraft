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
pub(crate) const ATTACHMENT_AAD: &str = "TeamCraft-v1 attachment chunk";
pub(crate) const KEY_BAG_AAD: &str = "TeamCraft-v1 key bag";
pub(crate) const BACKUP_KEY_WRAP_AAD: &str = "TeamCraft-v1 backup key wrap";
pub(crate) const PASSKEY_PRF_KDF: &str = "TeamCraft-v1 passkey prf backup key wrap";
pub(crate) const MEMBERSHIP_CHANGE: &str = "TeamCraft-v1 membership change";
pub(crate) const PUSH_WAKEUP_AAD: &str = "TeamCraft-v1 push wake-up";

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

  const ALL_LABELS: [&str; 9] = [
    DEVICE_CERTIFICATE,
    IDENTITY_FINGERPRINT,
    MESSAGE_AAD,
    ATTACHMENT_AAD,
    KEY_BAG_AAD,
    BACKUP_KEY_WRAP_AAD,
    PASSKEY_PRF_KDF,
    MEMBERSHIP_CHANGE,
    PUSH_WAKEUP_AAD,
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
