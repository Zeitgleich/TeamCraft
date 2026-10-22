//! What the user shares about themself, and "last seen" of others.

use serde::{Deserialize, Serialize};
pub use teamcraft_proto::LastSeenVisibility;

/// The server-side privacy choices (the read receipts and typing switches are local preferences).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacySettings {
  /// Who may see when the user was last active.
  pub last_seen: LastSeenVisibility,
  /// The workspace allows last seen at all; if not, the choice has no effect and the UI says so.
  pub is_last_seen_available: bool,
}

impl Default for PrivacySettings {
  fn default() -> Self {
    Self {
      last_seen: LastSeenVisibility::Everyone,
      is_last_seen_available: true,
    }
  }
}
