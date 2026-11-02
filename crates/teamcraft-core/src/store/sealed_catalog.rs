//! The catalog of a sealed store (issue #155): which keys each table holds and where each
//! conversation's messages sit in display order, so the store reads only what it is asked for.
//!
//! The catalog is a set of [`super::sealed_index`] indexes whose nodes are sealed records like any
//! other: entries of [`CATALOG_TABLE`], a table name no version uses for data (versions before
//! skip it as unknown), keyed `<generation>/d/<table>/…` (a table's keys),
//! `<generation>/m/<conversation>/…` (a conversation's messages), `<generation>/f/<conversation>/…`
//! (the rows of its feed, whose page lengths count positions) and `<generation>/x/<conversation>/…`
//! (its reactions by target, replies by thread, and events), plus the record [`META_KEY`] saying
//! which generation is current, what it holds and whether it is complete. The catalog is derived
//! from the records: a damaged or older one is built again under a new generation, and the build
//! deletes the nodes of older ones. Nothing about the catalog is readable without the database key:
//! its records look like all others, and their values are padded to a power of two (at least
//! [`MIN_NODE_BYTES`]), so their sizes tell little of how many entries they hold (e2ee.md §9.1).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, ThreadId};

use super::lru::Lru;
use super::sealed_index::{IndexNodes, IndexPage, IndexRoot};
use super::{MessageFilter, MessageOrder, StoreError, StoredMessage};

/// The reserved table the catalog's records are entries of.
pub(crate) const CATALOG_TABLE: &str = "_catalog";
/// The catalog record saying how far the catalog is built ([`CatalogState`]).
pub(crate) const META_KEY: &str = "meta";
/// The smallest size a catalog node's value is padded to.
const MIN_NODE_BYTES: usize = 4 * 1024;
/// Prefix of a conversation's message index.
pub(crate) const MESSAGES_KIND: &str = "m";
/// Prefix of a table's key directory.
pub(crate) const DIRECTORY_KIND: &str = "d";
/// Prefix of a conversation's feed index.
pub(crate) const FEED_KIND: &str = "f";
/// Prefix of a conversation's relations index.
pub(crate) const RELATIONS_KIND: &str = "x";
/// What a complete catalog holds. A store with an older one builds it again when it opens: the
/// build only adds what is missing. 1: tables and messages; 2: feeds and relations.
pub(crate) const CATALOG_VERSION: u32 = 2;

/// How far the catalog of a store is built, and its generation (random, so a catalog built again
/// never meets nodes of an earlier one). A store written before the catalog existed builds it when
/// it is opened, in batches; one killed midway goes on after the last batch it saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum CatalogState {
  /// Built up to the record with this lookup ID (in ID order, as hex), or not yet started.
  Building {
    generation: u64,
    #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
    maybe_after: Option<String>,
    /// [`CATALOG_VERSION`] of the build; the first version wrote none.
    #[serde(default = "first_catalog_version")]
    version: u32,
  },
  Complete {
    generation: u64,
    #[serde(default = "first_catalog_version")]
    version: u32,
  },
}

impl CatalogState {
  pub(crate) fn generation(&self) -> u64 {
    match self {
      Self::Building { generation, .. } | Self::Complete { generation, .. } => *generation,
    }
  }
}

/// A message's place in its conversation's index: its display order ([`StoredMessage::sort_key`]),
/// ties broken by client message ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct MessageKey(pub(crate) u8, pub(crate) i64, pub(crate) u128);

/// What the index knows of a message, so ranges and filters need no decryption: server ID, thread
/// and [`MessageFlags`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MessageEntry(Option<i64>, Option<i64>, u8);

/// Bits of a [`MessageEntry`].
struct MessageFlags;

impl MessageFlags {
  const HIDDEN: u8 = 1;
  const ALSO_IN_CHANNEL: u8 = 2;
  const MEDIA: u8 = 4;
  const UNREADABLE: u8 = 8;
}

/// An entry of a conversation's relations index: what a message relates to, then the message.
/// All messages of one relation are neighbours, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RelationKey(u8, u128, MessageKey);

/// Which relation a [`RelationKey`] notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Relation {
  /// Reacts to this message.
  ReactionTo(ClientMessageId),
  /// A shown reply of this thread ([`StoredMessage::counts_as_reply`]).
  ReplyIn(ThreadId),
  /// An event or derived line ([`StoredMessage::is_event`]).
  Event,
}

/// A page of an index: its scope (the conversation or table) and its number.
type PageId = (String, u64);
/// A decoded page, shared by the cache and its readers.
type SharedPage<K, V> = Arc<IndexPage<K, V>>;
/// Reads a catalog record's value by its key.
pub(crate) type NodeRead<'a> = &'a dyn Fn(&str) -> Result<Option<Vec<u8>>, StoreError>;

/// A catalog record to write when a group of writes ends: its key, and its value or `None` to
/// delete it.
pub(crate) struct NodeChange {
  pub(crate) key: String,
  pub(crate) maybe_value: Option<Vec<u8>>,
}

/// The roots and pages of one kind of index, cached, with the changes of the open group of writes
/// until it ends ([`Self::take_dirty`]). Without a generation (a store whose catalog was never
/// started, opened by a process that doesn't build it) every index is empty and writes to it are
/// dropped: the build later reads every record anyway.
pub(crate) struct NodeCache<K, V> {
  kind: &'static str,
  maybe_generation: Option<u64>,
  roots: Lru<String, Arc<IndexRoot<K>>>,
  pages: Lru<PageId, SharedPage<K, V>>,
  /// Written in the open group; an empty root removes the index.
  dirty_roots: BTreeMap<String, Arc<IndexRoot<K>>>,
  dirty_pages: BTreeMap<PageId, Option<SharedPage<K, V>>>,
}

/// One index of a [`NodeCache`], reading what isn't cached through `read`.
pub(crate) struct ScopedNodes<'a, K, V> {
  pub(crate) cache: &'a mut NodeCache<K, V>,
  pub(crate) scope: &'a str,
  pub(crate) read: NodeRead<'a>,
}

impl Relation {
  /// The relations `message` has.
  pub(crate) fn of(message: &StoredMessage) -> Vec<Self> {
    let mut relations = Vec::new();
    relations.extend(message.reaction_target().map(Self::ReactionTo));
    if message.counts_as_reply() {
      relations.extend(message.maybe_thread_id.map(Self::ReplyIn));
    }
    if message.is_event() {
      relations.push(Self::Event);
    }
    relations
  }

  fn parts(self) -> (u8, u128) {
    match self {
      Self::ReactionTo(target) => (0, u128::from_be_bytes(*target.as_bytes())),
      Self::ReplyIn(thread) => (1, u128::from(raw_thread(thread).cast_unsigned())),
      Self::Event => (2, 0),
    }
  }

  /// The smallest key of this relation.
  pub(crate) fn first_key(self) -> RelationKey {
    let (kind, target) = self.parts();
    RelationKey(kind, target, MessageKey(0, i64::MIN, 0))
  }

  pub(crate) fn key_of(self, message: MessageKey) -> RelationKey {
    let (kind, target) = self.parts();
    RelationKey(kind, target, message)
  }

  /// Whether `key` notes this relation.
  pub(crate) fn holds(self, key: &RelationKey) -> bool {
    (key.0, key.1) == self.parts()
  }
}

impl RelationKey {
  pub(crate) fn message_key(self) -> MessageKey {
    self.2
  }
}

impl From<MessageOrder> for MessageKey {
  fn from(order: MessageOrder) -> Self {
    Self(
      order.group,
      order.value,
      u128::from_be_bytes(order.client_message_id),
    )
  }
}

impl MessageKey {
  pub(crate) fn of(message: &StoredMessage) -> Self {
    let (group, value) = message.sort_key();
    Self(
      group,
      value,
      u128::from_be_bytes(*message.client_message_id.as_bytes()),
    )
  }

  /// The smallest key at a server position: every message there sorts at or after it.
  pub(crate) fn first_at(position: MessageId) -> Self {
    Self(0, position.snowflake().get(), 0)
  }

  /// The largest key at a server position.
  pub(crate) fn last_at(position: MessageId) -> Self {
    Self(0, position.snowflake().get(), u128::MAX)
  }

  /// The smallest key of the messages not sent yet, which sort after every server position.
  pub(crate) fn first_pending() -> Self {
    Self(1, i64::MIN, 0)
  }

  pub(crate) fn client_message_id(self) -> ClientMessageId {
    ClientMessageId::from_bytes(self.2.to_be_bytes())
  }

  pub(crate) fn is_at(self, position: MessageId) -> bool {
    self.0 == 0 && self.1 == position.snowflake().get()
  }
}

impl MessageEntry {
  pub(crate) fn of(message: &StoredMessage) -> Self {
    let mut flags = 0;
    for (is_set, flag) in [
      (message.is_hidden(), MessageFlags::HIDDEN),
      (message.is_also_in_channel(), MessageFlags::ALSO_IN_CHANNEL),
      (message.has_media(), MessageFlags::MEDIA),
      (message.maybe_unreadable.is_some(), MessageFlags::UNREADABLE),
    ] {
      if is_set {
        flags |= flag;
      }
    }
    Self(
      message.maybe_server_id.map(|id| id.snowflake().get()),
      message.maybe_thread_id.map(|thread| thread.snowflake().get()),
      flags,
    )
  }

  /// Whether the message may be listed for `filter`; the message itself decides
  /// ([`StoredMessage::matches`]), this only preselects.
  pub(crate) fn may_match(self, filter: MessageFilter) -> bool {
    let is_hidden = self.has(MessageFlags::HIDDEN);
    match filter {
      MessageFilter::All => true,
      MessageFilter::TopLevel => !is_hidden && (self.1.is_none() || self.has(MessageFlags::ALSO_IN_CHANNEL)),
      MessageFilter::Thread(thread) => !is_hidden && self.1 == Some(raw_thread(thread)),
    }
  }

  /// The server ID of a readable server message: what [`super::LocalStore::edge_server_id`] counts.
  pub(crate) fn readable_server_id(self) -> Option<MessageId> {
    if self.has(MessageFlags::UNREADABLE) {
      return None;
    }
    self.server_id()
  }

  pub(crate) fn server_id(self) -> Option<MessageId> {
    self
      .0
      .and_then(|raw| teamcraft_proto::Snowflake::new(raw).ok())
      .map(MessageId::from_snowflake)
  }

  pub(crate) fn has_media(self) -> bool {
    self.has(MessageFlags::MEDIA)
  }

  fn has(self, flag: u8) -> bool {
    self.2 & flag != 0
  }
}

impl<K, V> NodeCache<K, V> {
  /// A cache of the indexes of one `kind` (the prefix of their records), keeping up to `max_nodes`
  /// roots and as many pages.
  pub(crate) fn new(kind: &'static str, max_nodes: usize) -> Self {
    Self {
      kind,
      maybe_generation: None,
      roots: Lru::new(max_nodes),
      pages: Lru::new(max_nodes),
      dirty_roots: BTreeMap::new(),
      dirty_pages: BTreeMap::new(),
    }
  }

  /// Forgets everything, the open group's changes too (the disk no longer matches the cache).
  pub(crate) fn clear(&mut self) {
    self.roots.clear();
    self.pages.clear();
    self.dirty_roots.clear();
    self.dirty_pages.clear();
  }

  /// Switches to `maybe_generation`'s nodes, forgetting the others'.
  pub(crate) fn set_generation(&mut self, maybe_generation: Option<u64>) {
    if self.maybe_generation != maybe_generation {
      self.clear();
      self.maybe_generation = maybe_generation;
    }
  }

  pub(crate) fn has_dirty(&self) -> bool {
    !self.dirty_roots.is_empty() || !self.dirty_pages.is_empty()
  }
}

impl<K: Serialize, V: Serialize> NodeCache<K, V> {
  /// The changes of the group that ends, serialized. They stay cached.
  pub(crate) fn take_dirty(&mut self) -> Result<Vec<NodeChange>, StoreError> {
    let mut changes = Vec::with_capacity(self.dirty_roots.len() + self.dirty_pages.len());
    for (scope, root) in std::mem::take(&mut self.dirty_roots) {
      let key = root_key(self.prefix(), &scope);
      let maybe_value = (!root.is_empty()).then(|| encode_node(&*root)).transpose()?;
      changes.push(NodeChange { key, maybe_value });
    }
    for ((scope, id), maybe_page) in std::mem::take(&mut self.dirty_pages) {
      let key = page_key(self.prefix(), &scope, id);
      let maybe_value = maybe_page.map(|page| encode_node(&*page)).transpose()?;
      changes.push(NodeChange { key, maybe_value });
    }
    Ok(changes)
  }
}

impl<K, V> NodeCache<K, V> {
  /// `<generation>/<kind>`, the start of every node key of this cache.
  fn prefix(&self) -> String {
    format!(
      "{}/{}",
      generation_prefix(self.maybe_generation.unwrap_or(0)),
      self.kind
    )
  }
}

impl<K: DeserializeOwned, V: DeserializeOwned> IndexNodes<K, V> for ScopedNodes<'_, K, V> {
  fn root(&mut self) -> Result<Arc<IndexRoot<K>>, StoreError> {
    if self.cache.maybe_generation.is_none() {
      return Ok(Arc::new(IndexRoot::empty()));
    }
    if let Some(root) = self.cache.dirty_roots.get(self.scope) {
      return Ok(Arc::clone(root));
    }
    let scope = self.scope.to_string();
    if let Some(root) = self.cache.roots.get(&scope) {
      return Ok(Arc::clone(root));
    }
    let root = match (self.read)(&root_key(self.cache.prefix(), self.scope))? {
      Some(bytes) => decode(&bytes)?,
      None => IndexRoot::empty(),
    };
    let root = Arc::new(root);
    self.cache.roots.insert(scope, Arc::clone(&root), 1);
    Ok(root)
  }

  fn page(&mut self, id: u64) -> Result<Arc<IndexPage<K, V>>, StoreError> {
    let key = (self.scope.to_string(), id);
    if let Some(maybe_page) = self.cache.dirty_pages.get(&key) {
      return maybe_page
        .clone()
        .ok_or_else(|| StoreError::Corrupt("an index lists a removed page".into()));
    }
    if let Some(page) = self.cache.pages.get(&key) {
      return Ok(Arc::clone(page));
    }
    let bytes = (self.read)(&page_key(self.cache.prefix(), self.scope, id))?
      .ok_or_else(|| StoreError::Corrupt("an index page of the local database is missing".into()))?;
    let page = Arc::new(decode::<IndexPage<K, V>>(&bytes)?);
    self.cache.pages.insert(key, Arc::clone(&page), 1);
    Ok(page)
  }

  fn put_root(&mut self, root: IndexRoot<K>) {
    if self.cache.maybe_generation.is_none() {
      return;
    }
    let root = Arc::new(root);
    let scope = self.scope.to_string();
    self.cache.roots.insert(scope.clone(), Arc::clone(&root), 1);
    self.cache.dirty_roots.insert(scope, root);
  }

  fn put_page(&mut self, id: u64, maybe_page: Option<IndexPage<K, V>>) {
    if self.cache.maybe_generation.is_none() {
      return;
    }
    let key = (self.scope.to_string(), id);
    let maybe_page = maybe_page.map(Arc::new);
    match &maybe_page {
      Some(page) => self.cache.pages.insert(key.clone(), Arc::clone(page), 1),
      None => {
        self.cache.pages.remove(&key);
      },
    }
    self.cache.dirty_pages.insert(key, maybe_page);
  }
}

/// The scope of a conversation's message index.
pub(crate) fn messages_scope(conversation: ConversationId) -> String {
  conversation.to_string()
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
  serde_json::to_vec(value).map_err(|error| StoreError::Corrupt(error.to_string()))
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
  serde_json::from_slice(bytes).map_err(|error| StoreError::Corrupt(error.to_string()))
}

/// The start of the keys of a generation's nodes.
pub(crate) fn generation_prefix(generation: u64) -> String {
  format!("{generation:016x}")
}

/// Whether a catalog record's key belongs to `generation` (or is the catalog's state).
pub(crate) fn is_current_node(key: &str, generation: u64) -> bool {
  key == META_KEY
    || key
      .strip_prefix(&generation_prefix(generation))
      .is_some_and(|rest| rest.starts_with('/'))
}

/// A node's value, padded with spaces (which JSON ignores) to a power of two.
fn encode_node<T: Serialize>(node: &T) -> Result<Vec<u8>, StoreError> {
  let mut bytes = encode(node)?;
  let padded = bytes.len().next_power_of_two().max(MIN_NODE_BYTES);
  bytes.resize(padded, b' ');
  Ok(bytes)
}

fn root_key(prefix: String, scope: &str) -> String {
  format!("{prefix}/{scope}/r")
}

fn page_key(prefix: String, scope: &str, id: u64) -> String {
  format!("{prefix}/{scope}/{id}")
}

fn first_catalog_version() -> u32 {
  1
}

fn raw_thread(thread: ThreadId) -> i64 {
  thread.snowflake().get()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// What versions before wrote stays readable, as an older catalog.
  #[test]
  fn a_state_without_a_version_is_of_the_first_catalog() {
    let decoded: CatalogState = decode(br#"{"state":"complete","generation":7}"#).unwrap();
    assert_eq!(
      decoded,
      CatalogState::Complete {
        generation: 7,
        version: 1
      }
    );
    let decoded: CatalogState = decode(br#"{"state":"building","generation":7,"after":"00"}"#).unwrap();
    assert_eq!(
      decoded,
      CatalogState::Building {
        generation: 7,
        maybe_after: Some("00".into()),
        version: 1
      }
    );
  }
}
