//! Integration-bridge sources managed in the app (docs/bridge.md, "Sources"): a workspace admin's
//! client announces a source with an encrypted control message in the channel, and the bridge, a
//! member of that channel, reads it from there. Only members can write in the channel's encryption
//! group, so the server can neither create nor alter a source; it sees ciphertext only (e2ee.md
//! §13.1).
//!
//! These types are the plaintext inside the message ciphertext (`MessageContent::IntegrationSource`
//! in `teamcraft-core`). They are forward compatible: a newer change or source kind reaches an
//! older client as `Unknown` and is ignored.

use std::fmt;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

use crate::ClientMessageId;
use crate::wire_enum::wire_enum;

/// Webhook requests and log lines a source may send per minute when it doesn't say.
pub const DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE: u32 = 600;
/// Highest rate limit a source may ask for; a bigger value is clamped to it.
pub const MAX_SOURCE_RATE_LIMIT_PER_MINUTE: u32 = 60_000;
/// Longest source name, in characters (it shows in the bridge's logs and its notices).
pub const MAX_SOURCE_NAME_CHARS: usize = 80;
/// Most alert levels a source may list, and the longest level name.
pub const MAX_SOURCE_ALERT_LEVELS: usize = 16;
pub const MAX_SOURCE_ALERT_LEVEL_CHARS: usize = 32;
/// Log levels (lower case) that make a line an alert unless a source lists its own.
pub const DEFAULT_SOURCE_ALERT_LEVELS: [&str; 6] =
  ["error", "err", "fatal", "critical", "alert", "emergency"];

/// One change to a channel's sources. Sent as an encrypted, hidden message in that channel; the
/// channel is the one the source posts into, so a source can never point elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SourceChange {
  /// A new source. The token itself never travels: only its SHA-256, which the bridge compares
  /// the token of a request with.
  Created {
    source: SourceId,
    name: String,
    kind: SourceKind,
    token_sha256: SourceTokenHash,
    #[serde(default)]
    options: SourceOptions,
  },
  Renamed {
    source: SourceId,
    name: String,
  },
  /// The token stops working for good; a revoked source never comes back.
  Revoked {
    source: SourceId,
  },
  /// From a newer client.
  #[serde(other)]
  Unknown,
}

wire_enum! {
  /// Which endpoint of the bridge a source's token opens.
  pub enum SourceKind {
    /// `POST /hooks/<token>`: Slack/Mattermost-compatible incoming webhooks.
    Webhook => "webhook",
    /// `POST /ingest/<token>`: log lines, plain text or NDJSON.
    Log => "log",
  }
}

/// How the bridge treats a source's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceOptions {
  /// Webhook attachments with `color: "danger"` are alerts.
  #[serde(default)]
  pub danger_is_alert: bool,
  /// Log levels (lower case) that make a line an alert.
  #[serde(default = "default_alert_levels")]
  pub alert_levels: Vec<String>,
  #[serde(default = "default_rate_limit")]
  pub rate_limit_per_minute: u32,
}

/// Random 128-bit name of a source, chosen by the client that creates it. Wire format: 32 lowercase
/// hex characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(ClientMessageId);

/// SHA-256 of a source token (computed in `teamcraft-crypto`). Wire format: 64 lowercase hex
/// characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceTokenHash([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a source token hash must be 64 hex characters")]
pub struct SourceTokenHashError;

impl SourceChange {
  /// The source the change is about; `None` for an unknown change.
  pub fn source(&self) -> Option<SourceId> {
    match self {
      Self::Created { source, .. } | Self::Renamed { source, .. } | Self::Revoked { source } => Some(*source),
      Self::Unknown => None,
    }
  }
}

impl SourceOptions {
  /// The options with every value inside its limits: the rate between 1 and the maximum, at most
  /// [`MAX_SOURCE_ALERT_LEVELS`] short lower-case levels. A sender can't make the bridge do more
  /// than that, whatever it writes.
  pub fn bounded(&self) -> Self {
    Self {
      danger_is_alert: self.danger_is_alert,
      alert_levels: self
        .alert_levels
        .iter()
        .map(|level| level.trim().to_lowercase())
        .filter(|level| !level.is_empty() && level.chars().count() <= MAX_SOURCE_ALERT_LEVEL_CHARS)
        .take(MAX_SOURCE_ALERT_LEVELS)
        .collect(),
      rate_limit_per_minute: self
        .rate_limit_per_minute
        .clamp(1, MAX_SOURCE_RATE_LIMIT_PER_MINUTE),
    }
  }
}

impl Default for SourceOptions {
  fn default() -> Self {
    Self {
      danger_is_alert: false,
      alert_levels: default_alert_levels(),
      rate_limit_per_minute: default_rate_limit(),
    }
  }
}

impl SourceId {
  /// A fresh random ID from the operating system's random source.
  pub fn random() -> Self {
    Self(ClientMessageId::random())
  }

  pub fn from_bytes(bytes: [u8; 16]) -> Self {
    Self(ClientMessageId::from_bytes(bytes))
  }

  pub fn to_hex(&self) -> String {
    self.0.to_hex()
  }

  pub fn from_hex(text: &str) -> Result<Self, crate::ClientMessageIdError> {
    ClientMessageId::from_hex(text).map(Self)
  }
}

impl PartialOrd for SourceId {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for SourceId {
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    self.0.as_bytes().cmp(other.0.as_bytes())
  }
}

impl fmt::Display for SourceId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    self.0.fmt(f)
  }
}

impl SourceTokenHash {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  pub fn to_hex(&self) -> String {
    self.0.iter().map(|byte| format!("{byte:02x}")).collect()
  }

  pub fn from_hex(text: &str) -> Result<Self, SourceTokenHashError> {
    if text.len() != 64 || !text.is_ascii() {
      return Err(SourceTokenHashError);
    }
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| SourceTokenHashError)?;
    }
    Ok(Self(bytes))
  }
}

/// Not secret (a hash of a high-entropy token), but shortened in logs anyway.
impl fmt::Debug for SourceTokenHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SourceTokenHash({}…)", &self.to_hex()[..8])
  }
}

impl Serialize for SourceTokenHash {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for SourceTokenHash {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).map_err(de::Error::custom)
  }
}

fn default_alert_levels() -> Vec<String> {
  DEFAULT_SOURCE_ALERT_LEVELS.map(String::from).into()
}

fn default_rate_limit() -> u32 {
  DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  const ID_HEX: &str = "000102030405060708090a0b0c0d0e0f";

  mod wire_format {
    use super::*;

    #[test]
    fn a_created_source_has_a_fixed_shape() {
      let change = SourceChange::Created {
        source: SourceId::from_bytes(core::array::from_fn(|index| index as u8)),
        name: "nas-logs".into(),
        kind: SourceKind::Log,
        token_sha256: SourceTokenHash::from_bytes([0xab; 32]),
        options: SourceOptions {
          danger_is_alert: true,
          alert_levels: vec!["error".into()],
          rate_limit_per_minute: 60,
        },
      };
      let value = serde_json::to_value(&change).unwrap();
      assert_eq!(
        value,
        json!({
          "action": "created",
          "source": ID_HEX,
          "name": "nas-logs",
          "kind": "log",
          "token_sha256": "ab".repeat(32),
          "options": {"danger_is_alert": true, "alert_levels": ["error"], "rate_limit_per_minute": 60},
        })
      );
      assert_eq!(serde_json::from_value::<SourceChange>(value).unwrap(), change);
    }

    #[test]
    fn revoke_and_rename_name_the_source() {
      let source = SourceId::from_hex(ID_HEX).unwrap();
      assert_eq!(
        serde_json::to_value(SourceChange::Revoked { source }).unwrap(),
        json!({"action": "revoked", "source": ID_HEX})
      );
      assert_eq!(
        serde_json::to_value(SourceChange::Renamed {
          source,
          name: "ci".into()
        })
        .unwrap(),
        json!({"action": "renamed", "source": ID_HEX, "name": "ci"})
      );
    }

    #[test]
    fn missing_options_get_the_defaults() {
      let change: SourceChange = serde_json::from_value(json!({
        "action": "created", "source": ID_HEX, "name": "ci", "kind": "webhook",
        "token_sha256": "00".repeat(32),
      }))
      .unwrap();
      let SourceChange::Created { options, .. } = change else {
        panic!("{change:?}")
      };
      assert_eq!(options, SourceOptions::default());
      assert_eq!(options.rate_limit_per_minute, 600);
      assert!(options.alert_levels.contains(&"fatal".to_owned()));
    }
  }

  mod forward_compatibility {
    use super::*;

    #[test]
    fn an_unknown_change_is_kept_as_unknown() {
      let change: SourceChange =
        serde_json::from_value(json!({"action": "paused", "source": ID_HEX, "until": 5})).unwrap();
      assert_eq!(change, SourceChange::Unknown);
      assert_eq!(change.source(), None);
    }

    #[test]
    fn an_unknown_kind_round_trips() {
      let kind: SourceKind = serde_json::from_value(json!("metrics")).unwrap();
      assert_eq!(kind, SourceKind::Unknown("metrics".into()));
      assert_eq!(serde_json::to_value(&kind).unwrap(), json!("metrics"));
    }
  }

  mod parsing {
    use super::*;

    #[test]
    fn malformed_hashes_and_ids_are_rejected() {
      for bad in ["", "ab", &"zz".repeat(32), &"ab".repeat(33), &"é".repeat(32)] {
        assert!(SourceTokenHash::from_hex(bad).is_err(), "{bad}");
      }
      assert!(serde_json::from_value::<SourceId>(json!("short")).is_err());
      assert!(
        serde_json::from_value::<SourceChange>(json!({"action": "revoked", "source": "nope"})).is_err()
      );
    }

    #[test]
    fn options_are_bounded() {
      let options = SourceOptions {
        danger_is_alert: false,
        alert_levels: (0..40)
          .map(|index| format!(" Level{index} "))
          .chain([String::new(), "x".repeat(33)])
          .collect(),
        rate_limit_per_minute: u32::MAX,
      }
      .bounded();
      assert_eq!(options.rate_limit_per_minute, MAX_SOURCE_RATE_LIMIT_PER_MINUTE);
      assert_eq!(options.alert_levels.len(), MAX_SOURCE_ALERT_LEVELS);
      assert_eq!(options.alert_levels[0], "level0");
      let zero = SourceOptions {
        rate_limit_per_minute: 0,
        ..SourceOptions::default()
      };
      assert_eq!(zero.bounded().rate_limit_per_minute, 1);
    }
  }
}
