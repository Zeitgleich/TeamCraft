//! Device linking, login approval and key-change warnings (e2ee.md §3 and §5).

use crate::model::{ConversationId, DeviceId, TimestampMs, UserId};

/// A new device asks an existing one for approval after an OIDC login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceApproval {
  pub device: DeviceId,
  /// "Firefox on Linux"
  pub device_name: String,
  /// Approximate place of the login, e.g. "Berlin, Germany".
  pub location: String,
  pub requested_at: TimestampMs,
  /// Short code shown on both devices; the user compares them before approving.
  pub code: String,
}

impl DeviceApproval {
  /// The code with spaces between digits, as in "4 7 1 9", for reading aloud and screen readers.
  pub fn spaced_code(&self) -> String {
    self.code.chars().map(String::from).collect::<Vec<_>>().join(" ")
  }
}

/// A contact's identity key changed (only after a full reset without recovery).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyChange {
  pub user: UserId,
  pub conversation: ConversationId,
  pub old_fingerprint: String,
  pub new_fingerprint: String,
  pub changed_at: TimestampMs,
}

/// What the user compares with a contact out of band (call, in person) to trust their keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContactVerification {
  /// Twelve groups of five digits, identical on both sides.
  pub safety_number: String,
  pub is_verified: bool,
}

/// Placeholder derivation for demo data: a stable pseudo-fingerprint (32 hex characters) from a seed.
/// The real value comes from the identity keys in `teamcraft-crypto`.
pub fn demo_fingerprint(seed: u64) -> String {
  let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
  (0..4)
    .map(|_| {
      state ^= state << 13;
      state ^= state >> 7;
      state ^= state << 17;
      format!("{:08x}", state as u32)
    })
    .collect()
}

/// Placeholder safety number: the same for both people whichever order the seeds come in.
pub fn demo_safety_number(a: u64, b: u64) -> String {
  let (low, high) = if a <= b { (a, b) } else { (b, a) };
  let mut state = (low.wrapping_mul(31) ^ high.wrapping_mul(0x9E37_79B9)) | 1;
  (0..12)
    .map(|_| {
      state ^= state << 13;
      state ^= state >> 7;
      state ^= state << 17;
      format!("{:05}", state % 100_000)
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// The number two people compare out of band: the same for both whichever way round the
/// fingerprints come in. Twelve groups of five digits, each from 16 bits of the first 96 bits of a
/// fingerprint (the fingerprints themselves are hashes, so no further hashing is needed).
pub fn safety_number(fingerprint_a: &str, fingerprint_b: &str) -> String {
  let (low, high) = if fingerprint_a <= fingerprint_b {
    (fingerprint_a, fingerprint_b)
  } else {
    (fingerprint_b, fingerprint_a)
  };
  [low, high]
    .iter()
    .flat_map(|fingerprint| {
      let hex: Vec<char> = fingerprint.chars().filter(char::is_ascii_hexdigit).collect();
      (0..6)
        .map(|group| {
          let digits: String = hex.iter().skip(group * 4).take(4).collect();
          format!("{:05}", u32::from_str_radix(&digits, 16).unwrap_or(0))
        })
        .collect::<Vec<_>>()
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// Groups a hex fingerprint into blocks of four characters: "ab12 cd34 …".
pub fn group_fingerprint(fingerprint: &str) -> String {
  let chars: Vec<char> = fingerprint.chars().filter(|ch| !ch.is_whitespace()).collect();
  chars
    .chunks(4)
    .map(|chunk| chunk.iter().collect::<String>())
    .collect::<Vec<_>>()
    .join(" ")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_safety_number_is_symmetric_and_depends_on_both_keys() {
    let (a, b, c) = ("00ff10ab".repeat(8), "1234abcd".repeat(8), "5678abcd".repeat(8));
    assert_eq!(safety_number(&a, &b), safety_number(&b, &a));
    assert_ne!(safety_number(&a, &b), safety_number(&a, &c));
    assert_eq!(safety_number(&a, &b).split(' ').count(), 12);
    assert!(safety_number(&a, &b).split(' ').all(|group| group.len() == 5));
  }
  use crate::model::id_from_raw;

  #[test]
  fn code_is_spaced_for_reading() {
    let approval = DeviceApproval {
      device: id_from_raw(1),
      device_name: "Browser".into(),
      location: "Nowhere".into(),
      requested_at: 0,
      code: "4719".into(),
    };
    assert_eq!(approval.spaced_code(), "4 7 1 9");
  }

  #[test]
  fn demo_values_are_stable_and_symmetric() {
    assert_eq!(demo_fingerprint(7), demo_fingerprint(7));
    assert_ne!(demo_fingerprint(7), demo_fingerprint(8));
    assert_eq!(demo_fingerprint(1).len(), 32);
    assert_eq!(demo_safety_number(1, 2), demo_safety_number(2, 1));
    assert_eq!(demo_safety_number(1, 2).split(' ').count(), 12);
  }

  #[test]
  fn fingerprints_are_grouped() {
    assert_eq!(group_fingerprint("ab12cd34ef"), "ab12 cd34 ef");
    assert_eq!(group_fingerprint("ab12 cd34"), "ab12 cd34");
    assert_eq!(group_fingerprint(""), "");
  }
}
