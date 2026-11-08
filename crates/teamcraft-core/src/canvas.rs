//! A channel's shared page (issue #1221, e2ee.md §4.11). Its edits are Yjs updates in hidden,
//! encrypted messages ([`MessageContent::Canvas`](crate::store::MessageContent)); every device keeps
//! a channel's updates here, in the order they arrived, as soon as it reads or writes one. The core
//! never decodes them: merging is the page editor's job, and on the web the CRDT loads with the
//! editor, not with the app.
//!
//! The list only grows until the editor compacts it ([`compact`]): it hands back all updates as
//! one, which the core takes only if the list is still the one the editor read.
//!
//! With each update the core keeps who sent it and when (issue #1245), and with a compaction the
//! page's version history the editor made of the compacted updates: what each stretch of edits
//! changed, as opaque bytes, so the page's earlier versions can still be shown and restored.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ConversationId, UserId};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The key prefix of a conversation's page in [`Table::Account`], next to its bookmarks.
pub const CANVAS_PREFIX: &str = "canvas/";

/// Who sent an update and when (`teamcraft_client_api::canvas::CanvasOrigin`, kept here as the
/// core builds without the UI's types too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasOrigin {
  pub by: UserId,
  pub at_ms: i64,
  #[serde(default)]
  pub is_snapshot: bool,
}

/// What a stretch of edits changed, as the editor made it (`teamcraft_client_api::canvas::CanvasMark`);
/// `changes` is opaque to the core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasMark {
  pub maybe_by: Option<UserId>,
  pub from_ms: i64,
  pub until_ms: i64,
  pub changes: Base64Bytes,
}

/// The updates of one conversation's page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasRecord {
  /// Grows with every change of the list.
  pub revision: u64,
  pub updates: Vec<Base64Bytes>,
  /// Updates taken since the last snapshot arrived (or since the start).
  #[serde(default)]
  pub updates_since_snapshot: u32,
  /// Who sent each update and when, by position; `None` for a compaction. Shorter in a record
  /// kept before #1245: the first updates have none.
  #[serde(default)]
  pub origins: Vec<Option<CanvasOrigin>>,
  /// Whether the first update is a compaction whose changes `history` holds.
  #[serde(default)]
  pub is_compacted: bool,
  /// The version history of the compacted updates, as the editor made it.
  #[serde(default)]
  pub history: Vec<CanvasMark>,
}

/// The page of `conversation` as this device holds it; empty when it holds none.
pub fn load(store: &impl LocalStore, conversation: ConversationId) -> Result<CanvasRecord, CoreError> {
  Ok(records::get_json(store, Table::Account, &key(conversation))?.unwrap_or_default())
}

/// Keeps an update of the page by `by` at `at_ms`: an own edit at once, another member's when it
/// is decrypted. The same update again (an own message coming back from the server, a page of
/// history read twice) changes nothing.
pub fn take_update(
  store: &impl LocalStore,
  conversation: ConversationId,
  update: &Base64Bytes,
  is_snapshot: bool,
  by: UserId,
  at_ms: i64,
) -> Result<(), CoreError> {
  let mut record = load(store, conversation)?;
  if record.updates.contains(update) {
    return Ok(());
  }
  // A record from before origins: the updates it holds have none.
  let missing = record.updates.len().saturating_sub(record.origins.len());
  if missing > 0 {
    record.origins.splice(0..0, std::iter::repeat_n(None, missing));
  }
  record.updates.push(update.clone());
  record.origins.push(Some(CanvasOrigin {
    by,
    at_ms,
    is_snapshot,
  }));
  record.revision += 1;
  record.updates_since_snapshot = if is_snapshot {
    0
  } else {
    record.updates_since_snapshot.saturating_add(1)
  };
  records::put_json(store, Table::Account, &key(conversation), &record)
}

/// Gives an own update the server's time once it was sent: kept at once with the device's time, it
/// then counts at the time every other device sees for it.
pub fn settle_time(
  store: &impl LocalStore,
  conversation: ConversationId,
  update: &Base64Bytes,
  at_ms: i64,
) -> Result<(), CoreError> {
  let mut record = load(store, conversation)?;
  let missing = record.updates.len().saturating_sub(record.origins.len());
  let maybe_origin = record
    .updates
    .iter()
    .position(|kept| kept == update)
    .and_then(|index| index.checked_sub(missing))
    .and_then(|index| record.origins.get_mut(index))
    .and_then(Option::as_mut);
  match maybe_origin {
    Some(origin) if origin.at_ms != at_ms => {
      origin.at_ms = at_ms;
      record.revision += 1;
      records::put_json(store, Table::Account, &key(conversation), &record)
    },
    _ => Ok(()),
  }
}

/// Replaces the updates with `merged` and the history with `history` (the old one with the marks
/// of the merged updates added) if the list is still at `revision`; returns whether it did. An
/// update that arrived since then keeps the list as it is, and the editor compacts again later.
pub fn compact(
  store: &impl LocalStore,
  conversation: ConversationId,
  revision: u64,
  merged: Vec<u8>,
  history: Vec<CanvasMark>,
) -> Result<bool, CoreError> {
  let mut record = load(store, conversation)?;
  if record.revision != revision || record.updates.is_empty() {
    return Ok(false);
  }
  record.updates = vec![Base64Bytes::new(merged)];
  record.origins = vec![None];
  record.is_compacted = true;
  record.history = history;
  record.revision += 1;
  records::put_json(store, Table::Account, &key(conversation), &record)?;
  Ok(true)
}

fn key(conversation: ConversationId) -> String {
  format!("{CANVAS_PREFIX}{conversation}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;
  use teamcraft_proto::{Id, Snowflake};

  fn channel() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(7).unwrap())
  }

  fn bytes(value: &[u8]) -> Base64Bytes {
    Base64Bytes::new(value.to_vec())
  }

  fn user() -> UserId {
    UserId::from_snowflake(Snowflake::new(3).unwrap())
  }

  #[test]
  fn updates_are_kept_once_and_count_towards_a_snapshot() {
    let store = MemoryStore::new();
    let channel = channel();
    take_update(&store, channel, &bytes(b"a"), false, user(), 1).unwrap();
    take_update(&store, channel, &bytes(b"b"), false, user(), 2).unwrap();
    take_update(&store, channel, &bytes(b"a"), false, user(), 1).unwrap();
    let record = load(&store, channel).unwrap();
    assert_eq!(record.updates, [bytes(b"a"), bytes(b"b")]);
    assert_eq!(record.revision, 2);
    assert_eq!(record.updates_since_snapshot, 2);
    assert_eq!(
      record.origins,
      [
        Some(CanvasOrigin {
          by: user(),
          at_ms: 1,
          is_snapshot: false
        }),
        Some(CanvasOrigin {
          by: user(),
          at_ms: 2,
          is_snapshot: false
        })
      ],
      "the first time an update came counts"
    );

    take_update(&store, channel, &bytes(b"whole page"), true, user(), 4).unwrap();
    assert_eq!(load(&store, channel).unwrap().updates_since_snapshot, 0);
  }

  #[test]
  fn a_sent_update_takes_the_servers_time() {
    let store = MemoryStore::new();
    let channel = channel();
    take_update(&store, channel, &bytes(b"a"), false, user(), 1).unwrap();
    settle_time(&store, channel, &bytes(b"a"), 7).unwrap();
    let record = load(&store, channel).unwrap();
    assert_eq!(record.origins[0].map(|origin| origin.at_ms), Some(7));
    assert_eq!(record.revision, 2);
    // Gone in a compaction meanwhile: nothing to settle.
    settle_time(&store, channel, &bytes(b"b"), 8).unwrap();
    assert_eq!(load(&store, channel).unwrap().revision, 2);
  }

  #[test]
  fn a_record_from_before_origins_gets_none_for_its_updates() {
    let store = MemoryStore::new();
    let channel = channel();
    let old = CanvasRecord {
      revision: 2,
      updates: vec![bytes(b"a"), bytes(b"b")],
      ..CanvasRecord::default()
    };
    records::put_json(&store, Table::Account, &key(channel), &old).unwrap();
    take_update(&store, channel, &bytes(b"c"), false, user(), 5).unwrap();
    let record = load(&store, channel).unwrap();
    assert_eq!(record.origins.len(), 3);
    assert_eq!(record.origins[..2], [None, None]);
    assert_eq!(record.origins[2].map(|origin| origin.at_ms), Some(5));
  }

  #[test]
  fn a_compaction_waits_for_the_list_it_read() {
    let store = MemoryStore::new();
    let channel = channel();
    take_update(&store, channel, &bytes(b"a"), false, user(), 1).unwrap();
    take_update(&store, channel, &bytes(b"b"), false, user(), 2).unwrap();
    // An update arrived after the editor read revision 2.
    take_update(&store, channel, &bytes(b"c"), false, user(), 3).unwrap();
    assert!(!compact(&store, channel, 2, b"ab".to_vec(), Vec::new()).unwrap());
    assert_eq!(load(&store, channel).unwrap().updates.len(), 3);

    let history = vec![CanvasMark {
      maybe_by: Some(user()),
      from_ms: 1,
      until_ms: 3,
      changes: bytes(b"marks"),
    }];
    assert!(compact(&store, channel, 3, b"abc".to_vec(), history.clone()).unwrap());
    let record = load(&store, channel).unwrap();
    assert_eq!(record.updates, [bytes(b"abc")]);
    assert_eq!(record.origins, [None]);
    assert!(record.is_compacted);
    assert_eq!(record.history, history);
    assert_eq!(record.revision, 4);
    assert_eq!(record.updates_since_snapshot, 3, "a compaction is no snapshot");
  }
}
