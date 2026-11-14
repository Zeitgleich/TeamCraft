//! Names a channel's further pages of notes (issue #1456, e2ee.md §4.11). The channel's first page
//! has no ID; every page added later has one, made by the client that adds it and carried inside
//! the encrypted edit messages of that page. The server never sees it.

use serde::{Deserialize, Serialize};

/// The most further pages a channel lists: entries of the first page's list beyond these (in the
/// list's order, deleted ones not counted) are left out by every client alike, and a client adds no
/// page at the limit. A protocol limit, not a setting: all members' clients must show the same
/// pages, as with the limits a receiver puts on polls.
pub const MAX_LISTED_CANVAS_PAGES: usize = 50;

/// The most further pages of one channel a device keeps updates of while it doesn't know them as
/// listed: edits that arrive before the list names their page, and on a device whose editor hasn't
/// read the list. The one edited least recently goes first, so a member sending edits for thousands
/// of made-up page IDs costs every device a bounded store, and pages the editor knows as listed
/// (at most [`MAX_LISTED_CANVAS_PAGES`]) are never pushed out.
pub const MAX_UNLISTED_CANVAS_PAGES: usize = 50;

/// One of a channel's further pages. Another member's client chose it, so a device keeps only IDs
/// that pass [`CanvasPageId::parse`]: short, lowercase hex digits and dashes, safe in a store key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanvasPageId(String);

impl CanvasPageId {
  /// The longest ID a device keeps.
  pub const MAX_LEN: usize = 40;

  /// The ID if it keeps the rules; `None` for anything else another client may have sent.
  pub fn parse(value: &str) -> Option<Self> {
    let is_valid = !value.is_empty()
      && value.len() <= Self::MAX_LEN
      && value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-');
    is_valid.then(|| Self(value.to_owned()))
  }

  /// Whether an ID read from a message keeps the rules (deserializing doesn't check them).
  pub fn is_valid(&self) -> bool {
    Self::parse(&self.0).is_some()
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl std::fmt::Display for CanvasPageId {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str(&self.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_short_hex_ids_with_dashes_are_kept() {
    assert!(CanvasPageId::parse("1f2e-0").is_some());
    for refused in [
      "",
      "../x",
      "A1",
      "page 1",
      "ü",
      &"a".repeat(CanvasPageId::MAX_LEN + 1),
    ] {
      assert!(CanvasPageId::parse(refused).is_none(), "{refused:?}");
    }
  }

  #[test]
  fn an_id_is_a_plain_string_on_the_wire() {
    let id = CanvasPageId::parse("ab-1").unwrap();
    assert_eq!(serde_json::to_string(&id).unwrap(), r#""ab-1""#);
    let read: CanvasPageId = serde_json::from_str(r#""../evil""#).unwrap();
    assert!(!read.is_valid(), "read as it is, checked before use");
  }
}
