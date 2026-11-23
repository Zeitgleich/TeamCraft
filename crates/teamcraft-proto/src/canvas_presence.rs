//! Who has a channel's notes open, and where their caret is (issue #1456, e2ee.md §4.11, "Who has
//! the page open"). A device with a page open says so in a `canvas_presence` gateway frame whose
//! content is [`CanvasPresence`], encrypted with the channel's content key and signed by the device:
//! the server relays it to the members connected at that moment and keeps nothing.

use serde::{Deserialize, Serialize};

use crate::{CanvasPageId, DeviceId, UserId};

/// How often a device with a page open says so again: the others drop it after
/// [`CANVAS_PRESENCE_TIMEOUT_MS`] without word, so a device that crashed or lost its connection
/// leaves their pages by itself.
pub const CANVAS_PRESENCE_REFRESH_MS: i64 = 20_000;
/// How long a device counts as having a page open after its last word: two refreshes and a margin
/// for a slow link.
pub const CANVAS_PRESENCE_TIMEOUT_MS: i64 = 50_000;
/// The shortest time between two words of a device about one channel's notes: a moving caret goes
/// out at most this often (the server drops more, [`CANVAS_PRESENCE_SERVER_GAP_MS`]).
pub const CANVAS_PRESENCE_INTERVAL_MS: i64 = 1_000;
/// The server's own floor per connection and channel, below the clients' interval so a frame sent
/// a little early still goes out.
pub const CANVAS_PRESENCE_SERVER_GAP_MS: u64 = 500;
/// The largest sealed frame the server relays: a word with a caret is well below 300 bytes.
pub const MAX_CANVAS_PRESENCE_BYTES: usize = 1_024;

/// What a device says about a channel's notes, inside the sealed frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasPresence {
  /// Grows with every word of the device; a receiver drops a word not newer than the last it read
  /// from that device, so a frame the server replays can't move a caret back.
  pub seq: u64,
  /// The page: the channel's first (absent) or a further one.
  #[serde(rename = "page", default, skip_serializing_if = "Option::is_none")]
  pub maybe_page: Option<CanvasPageId>,
  pub state: CanvasPageState,
  /// Where the device's caret or selection is, if it is in a block of the page.
  #[serde(rename = "caret", default, skip_serializing_if = "Option::is_none")]
  pub maybe_caret: Option<CanvasCaret>,
}

/// Whether the device has the page open or just closed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasPageState {
  Open,
  /// Left the page: the others drop it at once instead of after the timeout.
  Closed,
  #[serde(other)]
  Unknown,
}

/// A caret or selection in a block, in UTF-16 units of the block's text: `start == end` is a
/// caret. Another member's client sent it, so a receiver checks it ([`Self::is_valid`]) and
/// clamps the offsets to the block's text before drawing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasCaret {
  /// The block's ID in the page's document.
  pub block: String,
  pub start: u32,
  pub end: u32,
}

/// Another member's device that has a page of a channel's notes open, and where its caret is
/// (issue #1456, e2ee.md §4.11): from its encrypted, ephemeral word over the gateway.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasPeer {
  pub user: UserId,
  pub device: DeviceId,
  /// Its caret or selection, checked against the rules ([`CanvasCaret::is_valid`])
  /// but not against the page: the editor clamps it to the block's text.
  #[serde(default)]
  pub maybe_caret: Option<CanvasCaret>,
  /// When its last word arrived, by the clock of the device that keeps the core (the same as the
  /// UI's on desktop and web): the editor drops it after
  /// [`CANVAS_PRESENCE_TIMEOUT_MS`] even when nothing asks the core again.
  pub heard_at_ms: i64,
}

/// What the editor says about the page it shows: open (with the caret, if it is in a block) or
/// closed. The core numbers and seals it ([`CanvasPresence`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasPresenceNote {
  #[serde(default)]
  pub maybe_page: Option<CanvasPageId>,
  pub state: CanvasPageState,
  #[serde(default)]
  pub maybe_caret: Option<CanvasCaret>,
}

impl CanvasCaret {
  /// The longest block ID a receiver keeps (the editor makes them far shorter).
  pub const MAX_BLOCK_LEN: usize = 64;

  /// Whether the caret keeps the rules: a short block ID of letters, digits, `-` and `_`.
  pub fn is_valid(&self) -> bool {
    !self.block.is_empty()
      && self.block.len() <= Self::MAX_BLOCK_LEN
      && self
        .block
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
  }
}

impl CanvasPresence {
  /// Whether everything another client sent keeps the rules (page ID and caret); a word that
  /// doesn't is dropped whole.
  pub fn is_valid(&self) -> bool {
    self.maybe_page.as_ref().is_none_or(CanvasPageId::is_valid)
      && self.maybe_caret.as_ref().is_none_or(CanvasCaret::is_valid)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_word_reads_back_with_and_without_its_parts() {
    let word = CanvasPresence {
      seq: 3,
      maybe_page: CanvasPageId::parse("ab-1"),
      state: CanvasPageState::Open,
      maybe_caret: Some(CanvasCaret {
        block: "b1".to_owned(),
        start: 2,
        end: 4,
      }),
    };
    let json = serde_json::to_string(&word).unwrap();
    assert_eq!(
      json,
      r#"{"seq":3,"page":"ab-1","state":"open","caret":{"block":"b1","start":2,"end":4}}"#
    );
    assert_eq!(serde_json::from_str::<CanvasPresence>(&json).unwrap(), word);
    let bare: CanvasPresence = serde_json::from_str(r#"{"seq":1,"state":"closed"}"#).unwrap();
    assert_eq!(bare.maybe_page, None);
    assert_eq!(bare.state, CanvasPageState::Closed);
  }

  #[test]
  fn a_newer_state_reads_as_unknown() {
    let word: CanvasPresence = serde_json::from_str(r#"{"seq":1,"state":"away"}"#).unwrap();
    assert_eq!(word.state, CanvasPageState::Unknown);
  }

  #[test]
  fn made_up_pages_and_blocks_are_refused() {
    let mut word = CanvasPresence {
      seq: 1,
      maybe_page: None,
      state: CanvasPageState::Open,
      maybe_caret: None,
    };
    assert!(word.is_valid());
    word.maybe_page = serde_json::from_str(r#""../x""#).ok();
    assert!(!word.is_valid());
    word.maybe_page = None;
    for block in ["", "a b", "../x", &"a".repeat(CanvasCaret::MAX_BLOCK_LEN + 1)] {
      word.maybe_caret = Some(CanvasCaret {
        block: block.to_owned(),
        start: 0,
        end: 0,
      });
      assert!(!word.is_valid(), "{block:?}");
    }
  }
}
