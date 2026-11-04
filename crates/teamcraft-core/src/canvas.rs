//! A channel's shared page (issue #1221, e2ee.md §4.11). Its edits are Yjs updates in hidden,
//! encrypted messages ([`MessageContent::Canvas`](crate::store::MessageContent)); every device keeps
//! a channel's updates here, in the order they arrived, as soon as it reads or writes one. The core
//! never decodes them: merging is the page editor's job, and on the web the CRDT loads with the
//! editor, not with the app.
//!
//! The list only grows until the editor compacts it ([`compact`]): it hands back all updates as
//! one, which the core takes only if the list is still the one the editor read.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ConversationId};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The key prefix of a conversation's page in [`Table::Account`], next to its bookmarks.
pub const CANVAS_PREFIX: &str = "canvas/";

/// The updates of one conversation's page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasRecord {
  /// Grows with every change of the list.
  pub revision: u64,
  pub updates: Vec<Base64Bytes>,
  /// Updates taken since the last snapshot arrived (or since the start).
  #[serde(default)]
  pub updates_since_snapshot: u32,
}

/// The page of `conversation` as this device holds it; empty when it holds none.
pub fn load(store: &impl LocalStore, conversation: ConversationId) -> Result<CanvasRecord, CoreError> {
  Ok(records::get_json(store, Table::Account, &key(conversation))?.unwrap_or_default())
}

/// Keeps an update of the page: an own edit at once, another member's when it is decrypted. The
/// same update again (an own message coming back from the server, a page of history read twice)
/// changes nothing.
pub fn take_update(
  store: &impl LocalStore,
  conversation: ConversationId,
  update: &Base64Bytes,
  is_snapshot: bool,
) -> Result<(), CoreError> {
  let mut record = load(store, conversation)?;
  if record.updates.contains(update) {
    return Ok(());
  }
  record.updates.push(update.clone());
  record.revision += 1;
  record.updates_since_snapshot = if is_snapshot {
    0
  } else {
    record.updates_since_snapshot.saturating_add(1)
  };
  records::put_json(store, Table::Account, &key(conversation), &record)
}

/// Replaces the updates with `merged` if the list is still at `revision`; returns whether it did.
/// An update that arrived since then keeps the list as it is, and the editor compacts again later.
pub fn compact(
  store: &impl LocalStore,
  conversation: ConversationId,
  revision: u64,
  merged: Vec<u8>,
) -> Result<bool, CoreError> {
  let mut record = load(store, conversation)?;
  if record.revision != revision || record.updates.is_empty() {
    return Ok(false);
  }
  record.updates = vec![Base64Bytes::new(merged)];
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

  #[test]
  fn updates_are_kept_once_and_count_towards_a_snapshot() {
    let store = MemoryStore::new();
    let channel = channel();
    take_update(&store, channel, &bytes(b"a"), false).unwrap();
    take_update(&store, channel, &bytes(b"b"), false).unwrap();
    take_update(&store, channel, &bytes(b"a"), false).unwrap();
    let record = load(&store, channel).unwrap();
    assert_eq!(record.updates, [bytes(b"a"), bytes(b"b")]);
    assert_eq!(record.revision, 2);
    assert_eq!(record.updates_since_snapshot, 2);

    take_update(&store, channel, &bytes(b"whole page"), true).unwrap();
    assert_eq!(load(&store, channel).unwrap().updates_since_snapshot, 0);
  }

  #[test]
  fn a_compaction_waits_for_the_list_it_read() {
    let store = MemoryStore::new();
    let channel = channel();
    take_update(&store, channel, &bytes(b"a"), false).unwrap();
    take_update(&store, channel, &bytes(b"b"), false).unwrap();
    // An update arrived after the editor read revision 2.
    take_update(&store, channel, &bytes(b"c"), false).unwrap();
    assert!(!compact(&store, channel, 2, b"ab".to_vec()).unwrap());
    assert_eq!(load(&store, channel).unwrap().updates.len(), 3);

    assert!(compact(&store, channel, 3, b"abc".to_vec()).unwrap());
    let record = load(&store, channel).unwrap();
    assert_eq!(record.updates, [bytes(b"abc")]);
    assert_eq!(record.revision, 4);
    assert_eq!(record.updates_since_snapshot, 3, "a compaction is no snapshot");
  }
}
