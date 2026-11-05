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

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, ClientMessageId};

/// Webhook requests and log lines a source may send per minute when it doesn't say.
pub const DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE: u32 = 600;
/// Highest rate limit a source may ask for; a bigger value is clamped to it.
pub const MAX_SOURCE_RATE_LIMIT_PER_MINUTE: u32 = 60_000;
/// Most files one webhook message of a source may carry; a bigger value is clamped to it. The
/// bridge completes a message's uploads only once all are up, and the server allows 20 unfinished
/// uploads per account by default (`max_pending_attachments_per_user`): this leaves room.
pub const MAX_SOURCE_FILES_PER_MESSAGE: u32 = 16;
/// Longest source name, in characters (it shows in the bridge's logs and its notices).
pub const MAX_SOURCE_NAME_CHARS: usize = 80;
/// Longest name a message may state before anything is cleaned, in bytes; a message with more loses
/// its source when it arrives, so no sender can make a device store a large name.
pub const MAX_SOURCE_NAME_WIRE_BYTES: usize = 1024;
/// Combining marks kept on one letter of a source's name; more only pile up over other text.
const MAX_MARKS_PER_LETTER: usize = 4;
/// Largest source picture, in bytes. It travels inside encrypted messages, so it stays small; the
/// app scales a picture down until it fits.
pub const MAX_SOURCE_PICTURE_BYTES: usize = 32 * 1024;
/// Longest side of a source picture, in pixels; receivers refuse a larger one before decoding it.
pub const MAX_SOURCE_PICTURE_SIDE_PX: u32 = 256;
/// Most alert levels a source may list, and the longest level name.
pub const MAX_SOURCE_ALERT_LEVELS: usize = 16;
pub const MAX_SOURCE_ALERT_LEVEL_CHARS: usize = 32;
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
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
  /// Sets the source's picture, a small PNG ([`check_source_picture`]), or takes it away.
  PictureChanged {
    source: SourceId,
    #[serde(rename = "picture", default, skip_serializing_if = "Option::is_none")]
    maybe_picture: Option<Base64Bytes>,
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
  /// Files one webhook message may carry (`0`: none). Absent: the bridge's default
  /// (`files.max_files_per_message`).
  #[serde(
    rename = "max_files_per_message",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_max_files_per_message: Option<u32>,
}

/// Who posted a message of the bridge, in its encrypted metadata (docs/bridge.md, "Source
/// identity"). Clients believe it only from a bot account and always show the bot badge with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
  /// `None` for a source of the bridge's configuration file, which has no ID.
  #[serde(rename = "id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_id: Option<SourceId>,
  pub name: String,
  /// The source's picture at the time of the message.
  #[serde(rename = "picture_sha256", default, skip_serializing_if = "Option::is_none")]
  pub maybe_picture_sha256: Option<SourcePictureHash>,
  /// The picture itself, now and then, for devices that never read the announcement.
  #[serde(rename = "picture", default, skip_serializing_if = "Option::is_none")]
  pub maybe_picture: Option<Base64Bytes>,
}

/// Why a source picture is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourcePictureError {
  #[error("a source picture is at most {MAX_SOURCE_PICTURE_BYTES} bytes")]
  TooLarge,
  #[error("a source picture must be a PNG")]
  NotPng,
  #[error("a source picture is at most {MAX_SOURCE_PICTURE_SIDE_PX} pixels on each side")]
  TooManyPixels,
  #[error("a source picture can't be animated")]
  Animated,
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

/// SHA-256 of a source picture (computed in `teamcraft-crypto`), which names it on a device. Wire
/// format: 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourcePictureHash([u8; 32]);

impl SourceChange {
  /// The source the change is about; `None` for an unknown change.
  pub fn source(&self) -> Option<SourceId> {
    match self {
      Self::Created { source, .. }
      | Self::Renamed { source, .. }
      | Self::PictureChanged { source, .. }
      | Self::Revoked { source } => Some(*source),
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
      maybe_max_files_per_message: self
        .maybe_max_files_per_message
        .map(|files| files.min(MAX_SOURCE_FILES_PER_MESSAGE)),
    }
  }
}

impl Default for SourceOptions {
  fn default() -> Self {
    Self {
      danger_is_alert: false,
      alert_levels: default_alert_levels(),
      rate_limit_per_minute: default_rate_limit(),
      maybe_max_files_per_message: None,
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

impl SourcePictureHash {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn to_hex(&self) -> String {
    SourceTokenHash::from_bytes(self.0).to_hex()
  }

  pub fn from_hex(text: &str) -> Result<Self, SourceTokenHashError> {
    SourceTokenHash::from_hex(text).map(|hash| Self::from_bytes(*hash.as_bytes()))
  }
}

impl fmt::Debug for SourcePictureHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "SourcePictureHash({}…)", &self.to_hex()[..8])
  }
}

impl fmt::Display for SourcePictureHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.to_hex())
  }
}

impl Serialize for SourcePictureHash {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&self.to_hex())
  }
}

impl<'de> Deserialize<'de> for SourcePictureHash {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let text = String::deserialize(deserializer)?;
    Self::from_hex(&text).map_err(de::Error::custom)
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

/// A source's name as it may be shown: without control and invisible characters (bidi overrides,
/// zero-width and blank-looking ones, line separators, tags), which could dress one name up as
/// another or leave it blank; whitespace collapsed, marks per letter limited, of 1 to
/// [`MAX_SOURCE_NAME_CHARS`] characters. `None` if nothing visible is left.
pub fn clean_source_name(name: &str) -> Option<String> {
  let mut kept = String::with_capacity(name.len().min(MAX_SOURCE_NAME_WIRE_BYTES));
  let mut marks = 0;
  for c in name.chars().filter(|c| !c.is_control() && !is_invisible(*c)) {
    marks = if is_combining_mark(c) { marks + 1 } else { 0 };
    if marks <= MAX_MARKS_PER_LETTER {
      kept.push(c);
    }
  }
  let name = kept.split_whitespace().collect::<Vec<_>>().join(" ");
  let is_shown = name.chars().any(|c| !c.is_whitespace() && !is_combining_mark(c));
  (is_shown && name.chars().count() <= MAX_SOURCE_NAME_CHARS).then_some(name)
}

/// Checks a source picture before anything decodes it: small, a still PNG, and few pixels by its
/// header, so a tiny file can't unpack into a huge bitmap. PNG alone: no format with scripts (SVG),
/// and one header to read.
pub fn check_source_picture(bytes: &[u8]) -> Result<(), SourcePictureError> {
  if bytes.len() > MAX_SOURCE_PICTURE_BYTES {
    return Err(SourcePictureError::TooLarge);
  }
  // The signature, the length and type of the first chunk, then width and height.
  let header = bytes.get(..24).ok_or(SourcePictureError::NotPng)?;
  if header[..8] != PNG_SIGNATURE || header[12..16] != *b"IHDR" {
    return Err(SourcePictureError::NotPng);
  }
  let side = |at: usize| u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]]);
  let (width, height) = (side(16), side(20));
  if width == 0 || height == 0 {
    return Err(SourcePictureError::NotPng);
  }
  if width > MAX_SOURCE_PICTURE_SIDE_PX || height > MAX_SOURCE_PICTURE_SIDE_PX {
    return Err(SourcePictureError::TooManyPixels);
  }
  check_png_is_still(bytes)
}

/// Walks the chunks up to the first image data: an animated PNG says so before it (`acTL`), and it
/// would ignore the user's reduced motion in an `<img>`.
fn check_png_is_still(bytes: &[u8]) -> Result<(), SourcePictureError> {
  let mut at = PNG_SIGNATURE.len();
  loop {
    let chunk = bytes.get(at..at + 8).ok_or(SourcePictureError::NotPng)?;
    match &chunk[4..8] {
      b"acTL" => return Err(SourcePictureError::Animated),
      b"IDAT" => return Ok(()),
      _ => {},
    }
    let length = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    // The size limit above keeps this far from overflowing; a wild length just ends past the file.
    at = at
      .saturating_add(usize::try_from(length).unwrap_or(usize::MAX))
      .saturating_add(12);
  }
}

/// Unicode's Default_Ignorable_Code_Point, the other format characters, line and paragraph
/// separators, and the braille blank, which shows as nothing and isn't whitespace.
fn is_invisible(c: char) -> bool {
  matches!(c,
    '\u{ad}' | '\u{34f}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}'
    | '\u{890}'..='\u{891}' | '\u{8e2}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}'
    | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
    | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}'
    | '\u{ffa0}' | '\u{fff0}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{13430}'..='\u{1343f}'
    | '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' | '\u{e0000}'..='\u{e0fff}')
}

/// The blocks of marks that pile up without a limit in practice. Not every mark of every script: a
/// script's own vowel signs stay as they are.
fn is_combining_mark(c: char) -> bool {
  matches!(c,
    '\u{300}'..='\u{36f}' | '\u{483}'..='\u{489}' | '\u{591}'..='\u{5bd}' | '\u{610}'..='\u{61a}'
    | '\u{64b}'..='\u{65f}' | '\u{1ab0}'..='\u{1aff}' | '\u{1dc0}'..='\u{1dff}'
    | '\u{20d0}'..='\u{20ff}' | '\u{fe20}'..='\u{fe2f}')
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
          maybe_max_files_per_message: None,
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
        maybe_max_files_per_message: Some(u32::MAX),
      }
      .bounded();
      assert_eq!(options.rate_limit_per_minute, MAX_SOURCE_RATE_LIMIT_PER_MINUTE);
      assert_eq!(options.alert_levels.len(), MAX_SOURCE_ALERT_LEVELS);
      assert_eq!(options.alert_levels[0], "level0");
      assert_eq!(
        options.maybe_max_files_per_message,
        Some(MAX_SOURCE_FILES_PER_MESSAGE)
      );
      let zero = SourceOptions {
        rate_limit_per_minute: 0,
        maybe_max_files_per_message: Some(0),
        ..SourceOptions::default()
      }
      .bounded();
      assert_eq!(zero.rate_limit_per_minute, 1);
      assert_eq!(zero.maybe_max_files_per_message, Some(0), "no files at all");
    }

    #[test]
    fn the_files_limit_travels_only_when_set() {
      let options: SourceOptions = serde_json::from_value(json!({"max_files_per_message": 3})).unwrap();
      assert_eq!(options.maybe_max_files_per_message, Some(3));
      assert_eq!(
        serde_json::to_value(&options).unwrap()["max_files_per_message"],
        json!(3)
      );
      let unset = serde_json::to_value(SourceOptions::default()).unwrap();
      assert!(unset.get("max_files_per_message").is_none());
    }
  }

  mod source_identity {
    use super::*;

    #[test]
    fn a_picture_change_has_a_stable_form_and_an_older_client_ignores_it() {
      let source = SourceId::from_bytes([1; 16]);
      let change = SourceChange::PictureChanged {
        source,
        maybe_picture: Some(Base64Bytes::new(vec![1, 2, 3])),
      };
      let value = serde_json::to_value(&change).unwrap();
      assert_eq!(value["action"], json!("picture_changed"));
      assert_eq!(value["picture"], json!("AQID"));
      assert_eq!(serde_json::from_value::<SourceChange>(value).unwrap(), change);
      let removed: SourceChange =
        serde_json::from_value(json!({"action": "picture_changed", "source": source.to_hex()})).unwrap();
      assert_eq!(
        removed,
        SourceChange::PictureChanged {
          source,
          maybe_picture: None
        }
      );
    }

    #[test]
    fn an_identity_leaves_out_what_it_does_not_have() {
      let identity = SourceIdentity {
        maybe_id: None,
        name: "nas-logs".into(),
        maybe_picture_sha256: None,
        maybe_picture: None,
      };
      assert_eq!(
        serde_json::to_value(&identity).unwrap(),
        json!({"name": "nas-logs"})
      );
    }

    #[test]
    fn names_lose_invisible_characters() {
      assert_eq!(clean_source_name("  Hotline\n"), Some("Hotline".to_owned()));
      assert_eq!(clean_source_name("Ceo\u{202e}oot"), Some("Ceooot".to_owned()));
      assert_eq!(clean_source_name("a\u{200b}b\u{feff}"), Some("ab".to_owned()));
      assert_eq!(clean_source_name("\u{200b} \u{202e}"), None);
      assert_eq!(clean_source_name(""), None);
      assert_eq!(clean_source_name(&"x".repeat(MAX_SOURCE_NAME_CHARS + 1)), None);
      assert!(clean_source_name(&"x".repeat(MAX_SOURCE_NAME_CHARS)).is_some());
    }

    #[test]
    fn nothing_invisible_or_line_breaking_stays_in_a_name() {
      for invisible in [
        '\u{2028}',
        '\u{2029}',
        '\u{3164}',
        '\u{115f}',
        '\u{1160}',
        '\u{ffa0}',
        '\u{2800}',
        '\u{34f}',
        '\u{17b4}',
        '\u{180b}',
        '\u{fe0f}',
        '\u{e0001}',
        '\u{e0041}',
        '\u{e0100}',
        '\u{1d173}',
      ] {
        assert_eq!(
          clean_source_name(&format!("A{invisible}B")).as_deref(),
          Some("AB"),
          "{invisible:?}"
        );
        assert_eq!(
          clean_source_name(&invisible.to_string()),
          None,
          "alone: {invisible:?}"
        );
      }
    }

    #[test]
    fn whitespace_collapses_and_marks_are_limited() {
      assert_eq!(clean_source_name("a  \u{a0} b").as_deref(), Some("a b"));
      let piled = format!("a{}b", "\u{301}".repeat(79));
      assert_eq!(
        clean_source_name(&piled),
        Some(format!("a{}b", "\u{301}".repeat(4)))
      );
      assert_eq!(
        clean_source_name("\u{301}\u{301}"),
        None,
        "marks alone show nothing"
      );
      assert_eq!(
        clean_source_name("Prüfung Größe"),
        Some("Prüfung Größe".to_owned())
      );
      assert_eq!(clean_source_name("Tiếng Việt").as_deref(), Some("Tiếng Việt"));
    }

    #[test]
    fn only_a_small_png_is_a_source_picture() {
      assert_eq!(check_source_picture(&png(128, 128)), Ok(()));
      assert_eq!(check_source_picture(&[]), Err(SourcePictureError::NotPng));
      assert_eq!(
        check_source_picture(b"<svg></svg>"),
        Err(SourcePictureError::NotPng)
      );
      assert_eq!(
        check_source_picture(&png(128, 128)[..20]),
        Err(SourcePictureError::NotPng),
        "cut off in the header"
      );
      assert_eq!(
        check_source_picture(&png_header(0, 8)),
        Err(SourcePictureError::NotPng)
      );
      assert_eq!(
        check_source_picture(&png(MAX_SOURCE_PICTURE_SIDE_PX + 1, 8)),
        Err(SourcePictureError::TooManyPixels)
      );
      assert_eq!(
        check_source_picture(&png(8, 30_000)),
        Err(SourcePictureError::TooManyPixels),
        "a tiny file can claim a huge bitmap"
      );
      let mut large = png(8, 8);
      large.resize(MAX_SOURCE_PICTURE_BYTES + 1, 0);
      assert_eq!(check_source_picture(&large), Err(SourcePictureError::TooLarge));
    }

    #[test]
    fn an_animated_png_is_refused() {
      let mut animated = png_header(8, 8);
      chunk(&mut animated, b"acTL");
      chunk(&mut animated, b"IDAT");
      assert_eq!(check_source_picture(&animated), Err(SourcePictureError::Animated));
      let mut with_text = png_header(8, 8);
      chunk(&mut with_text, b"tEXt");
      chunk(&mut with_text, b"IDAT");
      assert_eq!(check_source_picture(&with_text), Ok(()));
      let mut wild = png_header(8, 8);
      wild.extend(u32::MAX.to_be_bytes());
      wild.extend(*b"tEXt");
      assert_eq!(check_source_picture(&wild), Err(SourcePictureError::NotPng));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
      let mut bytes = png_header(width, height);
      chunk(&mut bytes, b"IDAT");
      bytes
    }

    /// The signature and the image header with its 5 other bytes and its checksum.
    fn png_header(width: u32, height: u32) -> Vec<u8> {
      let mut bytes = PNG_SIGNATURE.to_vec();
      bytes.extend(13_u32.to_be_bytes());
      bytes.extend(*b"IHDR");
      bytes.extend(width.to_be_bytes());
      bytes.extend(height.to_be_bytes());
      bytes.extend([0; 9]);
      bytes
    }

    fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4]) {
      bytes.extend(0_u32.to_be_bytes());
      bytes.extend(kind);
      bytes.extend([0; 4]);
    }
  }
}
