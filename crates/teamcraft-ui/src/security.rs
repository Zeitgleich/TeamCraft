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
  fn fingerprints_are_grouped() {
    assert_eq!(group_fingerprint("ab12cd34ef"), "ab12 cd34 ef");
    assert_eq!(group_fingerprint("ab12 cd34"), "ab12 cd34");
    assert_eq!(group_fingerprint(""), "");
  }
}
