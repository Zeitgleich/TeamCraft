//! A [`LocalStore`] that reads through to another one and keeps its own writes in memory, for a
//! probe that must leave the store as it found it (issue #839, e2ee.md §7.2): the iOS notification
//! extension catches a conversation up on it to show a message, and drops it afterwards.
//!
//! Nothing it is given reaches the store below: it only ever calls that store's reads. Its writes
//! are bounded ([`OverlayStore::new`]): past the limit it refuses them like a full disk, so a probe
//! that would take more memory than the extension has fails instead of the extension.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId};
use zeroize::Zeroize as _;

use super::{
  FeedVersion, LocalStore, MessageEdge, MessageFilter, StoreError, StoredMessage, Table, WriteTally,
};

/// What one key-value entry costs besides its key and value (map node, allocator).
const ENTRY_OVERHEAD_BYTES: usize = 64;

/// `base` with this probe's writes on top; see the module documentation.
pub struct OverlayStore<S> {
  base: S,
  max_bytes: usize,
  inner: Mutex<Overlay>,
}

#[derive(Default)]
struct Overlay {
  /// `None`: deleted here.
  tables: BTreeMap<(Table, String), Option<Vec<u8>>>,
  /// `None`: deleted here.
  messages: BTreeMap<(ConversationId, [u8; 16]), Option<StoredMessage>>,
  /// Conversations whose messages were all deleted here: none of the base's count any more.
  cleared: BTreeSet<ConversationId>,
  /// About how much memory the writes above take.
  bytes: usize,
}

impl<S: LocalStore> OverlayStore<S> {
  /// Reads `base`, keeps writes in memory, and refuses a write once they take about `max_bytes`.
  pub fn new(base: S, max_bytes: usize) -> Self {
    Self {
      base,
      max_bytes,
      inner: Mutex::new(Overlay::default()),
    }
  }

  /// About how many bytes the writes kept here take.
  pub fn overlay_bytes(&self) -> usize {
    self.lock().bytes
  }

  fn lock(&self) -> MutexGuard<'_, Overlay> {
    // A poisoned lock only means another thread panicked mid-update; the maps stay usable.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Counts `bytes` more, or refuses them past the limit (nothing is counted then).
  fn reserve(&self, overlay: &mut Overlay, bytes: usize) -> Result<(), StoreError> {
    let total = overlay.bytes.saturating_add(bytes);
    if total > self.max_bytes {
      return Err(StoreError::QuotaExceeded);
    }
    overlay.bytes = total;
    Ok(())
  }

  fn put_entry(&self, table: Table, key: &str, maybe_value: Option<&[u8]>) -> Result<(), StoreError> {
    let mut overlay = self.lock();
    let bytes = ENTRY_OVERHEAD_BYTES + key.len() + maybe_value.map_or(0, <[u8]>::len);
    self.reserve(&mut overlay, bytes)?;
    if let Some(Some(mut old)) = overlay
      .tables
      .insert((table, key.to_string()), maybe_value.map(<[u8]>::to_vec))
    {
      // Probably an MLS secret: not left in freed memory (as `MemoryStore`).
      old.zeroize();
    }
    Ok(())
  }

  /// The base's messages of `conversation` this overlay doesn't replace or delete.
  fn base_messages(
    &self,
    overlay: &Overlay,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    if overlay.cleared.contains(&conversation) {
      return Ok(Vec::new());
    }
    let mut messages = self.base.messages(conversation, filter)?;
    messages.retain(|message| {
      !overlay
        .messages
        .contains_key(&(conversation, *message.client_message_id.as_bytes()))
    });
    Ok(messages)
  }

  /// Whether this overlay holds anything of `conversation`'s messages.
  fn touches(overlay: &Overlay, conversation: ConversationId) -> bool {
    overlay.cleared.contains(&conversation)
      || overlay
        .messages
        .range((conversation, [0; 16])..=(conversation, [u8::MAX; 16]))
        .next()
        .is_some()
  }
}

impl<S: LocalStore> LocalStore for OverlayStore<S> {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    if let Some(entry) = self.lock().tables.get(&(table, key.to_string())) {
      return Ok(entry.clone());
    }
    self.base.get(table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.put_entry(table, key, Some(value))
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.put_entry(table, key, None)
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut entries: BTreeMap<String, Vec<u8>> = self.base.list(table)?.into_iter().collect();
    let overlay = self.lock();
    for ((entry_table, key), entry) in &overlay.tables {
      if *entry_table != table {
        continue;
      }
      match entry {
        Some(value) => drop(entries.insert(key.clone(), value.clone())),
        None => drop(entries.remove(key)),
      }
    }
    Ok(entries.into_iter().collect())
  }

  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut entries: BTreeMap<String, Vec<u8>> = self.base.list_prefix(table, prefix)?.into_iter().collect();
    let overlay = self.lock();
    let ours = overlay
      .tables
      .range((table, prefix.to_string())..)
      .take_while(|((entry_table, key), _)| *entry_table == table && key.starts_with(prefix));
    for ((_, key), entry) in ours {
      match entry {
        Some(value) => drop(entries.insert(key.clone(), value.clone())),
        None => drop(entries.remove(key)),
      }
    }
    Ok(entries.into_iter().collect())
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    let mut overlay = self.lock();
    self.reserve(&mut overlay, message.footprint_bytes())?;
    let key = (message.conversation_id, *message.client_message_id.as_bytes());
    overlay.messages.insert(key, Some(message.clone()));
    Ok(())
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    {
      let overlay = self.lock();
      if let Some(entry) = overlay
        .messages
        .get(&(conversation, *client_message_id.as_bytes()))
      {
        return Ok(entry.clone());
      }
      if overlay.cleared.contains(&conversation) {
        return Ok(None);
      }
    }
    self.base.message(conversation, client_message_id)
  }

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let overlay = self.lock();
    let ours = overlay
      .messages
      .range((conversation, [0; 16])..=(conversation, [u8::MAX; 16]))
      .filter_map(|(_, entry)| entry.as_ref())
      .find(|message| message.maybe_server_id == Some(server_id));
    if let Some(message) = ours {
      return Ok(Some(message.clone()));
    }
    if overlay.cleared.contains(&conversation) {
      return Ok(None);
    }
    // One the overlay replaced or deleted doesn't count: its own version was looked at above.
    Ok(
      self
        .base
        .message_by_server_id(conversation, server_id)?
        .filter(|message| {
          !overlay
            .messages
            .contains_key(&(conversation, *message.client_message_id.as_bytes()))
        }),
    )
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let overlay = self.lock();
    let mut messages = self.base_messages(&overlay, conversation, filter)?;
    messages.extend(
      overlay
        .messages
        .range((conversation, [0; 16])..=(conversation, [u8::MAX; 16]))
        .filter_map(|(_, entry)| entry.as_ref())
        .filter(|message| message.matches(filter))
        .cloned(),
    );
    messages.sort_by_key(StoredMessage::sort_key);
    Ok(messages)
  }

  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    // The base's index answers while the overlay holds nothing of the conversation (the cursor of
    // the catch-up's first page); after that the conversation is read whole, as the default does.
    if !Self::touches(&self.lock(), conversation) {
      return self.base.edge_server_id(conversation, edge);
    }
    let server_ids = self
      .messages(conversation, MessageFilter::All)?
      .into_iter()
      .filter(|message| message.maybe_unreadable.is_none())
      .filter_map(|message| message.maybe_server_id);
    Ok(match edge {
      MessageEdge::Oldest => server_ids.min(),
      MessageEdge::Newest => server_ids.max(),
    })
  }

  fn feed_version(&self, conversation: ConversationId) -> Result<Option<FeedVersion>, StoreError> {
    let _ = conversation;
    Ok(None)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    let mut overlay = self.lock();
    self.reserve(&mut overlay, ENTRY_OVERHEAD_BYTES)?;
    overlay
      .messages
      .retain(|(entry_conversation, _), _| *entry_conversation != conversation);
    overlay.cleared.insert(conversation);
    Ok(())
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    let mut overlay = self.lock();
    self.reserve(&mut overlay, ENTRY_OVERHEAD_BYTES)?;
    overlay
      .messages
      .insert((conversation, *client_message_id.as_bytes()), None);
    Ok(())
  }

  // Everything below stays here: none of it may reach the base, whose file another process owns.

  fn purge_freed(&self) -> Result<(), StoreError> {
    Ok(())
  }

  fn purge_freed_soon(&self) {}

  fn begin_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }

  fn fail_writes(&self) {}

  fn write_soon(&self) {}

  /// Every write is kept, and nobody else writes here.
  fn write_tally(&self) -> Option<WriteTally> {
    None
  }
}

impl<S> Drop for OverlayStore<S> {
  fn drop(&mut self) {
    let overlay = self
      .inner
      .get_mut()
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    for value in overlay.tables.values_mut().flatten() {
      value.zeroize();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;
  use crate::store::tests::{exercise_store, id, message};

  #[test]
  fn an_overlay_behaves_like_a_store() {
    exercise_store(&OverlayStore::new(MemoryStore::new(), usize::MAX));
  }

  /// Everything below the overlay, as a list: what a probe must leave unchanged.
  fn everything(store: &MemoryStore, conversation: ConversationId) -> String {
    let tables: Vec<_> = Table::ALL
      .iter()
      .map(|table| store.list(*table).unwrap())
      .collect();
    let messages = store.messages(conversation, MessageFilter::All).unwrap();
    format!("{tables:?}{messages:?}")
  }

  #[test]
  fn the_overlay_reads_through_and_never_writes_below() {
    let base = MemoryStore::new();
    let conversation: ConversationId = id(7);
    base.put(Table::Account, "kept", b"base").unwrap();
    base.put(Table::Account, "replaced", b"base").unwrap();
    base.put(Table::Account, "deleted", b"base").unwrap();
    let old = message(conversation, 1, Some(100), None, 1_000);
    base.upsert_message(&old).unwrap();
    let before = everything(&base, conversation);

    let overlay = OverlayStore::new(&base, usize::MAX);
    overlay.put(Table::Account, "replaced", b"overlay").unwrap();
    overlay.put(Table::Account, "new", b"overlay").unwrap();
    overlay.delete(Table::Account, "deleted").unwrap();
    let new = message(conversation, 2, Some(200), None, 2_000);
    overlay.upsert_message(&new).unwrap();
    overlay.begin_writes().unwrap();
    overlay.end_writes().unwrap();
    overlay.purge_freed().unwrap();

    assert_eq!(overlay.get(Table::Account, "kept").unwrap().unwrap(), b"base");
    assert_eq!(
      overlay.get(Table::Account, "replaced").unwrap().unwrap(),
      b"overlay"
    );
    assert_eq!(overlay.get(Table::Account, "deleted").unwrap(), None);
    let keys: Vec<String> = overlay
      .list(Table::Account)
      .unwrap()
      .into_iter()
      .map(|(key, _)| key)
      .collect();
    assert_eq!(keys, ["kept", "new", "replaced"]);
    assert_eq!(
      overlay.messages(conversation, MessageFilter::All).unwrap().len(),
      2
    );
    assert_eq!(
      overlay.edge_server_id(conversation, MessageEdge::Newest).unwrap(),
      new.maybe_server_id
    );
    assert!(
      overlay
        .message_by_server_id(conversation, new.maybe_server_id.unwrap())
        .unwrap()
        .is_some()
    );
    overlay
      .delete_message(conversation, old.client_message_id)
      .unwrap();
    assert!(
      overlay
        .message_by_server_id(conversation, old.maybe_server_id.unwrap())
        .unwrap()
        .is_none()
    );
    overlay.delete_messages(conversation).unwrap();
    assert!(
      overlay
        .messages(conversation, MessageFilter::All)
        .unwrap()
        .is_empty()
    );
    drop(overlay);

    assert_eq!(everything(&base, conversation), before, "the base is as it was");
  }

  #[test]
  fn writes_past_the_limit_are_refused_like_a_full_disk() {
    let overlay = OverlayStore::new(MemoryStore::new(), 1_000);
    overlay.put(Table::Account, "small", &[0; 100]).unwrap();
    let used = overlay.overlay_bytes();
    assert!(matches!(
      overlay.put(Table::Account, "large", &[0; 1_000]),
      Err(StoreError::QuotaExceeded)
    ));
    assert_eq!(overlay.overlay_bytes(), used, "a refused write counts nothing");
    assert_eq!(overlay.get(Table::Account, "large").unwrap(), None);
  }
}
