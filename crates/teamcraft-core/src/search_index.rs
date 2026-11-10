//! The local search index (issue #154, e2ee.md §10.1, "Local index"): a tantivy index of the
//! messages this device decrypted, kept in the local store and brought up to date as messages are
//! written, so a search no longer reads and indexes every stored message.
//!
//! - **Sealed like the store.** The index's files are records of [`Table::SearchIndex`], cut into
//!   chunks of [`CHUNK_BYTES`]: SQLCipher encrypts them with the rest of the database, the sealed
//!   store seals each record. Nothing of the index is written anywhere else, and nothing of it
//!   leaves the device. A copy is held in memory while it is used (see [`LoadedIndex`]).
//! - **Incremental.** Every write of a message search covers, or of one that may stop being covered
//!   (an edit, a deletion, a message removed), leaves a record in [`Table::SearchQueue`] in the same
//!   group of writes ([`crate::store::TrackedStore`]); a conversation whose messages all go leaves
//!   one for the conversation. [`SearchIndexer::step`] (from the upkeep's tick) takes a bounded
//!   number of them, indexes what the store holds *now*, and saves the index's new files and drops
//!   the records it handled in one group of writes. A record only says "look again": handling it is
//!   idempotent and the order doesn't matter, so a crash at any point loses nothing.
//! - **Complete at search time.** A search ([`crate::search::SearchService::search`]) asks the
//!   saved index for everything except what still waits in the queue, and reads what waits from the
//!   store itself: the results are what an index built from the store right now would find, whether
//!   or not the indexer caught up.
//! - **A new format** (and a store from before the index) starts over: the indexer queues every
//!   conversation, and searches read those from the store until they are indexed.
//! - **Native only**, like tantivy: the browser keeps its substring scan and writes no queue.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::directory::RamDirectory;
use tantivy::indexer::LogMergePolicy;
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery, TermSetQuery};
use tantivy::schema::{
  BytesOptions, Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::{
  Directory, DocAddress, Index, IndexReader, IndexSettings, Order, ReloadPolicy, Searcher, TantivyDocument,
  Term,
};
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, Snowflake};

use crate::error::CoreError;
use crate::search::{
  BundleDirectory, ENGLISH, FLAG_FILE, FLAG_LINK, FLAG_THREAD, GERMAN, MAX_CANDIDATES, SearchMatch,
  SearchQuery, WRITER_MEMORY_BYTES, analyzers, fuzzy_query, raw, search_error, searchable_body, tokenizers,
};
use crate::store::{LocalStore, MessageFilter, StoreError, StoredMessage, Table, read_together, write_group};

/// The index's layout; a store with another one is indexed again from its messages.
const FORMAT: u32 = 1;
/// [`Table::SearchIndex`]: the [`Manifest`].
const MANIFEST_KEY: &str = "manifest";
/// [`Table::SearchIndex`]: `file/{name}/{chunk:05}`, the chunks of the index's files.
const FILE_PREFIX: &str = "file/";
/// The most of a file one record holds: big enough that a file is a few records, small enough that
/// no record is a burden to seal or to read back.
const CHUNK_BYTES: usize = 512 * 1024;
/// Messages one [`SearchIndexer::step`] indexes by default (a conversation queued whole counts all
/// of its messages, and is taken whole). About 50 ms of indexing on a laptop.
pub const DEFAULT_STEP_MESSAGES: usize = 5_000;
/// Segments of more documents than this aren't merged further: a merge stays bounded (a few hundred
/// milliseconds), at the price of a few more segments to look through.
const MAX_DOCS_BEFORE_MERGE: usize = 200_000;
/// The value of [`Table::SearchQueue`] records: who wrote it last, so the indexer drops a record
/// only if nobody wrote it again since it read it.
const NONCE_BYTES: usize = 8;

/// Fields of the local index. Only what a search needs is kept: the words (without positions, as no
/// query asks for phrases), the filters, and what identifies the message. No text is stored.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalFields {
  /// The conversation's and the message's IDs ([`message_key`]): what an update deletes by, and
  /// what a hit names.
  key: Field,
  conversation: Field,
  sender: Field,
  server_id: Field,
  sent_at: Field,
  /// [`FLAG_FILE`], [`FLAG_LINK`] and [`FLAG_THREAD`], one value each.
  tags: Field,
  body_en: Field,
  body_de: Field,
}

/// What [`Table::SearchIndex`] holds: the index's files and how far it got.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Manifest {
  format: u32,
  /// Counts the saves; a reader whose copy is of this generation has nothing to read.
  generation: u64,
  files: BTreeMap<String, FileRecord>,
}

/// One file of the index in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FileRecord {
  len: u64,
  chunks: u32,
  /// The generation that wrote it: tantivy rewrites `meta.json` under its name.
  generation: u64,
}

/// What waits in [`Table::SearchQueue`], with the nonce each record had when it was read.
#[derive(Debug, Default)]
pub(crate) struct Pending {
  pub(crate) conversations: BTreeMap<ConversationId, Vec<u8>>,
  /// By conversation and client message ID (as bytes: the ID has no order).
  pub(crate) messages: BTreeMap<(ConversationId, [u8; 16]), Vec<u8>>,
}

/// The index's files read from the store into memory, opened.
pub(crate) struct LoadedIndex {
  generation: u64,
  files: BTreeMap<String, FileRecord>,
  directory: BundleDirectory,
  index: Index,
  reader: IndexReader,
}

/// Keeps the local index up to date; one per writing client ([`crate::Client::index_search_step`]).
/// Holds the index in memory while there is work and lets it go once the queue is empty.
#[derive(Default)]
pub struct SearchIndexer {
  maybe_loaded: Option<LoadedIndex>,
}

/// What one [`SearchIndexer::step`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexStep {
  /// Queue records handled.
  pub handled: usize,
  /// Nothing waits any more.
  pub is_done: bool,
}

/// Work taken from the queue for one step, with the store's messages as they are now.
struct Batch {
  conversations: Vec<(ConversationId, Vec<u8>, Vec<StoredMessage>)>,
  messages: Vec<(ConversationId, ClientMessageId, Vec<u8>, Option<StoredMessage>)>,
}

impl SearchIndexer {
  pub fn new() -> Self {
    Self::default()
  }

  /// Indexes what waits in the queue, up to `budget` messages, and saves the index. Starts the
  /// index over when the store has none of this format.
  pub fn step(&mut self, store: &impl LocalStore, budget: usize) -> Result<IndexStep, CoreError> {
    let _step = crate::timing::step("search_index_step");
    let manifest = match read_manifest(store) {
      Ok(Some(manifest)) if manifest.format == FORMAT => manifest,
      Ok(_) | Err(CoreError::Corrupt(_)) => {
        self.maybe_loaded = None;
        start_over(store)?;
        return Ok(IndexStep::default());
      },
      Err(error) => return Err(error),
    };
    let pending = read_pending(store)?;
    if pending.is_empty() {
      // Nothing to do until messages are written: the memory goes back.
      self.maybe_loaded = None;
      return Ok(IndexStep {
        handled: 0,
        is_done: true,
      });
    }
    let batch = Batch::take(store, pending, budget)?;
    let result = self.apply(store, &manifest, &batch);
    if result.is_err() {
      // What is in memory may be ahead of the store: read it again next time.
      self.maybe_loaded = None;
    }
    result?;
    let handled = batch.conversations.len() + batch.messages.len();
    Ok(IndexStep {
      handled,
      is_done: read_pending(store)?.is_empty(),
    })
  }

  fn apply(&mut self, store: &impl LocalStore, manifest: &Manifest, batch: &Batch) -> Result<(), CoreError> {
    let loaded = match LoadedIndex::follow(self.maybe_loaded.take(), store, manifest) {
      Ok(loaded) => loaded,
      // A file of the index is missing or damaged: start over rather than stop indexing.
      Err(CoreError::Corrupt(error)) => {
        tracing::warn!(%error, "the local search index is unreadable; indexing again");
        start_over(store)?;
        return Ok(());
      },
      Err(error) => return Err(error),
    };
    let fields = local_fields();
    let mut writer: tantivy::IndexWriter<TantivyDocument> = loaded
      .index
      .writer_with_num_threads(1, WRITER_MEMORY_BYTES)
      .map_err(search_error)?;
    let mut policy = LogMergePolicy::default();
    policy.set_max_docs_before_merge(MAX_DOCS_BEFORE_MERGE);
    writer.set_merge_policy(Box::new(policy));
    for (conversation, _, messages) in &batch.conversations {
      writer.delete_term(Term::from_field_u64(fields.conversation, raw(*conversation)));
      for message in messages {
        if let Some(document) = local_document(&fields, message) {
          writer.add_document(document).map_err(search_error)?;
        }
      }
    }
    let whole: BTreeSet<ConversationId> = batch
      .conversations
      .iter()
      .map(|(conversation, ..)| *conversation)
      .collect();
    for (conversation, client_message_id, _, maybe_message) in &batch.messages {
      if whole.contains(conversation) {
        // Read with the whole conversation above.
        continue;
      }
      writer.delete_term(key_term(&fields, *conversation, *client_message_id));
      if let Some(document) = maybe_message
        .as_ref()
        .and_then(|message| local_document(&fields, message))
      {
        writer.add_document(document).map_err(search_error)?;
      }
    }
    writer.commit().map_err(search_error)?;
    writer.wait_merging_threads().map_err(search_error)?;
    let live = loaded.live_files()?;
    let maybe_saved = write_group(store, || save(store, manifest, &loaded, &live, batch))?;
    self.maybe_loaded = maybe_saved.map(|saved| loaded.after_save(saved));
    Ok(())
  }
}

impl std::fmt::Debug for SearchIndexer {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SearchIndexer")
      .field("is_loaded", &self.maybe_loaded.is_some())
      .finish()
  }
}

impl Batch {
  /// Takes queued records up to `budget` messages (whole conversations first, at least one record)
  /// and reads what the store holds of them now.
  fn take(store: &impl LocalStore, pending: Pending, budget: usize) -> Result<Self, CoreError> {
    let mut left = budget;
    let mut conversations = Vec::new();
    for (conversation, nonce) in pending.conversations {
      if left == 0 {
        break;
      }
      let messages = store.messages(conversation, MessageFilter::All)?;
      left = left.saturating_sub(messages.len().max(1));
      conversations.push((conversation, nonce, messages));
    }
    let mut messages = Vec::new();
    for ((conversation, client_message_id), nonce) in pending.messages {
      if left == 0 && !(conversations.is_empty() && messages.is_empty()) {
        break;
      }
      let client_message_id = ClientMessageId::from_bytes(client_message_id);
      let maybe_message = store.message(conversation, client_message_id)?;
      left = left.saturating_sub(1);
      messages.push((conversation, client_message_id, nonce, maybe_message));
    }
    Ok(Self {
      conversations,
      messages,
    })
  }
}

impl Pending {
  pub(crate) fn is_empty(&self) -> bool {
    self.conversations.is_empty() && self.messages.is_empty()
  }
}

impl LoadedIndex {
  /// The index as the store holds it, reading only the files `maybe_copy` doesn't have yet (an
  /// index file never changes under its name, except tantivy's `meta.json`).
  fn follow(
    maybe_copy: Option<Self>,
    store: &impl LocalStore,
    manifest: &Manifest,
  ) -> Result<Self, CoreError> {
    if manifest.files.is_empty() {
      return Self::create();
    }
    let (directory, mut files) = match maybe_copy {
      Some(copy) if copy.generation == manifest.generation => return Ok(copy),
      Some(copy) => (copy.directory, copy.files),
      None => (BundleDirectory::default(), BTreeMap::new()),
    };
    for (name, kept) in std::mem::take(&mut files) {
      if manifest.files.get(&name) == Some(&kept) {
        files.insert(name, kept);
      } else {
        directory.remove(Path::new(&name));
      }
    }
    for (name, record) in &manifest.files {
      if files.contains_key(name) {
        continue;
      }
      let data = read_file(store, name, record)?;
      directory.write(Path::new(name), &data)?;
      files.insert(name.clone(), *record);
    }
    let index = open_index(&directory)?;
    let reader = reader_of(&index)?;
    Ok(Self {
      generation: manifest.generation,
      files,
      directory,
      index,
      reader,
    })
  }

  /// A new, empty index.
  fn create() -> Result<Self, CoreError> {
    let directory = BundleDirectory::default();
    let index =
      Index::create(directory.clone(), local_schema().0, IndexSettings::default()).map_err(search_error)?;
    register_analyzers(&index);
    let reader = reader_of(&index)?;
    Ok(Self {
      generation: 0,
      files: BTreeMap::new(),
      directory,
      index,
      reader,
    })
  }

  pub(crate) fn searcher(&self) -> Searcher {
    self.reader.searcher()
  }

  /// The files of the committed index: `meta.json` and every file of its segments.
  fn live_files(&self) -> Result<BTreeSet<PathBuf>, CoreError> {
    use tantivy::index::SegmentComponent;
    let mut live = BTreeSet::from([PathBuf::from("meta.json")]);
    for segment in self.index.searchable_segment_metas().map_err(search_error)? {
      for component in [
        SegmentComponent::Postings,
        SegmentComponent::Positions,
        SegmentComponent::FastFields,
        SegmentComponent::FieldNorms,
        SegmentComponent::Terms,
        SegmentComponent::Store,
        SegmentComponent::Delete,
      ] {
        let path = segment.relative_path(component);
        if self.directory.exists(&path).unwrap_or(false) {
          live.insert(path);
        }
      }
    }
    Ok(live)
  }

  /// After `saved` was saved: keeps only its files.
  fn after_save(mut self, saved: Manifest) -> Self {
    for path in self.directory.paths() {
      if !saved.files.contains_key(path.to_string_lossy().as_ref()) {
        self.directory.remove(&path);
      }
    }
    self.files = saved.files;
    self.generation = saved.generation;
    self
  }
}

/// The local index's schema and fields.
pub(crate) fn local_schema() -> (Schema, LocalFields) {
  let mut builder = Schema::builder();
  let text = |tokenizer: &str| {
    TextOptions::default().set_indexing_options(
      TextFieldIndexing::default()
        .set_tokenizer(tokenizer)
        .set_index_option(IndexRecordOption::Basic)
        .set_fieldnorms(false),
    )
  };
  let indexed = NumericOptions::default().set_indexed();
  let fields = LocalFields {
    key: builder.add_bytes_field("key", BytesOptions::default().set_indexed().set_fast()),
    conversation: builder.add_u64_field("conversation", indexed.clone()),
    sender: builder.add_u64_field("sender", indexed.clone()),
    server_id: builder.add_i64_field("server_id", NumericOptions::default().set_fast()),
    sent_at: builder.add_i64_field("sent_at", NumericOptions::default().set_indexed().set_fast()),
    tags: builder.add_u64_field("tags", indexed),
    body_en: builder.add_text_field("body_en", text(ENGLISH)),
    body_de: builder.add_text_field("body_de", text(GERMAN)),
  };
  (builder.build(), fields)
}

fn local_fields() -> LocalFields {
  local_schema().1
}

/// An index of `messages` in memory, for what the saved index doesn't hold yet. `None` when none
/// of them is searchable.
pub(crate) fn index_in_memory<'a>(
  messages: impl Iterator<Item = &'a StoredMessage>,
) -> Result<Option<Index>, CoreError> {
  let fields = local_fields();
  let mut documents = messages
    .filter_map(|message| local_document(&fields, message))
    .peekable();
  if documents.peek().is_none() {
    return Ok(None);
  }
  let mut writer = Index::builder()
    .schema(local_schema().0)
    .tokenizers(tokenizers())
    .single_segment_index_writer(RamDirectory::create(), WRITER_MEMORY_BYTES)
    .map_err(search_error)?;
  for document in documents {
    writer.add_document(document).map_err(search_error)?;
  }
  writer.finalize().map(Some).map_err(search_error)
}

/// The newest [`MAX_CANDIDATES`] messages of `searcher` that match `query`, in `allowed`
/// conversations, leaving out what `pending` holds (it is read from the store instead).
pub(crate) fn find(
  searcher: &Searcher,
  query: &SearchQuery,
  allowed: &BTreeSet<ConversationId>,
  maybe_pending: Option<&Pending>,
) -> Result<Vec<SearchMatch>, CoreError> {
  // Automaton (fuzzy) queries panic in tantivy on a segment without documents.
  if searcher.num_docs() == 0 || allowed.is_empty() {
    return Ok(Vec::new());
  }
  let fields = local_fields();
  let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
    Occur::Must,
    Box::new(TermSetQuery::new(allowed.iter().map(|conversation| {
      Term::from_field_u64(fields.conversation, raw(*conversation))
    }))),
  )];
  if !query.terms.is_empty() {
    clauses.push((
      Occur::Must,
      fuzzy_query(&query.terms, [fields.body_en, fields.body_de]),
    ));
  }
  if let Some(sender) = query.maybe_sender {
    clauses.push((
      Occur::Must,
      term_query(Term::from_field_u64(fields.sender, raw(sender))),
    ));
  }
  if query.maybe_before_ms.is_some() || query.maybe_after_ms.is_some() {
    let bound = |maybe_ms: Option<i64>| {
      maybe_ms.map_or(Bound::Unbounded, |ms| {
        Bound::Excluded(Term::from_field_i64(fields.sent_at, ms))
      })
    };
    clauses.push((
      Occur::Must,
      Box::new(RangeQuery::new(
        bound(query.maybe_after_ms),
        bound(query.maybe_before_ms),
      )),
    ));
  }
  for (is_wanted, tag) in [
    (query.has_file, FLAG_FILE),
    (query.has_link, FLAG_LINK),
    (query.in_thread, FLAG_THREAD),
  ] {
    if is_wanted {
      clauses.push((Occur::Must, term_query(Term::from_field_u64(fields.tags, tag))));
    }
  }
  if let Some(pending) = maybe_pending {
    if !pending.conversations.is_empty() {
      clauses.push((
        Occur::MustNot,
        Box::new(TermSetQuery::new(pending.conversations.keys().map(
          |conversation| Term::from_field_u64(fields.conversation, raw(*conversation)),
        ))),
      ));
    }
    if !pending.messages.is_empty() {
      clauses.push((
        Occur::MustNot,
        Box::new(TermSetQuery::new(pending.messages.keys().map(
          |(conversation, client_message_id)| {
            key_term(
              &fields,
              *conversation,
              ClientMessageId::from_bytes(*client_message_id),
            )
          },
        ))),
      ));
    }
  }
  let newest = TopDocs::with_limit(MAX_CANDIDATES).order_by_fast_field::<i64>("sent_at", Order::Desc);
  let top = searcher
    .search(&BooleanQuery::new(clauses), &newest)
    .map_err(search_error)?;
  let mut found = Vec::with_capacity(top.len());
  let mut columns = SegmentColumns::default();
  for (maybe_sent_at, address) in top {
    if let Some(hit) = columns.hit(searcher, address, maybe_sent_at)? {
      found.push(hit);
    }
  }
  Ok(found)
}

/// The fast-field columns of the segment last read, for turning hits into [`SearchMatch`]es.
#[derive(Default)]
struct SegmentColumns {
  maybe_segment: Option<u32>,
  maybe_key: Option<tantivy::columnar::BytesColumn>,
  maybe_server_id: Option<tantivy::columnar::Column<i64>>,
  buffer: Vec<u8>,
}

impl SegmentColumns {
  fn hit(
    &mut self,
    searcher: &Searcher,
    address: DocAddress,
    maybe_sent_at: Option<i64>,
  ) -> Result<Option<SearchMatch>, CoreError> {
    if self.maybe_segment != Some(address.segment_ord) {
      let fast = searcher.segment_reader(address.segment_ord).fast_fields();
      self.maybe_key = fast.bytes("key").map_err(search_error)?;
      self.maybe_server_id = Some(fast.i64("server_id").map_err(search_error)?);
      self.maybe_segment = Some(address.segment_ord);
    }
    let (Some(key), Some(server_ids), Some(sent_at_ms)) =
      (&self.maybe_key, &self.maybe_server_id, maybe_sent_at)
    else {
      return Ok(None);
    };
    let Some(ord) = key.term_ords(address.doc_id).next() else {
      return Ok(None);
    };
    self.buffer.clear();
    key
      .ord_to_bytes(ord, &mut self.buffer)
      .map_err(|error| CoreError::Corrupt(format!("search index: {error}")))?;
    let Some((conversation_id, client_message_id)) = parse_message_key(&self.buffer) else {
      return Ok(None);
    };
    let maybe_server_id = server_ids
      .first(address.doc_id)
      .filter(|id| *id > 0)
      .and_then(|id| Snowflake::new(id).ok())
      .map(MessageId::from_snowflake);
    Ok(Some(SearchMatch {
      conversation_id,
      client_message_id,
      maybe_server_id,
      sent_at_ms,
    }))
  }
}

/// Reads the queue: what waits to be indexed. Keys that aren't the queue's are left out.
pub(crate) fn read_pending(store: &impl LocalStore) -> Result<Pending, CoreError> {
  let mut pending = Pending::default();
  for (key, nonce) in store.list(Table::SearchQueue)? {
    match key.split_once('/') {
      None => {
        if let Ok(conversation) = key.parse::<ConversationId>() {
          pending.conversations.insert(conversation, nonce);
        }
      },
      Some((conversation, client)) => {
        if let (Ok(conversation), Ok(client_message_id)) = (
          conversation.parse::<ConversationId>(),
          ClientMessageId::from_hex(client),
        ) {
          pending
            .messages
            .insert((conversation, *client_message_id.as_bytes()), nonce);
        }
      },
    }
  }
  Ok(pending)
}

/// The saved index as it stands (`None` before it exists or in another format), with what waits
/// for it, read as one look at the store, and `maybe_copy` brought up to it.
pub(crate) fn read_saved(
  store: &impl LocalStore,
  maybe_copy: Option<LoadedIndex>,
) -> Result<(Option<LoadedIndex>, Pending), CoreError> {
  read_together(store, || {
    let Some(manifest) = read_manifest(store)?.filter(|manifest| manifest.format == FORMAT) else {
      return Ok((None, read_pending(store)?));
    };
    let pending = read_pending(store)?;
    let loaded = LoadedIndex::follow(maybe_copy, store, &manifest)?;
    Ok((Some(loaded), pending))
  })
}

/// Queues `message` to be looked at again by the indexer: written (in any way) or removed.
pub(crate) fn queue_message(
  store: &(impl LocalStore + ?Sized),
  conversation: ConversationId,
  client_message_id: ClientMessageId,
) -> Result<(), StoreError> {
  store.put(
    Table::SearchQueue,
    &format!("{conversation}/{}", client_message_id.to_hex()),
    &nonce(),
  )
}

/// Queues all of `conversation` to be indexed again from what the store holds (its messages went).
pub(crate) fn queue_conversation(
  store: &(impl LocalStore + ?Sized),
  conversation: ConversationId,
) -> Result<(), StoreError> {
  store.put(Table::SearchQueue, &conversation.to_string(), &nonce())
}

/// A value no other write of a queue record of this process or another has: a random start, counted
/// up.
fn nonce() -> [u8; NONCE_BYTES] {
  static NEXT: OnceLock<AtomicU64> = OnceLock::new();
  NEXT
    .get_or_init(|| AtomicU64::new(rand_core::RngCore::next_u64(&mut rand_core::OsRng)))
    .fetch_add(1, Ordering::Relaxed)
    .to_be_bytes()
}

/// Wipes the index and queues every listed conversation, so it is built again from the store.
fn start_over(store: &impl LocalStore) -> Result<(), CoreError> {
  let previous = read_manifest(store).ok().flatten().unwrap_or_default();
  let conversations: Vec<ConversationId> = store
    .list(Table::Conversations)?
    .into_iter()
    .filter_map(|(key, _)| key.parse().ok())
    .collect();
  write_group(store, || {
    for (key, _) in store.list(Table::SearchIndex)? {
      store.delete(Table::SearchIndex, &key)?;
    }
    for conversation in &conversations {
      queue_conversation(store, *conversation)?;
    }
    write_manifest(
      store,
      &Manifest {
        format: FORMAT,
        generation: previous.generation + 1,
        files: BTreeMap::new(),
      },
    )
  })?;
  tracing::info!(
    conversations = conversations.len(),
    "the local search index starts over"
  );
  Ok(())
}

/// Saves the committed index and drops the queue records `batch` handled, unless somebody wrote
/// them again meanwhile. Returns what it saved; `None` (and saves nothing) if another writer saved
/// first.
fn save(
  store: &impl LocalStore,
  read: &Manifest,
  loaded: &LoadedIndex,
  live: &BTreeSet<PathBuf>,
  batch: &Batch,
) -> Result<Option<Manifest>, CoreError> {
  if read_manifest(store)?.as_ref() != Some(read) {
    tracing::debug!("the local search index was saved by another writer; this step is dropped");
    return Ok(None);
  }
  let generation = read.generation + 1;
  let mut files = BTreeMap::new();
  for path in live {
    let name = path.to_string_lossy().into_owned();
    match read.files.get(&name) {
      Some(record) if !is_rewritten(&name) => {
        files.insert(name, *record);
      },
      maybe_old => {
        let data = loaded.directory.read(path)?;
        let record = write_file(store, &name, &data, generation, maybe_old.copied())?;
        files.insert(name, record);
      },
    }
  }
  for (name, record) in &read.files {
    if !files.contains_key(name) {
      delete_file(store, name, record)?;
    }
  }
  let saved = Manifest {
    format: FORMAT,
    generation,
    files,
  };
  write_manifest(store, &saved)?;
  for (conversation, nonce, _) in &batch.conversations {
    drop_if_unchanged(store, &conversation.to_string(), nonce)?;
  }
  for (conversation, client_message_id, nonce, _) in &batch.messages {
    drop_if_unchanged(
      store,
      &format!("{conversation}/{}", client_message_id.to_hex()),
      nonce,
    )?;
  }
  Ok(Some(saved))
}

/// Files tantivy writes again under the same name.
fn is_rewritten(name: &str) -> bool {
  name == "meta.json" || name.starts_with('.')
}

fn drop_if_unchanged(store: &impl LocalStore, key: &str, nonce: &[u8]) -> Result<(), CoreError> {
  if store.get(Table::SearchQueue, key)?.as_deref() == Some(nonce) {
    store.delete(Table::SearchQueue, key)?;
  }
  Ok(())
}

fn read_manifest(store: &impl LocalStore) -> Result<Option<Manifest>, CoreError> {
  store
    .get(Table::SearchIndex, MANIFEST_KEY)?
    .map(|bytes| {
      serde_json::from_slice(&bytes)
        .map_err(|error| CoreError::Corrupt(format!("search index manifest: {error}")))
    })
    .transpose()
}

fn write_manifest(store: &impl LocalStore, manifest: &Manifest) -> Result<(), CoreError> {
  let bytes = serde_json::to_vec(manifest).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  store.put(Table::SearchIndex, MANIFEST_KEY, &bytes)?;
  Ok(())
}

fn read_file(store: &impl LocalStore, name: &str, record: &FileRecord) -> Result<Vec<u8>, CoreError> {
  let mut data = Vec::with_capacity(usize::try_from(record.len).unwrap_or(0));
  for chunk in 0..record.chunks {
    let chunk = store
      .get(Table::SearchIndex, &chunk_key(name, chunk))?
      .ok_or_else(|| CoreError::Corrupt(format!("search index file {name} is incomplete")))?;
    data.extend_from_slice(&chunk);
  }
  if data.len() as u64 != record.len {
    return Err(CoreError::Corrupt(format!(
      "search index file {name} has the wrong length"
    )));
  }
  Ok(data)
}

fn write_file(
  store: &impl LocalStore,
  name: &str,
  data: &[u8],
  generation: u64,
  maybe_old: Option<FileRecord>,
) -> Result<FileRecord, CoreError> {
  let record = FileRecord {
    len: data.len() as u64,
    chunks: chunk_count(data.len() as u64),
    generation,
  };
  for (at, chunk) in data.chunks(CHUNK_BYTES).enumerate() {
    let at =
      u32::try_from(at).map_err(|_| CoreError::Corrupt(format!("search index file {name} too large")))?;
    store.put(Table::SearchIndex, &chunk_key(name, at), chunk)?;
  }
  // A shorter file than before under the same name leaves no chunks behind.
  if let Some(old) = maybe_old {
    for chunk in record.chunks..old.chunks {
      store.delete(Table::SearchIndex, &chunk_key(name, chunk))?;
    }
  }
  Ok(record)
}

fn delete_file(store: &impl LocalStore, name: &str, record: &FileRecord) -> Result<(), CoreError> {
  for chunk in 0..record.chunks {
    store.delete(Table::SearchIndex, &chunk_key(name, chunk))?;
  }
  Ok(())
}

fn chunk_key(name: &str, chunk: u32) -> String {
  format!("{FILE_PREFIX}{name}/{chunk:05}")
}

fn chunk_count(len: u64) -> u32 {
  u32::try_from(len.div_ceil(CHUNK_BYTES as u64)).unwrap_or(u32::MAX)
}

fn open_index(directory: &BundleDirectory) -> Result<Index, CoreError> {
  let index = Index::open(directory.clone()).map_err(search_error)?;
  if index.schema() != local_schema().0 {
    return Err(CoreError::Corrupt("search index of another schema".into()));
  }
  register_analyzers(&index);
  Ok(index)
}

fn register_analyzers(index: &Index) {
  for (name, analyzer) in analyzers() {
    index.tokenizers().register(name, analyzer);
  }
}

fn reader_of(index: &Index) -> Result<IndexReader, CoreError> {
  index
    .reader_builder()
    .reload_policy(ReloadPolicy::Manual)
    .try_into()
    .map_err(search_error)
}

fn term_query(term: Term) -> Box<dyn Query> {
  Box::new(TermQuery::new(term, IndexRecordOption::Basic))
}

fn key_term(fields: &LocalFields, conversation: ConversationId, client_message_id: ClientMessageId) -> Term {
  Term::from_field_bytes(fields.key, &message_key(conversation, client_message_id))
}

/// The conversation's ID (8 bytes) and the message's (16): unique in the store.
fn message_key(conversation: ConversationId, client_message_id: ClientMessageId) -> [u8; 24] {
  let mut key = [0u8; 24];
  key[..8].copy_from_slice(&conversation.snowflake().get().to_be_bytes());
  key[8..].copy_from_slice(client_message_id.as_bytes());
  key
}

fn parse_message_key(bytes: &[u8]) -> Option<(ConversationId, ClientMessageId)> {
  let (conversation, client) = bytes.split_first_chunk::<8>()?;
  let client: [u8; 16] = client.try_into().ok()?;
  let conversation = Snowflake::new(i64::from_be_bytes(*conversation)).ok()?;
  Some((
    ConversationId::from_snowflake(conversation),
    ClientMessageId::from_bytes(client),
  ))
}

/// The document of a message search covers; `None` for the others.
fn local_document(fields: &LocalFields, message: &StoredMessage) -> Option<TantivyDocument> {
  let (body, flags) = searchable_body(message)?;
  let mut document = TantivyDocument::default();
  document.add_bytes(
    fields.key,
    &message_key(message.conversation_id, message.client_message_id),
  );
  document.add_u64(fields.conversation, raw(message.conversation_id));
  document.add_u64(fields.sender, raw(message.sender_user_id));
  document.add_i64(
    fields.server_id,
    message.maybe_server_id.map_or(0, |id| id.snowflake().get()),
  );
  document.add_i64(fields.sent_at, message.sent_at_ms);
  for tag in [FLAG_FILE, FLAG_LINK, FLAG_THREAD] {
    if flags & tag != 0 {
      document.add_u64(fields.tags, tag);
    }
  }
  document.add_text(fields.body_en, &body);
  document.add_text(fields.body_de, &body);
  Some(document)
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use teamcraft_proto::ThreadId;

  use super::*;
  use crate::search::{SearchService, export_segment, is_searchable};
  use crate::search_scope::ConversationScope;
  use crate::store::{MemoryStore, MessageContent, TrackedStore};

  type Store = TrackedStore<Arc<MemoryStore>>;

  const WORDS: [&str; 12] = [
    "budget",
    "review",
    "deployment",
    "lunch",
    "invoice",
    "Häuser",
    "gebaut",
    "planning",
    "release",
    "3000",
    "ticket",
    "spring",
  ];

  /// What the index finds is what an index of all stored messages finds, before it is built, while
  /// it is built step by step, and after edits, deletions and removals.
  #[test]
  fn results_follow_the_store_through_edits_and_deletions() {
    let store = store_with(3, 120);
    let mut service = SearchService::new();
    let mut indexer = SearchIndexer::new();
    assert_matches_reference(&store, &mut service);

    // Built in steps of 50 messages: the queue shrinks, results stay complete.
    let mut steps = 0;
    loop {
      let step = indexer.step(&store, 50).unwrap();
      assert_matches_reference(&store, &mut service);
      steps += 1;
      if step.is_done {
        break;
      }
      assert!(steps < 100, "the index never caught up");
    }
    assert!(steps > 5, "{steps} steps");
    assert!(read_pending(&store).unwrap().is_empty());

    // Edits, deletions (by the sender: the message turns `Deleted`), a dropped message, a
    // conversation that goes, and new messages.
    let conversation = conversation_id(1);
    let mut edited = store.message(conversation, client_id(1, 3)).unwrap().unwrap();
    edited.content = MessageContent::text("now about the spring release", Vec::new());
    edited.is_edited = true;
    store.upsert_message(&edited).unwrap();
    let mut withdrawn = store.message(conversation, client_id(1, 4)).unwrap().unwrap();
    withdrawn.content = MessageContent::Deleted;
    store.upsert_message(&withdrawn).unwrap();
    store.delete_message(conversation, client_id(1, 5)).unwrap();
    store.delete_messages(conversation_id(2)).unwrap();
    store
      .upsert_message(&message(1, 500, "a new budget review"))
      .unwrap();
    // Searches find all of it before the indexer ran.
    assert_matches_reference(&store, &mut service);
    let step = indexer.step(&store, 10_000).unwrap();
    assert!(step.is_done);
    assert_eq!(step.handled, 5);
    assert_matches_reference(&store, &mut service);

    // A service and an indexer that start fresh (an app start) use the saved index as it is.
    let mut fresh = SearchService::new();
    assert_matches_reference(&store, &mut fresh);
    assert_eq!(
      SearchIndexer::new().step(&store, 10_000).unwrap(),
      IndexStep {
        handled: 0,
        is_done: true
      }
    );
  }

  /// A step reads at most its budget of queued messages, and only what was written since.
  #[test]
  fn steps_are_bounded_and_incremental() {
    let store = store_with(1, 300);
    let mut indexer = SearchIndexer::new();
    // The first step starts the index: every conversation is queued whole.
    assert_eq!(indexer.step(&store, 100).unwrap().handled, 0);
    assert_eq!(read_pending(&store).unwrap().conversations.len(), 1);
    // A queued conversation is taken whole (its 300 messages were already queued one by one and
    // are handled with it).
    while !indexer.step(&store, 100).unwrap().is_done {}
    assert!(read_pending(&store).unwrap().is_empty());
    for n in 0..30 {
      store
        .upsert_message(&message(1, 1_000 + n, "late budget numbers"))
        .unwrap();
    }
    let first = indexer.step(&store, 10).unwrap();
    assert_eq!(
      first,
      IndexStep {
        handled: 10,
        is_done: false
      }
    );
    assert_eq!(read_pending(&store).unwrap().messages.len(), 20);
    while !indexer.step(&store, 10).unwrap().is_done {}
    let mut service = SearchService::new();
    assert_eq!(search(&mut service, &store, &["late"]).len(), 30);
  }

  /// A message written again while the indexer worked on it stays queued, so its new text is
  /// indexed by the next step.
  #[test]
  fn a_message_written_during_a_step_stays_queued() {
    let store = store_with(1, 3);
    let mut indexer = SearchIndexer::new();
    while !indexer.step(&store, 100).unwrap().is_done {}
    store
      .upsert_message(&message(1, 1, "first draft of the budget"))
      .unwrap();
    let manifest = read_manifest(&store).unwrap().unwrap();
    let batch = Batch::take(&store, read_pending(&store).unwrap(), 100).unwrap();
    // Written again after the step read it.
    store.upsert_message(&message(1, 1, "final invoice")).unwrap();
    indexer.apply(&store, &manifest, &batch).unwrap();
    assert_eq!(read_pending(&store).unwrap().messages.len(), 1);
    let mut service = SearchService::new();
    assert!(search(&mut service, &store, &["draft"]).is_empty());
    assert_eq!(search(&mut service, &store, &["invoice"]).len(), 1);
    indexer.step(&store, 100).unwrap();
    assert!(read_pending(&store).unwrap().is_empty());
    assert!(search(&mut service, &store, &["draft"]).is_empty());
    assert_eq!(search(&mut service, &store, &["invoice"]).len(), 1);
  }

  /// A damaged index is built again; searches meanwhile read the store.
  #[test]
  fn a_damaged_index_starts_over() {
    let store = store_with(2, 40);
    let mut indexer = SearchIndexer::new();
    while !indexer.step(&store, 1_000).unwrap().is_done {}
    let mut service = SearchService::new();
    let before = search(&mut service, &store, &["budget"]);
    assert!(!before.is_empty());

    // A chunk of a file goes missing.
    let (key, _) = store
      .list(Table::SearchIndex)
      .unwrap()
      .into_iter()
      .find(|(key, _)| key.starts_with(FILE_PREFIX) && !key.contains("meta.json"))
      .unwrap();
    store.delete(Table::SearchIndex, &key).unwrap();
    let mut fresh = SearchService::new();
    assert_eq!(search(&mut fresh, &store, &["budget"]), before);
    let mut fresh_indexer = SearchIndexer::new();
    while !fresh_indexer.step(&store, 1_000).unwrap().is_done {}
    assert_eq!(search(&mut SearchService::new(), &store, &["budget"]), before);

    // The manifest isn't readable at all.
    store.put(Table::SearchIndex, MANIFEST_KEY, b"not json").unwrap();
    assert_eq!(search(&mut SearchService::new(), &store, &["budget"]), before);
    while !fresh_indexer.step(&store, 1_000).unwrap().is_done {}
    assert_eq!(search(&mut SearchService::new(), &store, &["budget"]), before);
  }

  /// The index's files are records of the store: nothing is written anywhere else, and a file is
  /// cut into records of at most [`CHUNK_BYTES`].
  #[test]
  fn the_index_lives_in_the_store() {
    let store = store_with(4, 200);
    let mut indexer = SearchIndexer::new();
    while !indexer.step(&store, 10_000).unwrap().is_done {}
    let manifest = read_manifest(&store).unwrap().unwrap();
    assert!(manifest.files.contains_key("meta.json"));
    let records = store.list(Table::SearchIndex).unwrap();
    let chunks: usize = manifest.files.values().map(|record| record.chunks as usize).sum();
    assert_eq!(records.len(), chunks + 1);
    assert!(records.iter().all(|(_, value)| value.len() <= CHUNK_BYTES));
  }

  fn assert_matches_reference(store: &Store, service: &mut SearchService) {
    for words in [
      &["budget"][..],
      &["budgett"],
      &["review", "budget"],
      &["häuser"],
      &["3000"],
      &["spring"],
      &[],
    ] {
      for scope in [
        ConversationScope::All,
        ConversationScope::Only(BTreeSet::from([conversation_id(1)])),
      ] {
        let query = SearchQuery {
          terms: words.iter().map(|word| word.to_string()).collect(),
          conversations: scope.clone(),
          ..SearchQuery::default()
        };
        let found: Vec<SearchMatch> = service.search(store, &query).unwrap().hits;
        assert_eq!(found, reference(store, &query), "{words:?} in {scope:?}");
      }
    }
    let filtered = SearchQuery {
      terms: vec!["budget".into()],
      maybe_sender: Some(id(2)),
      in_thread: true,
      maybe_after_ms: Some(1_000_000 + 10 * 60_000),
      ..SearchQuery::default()
    };
    assert_eq!(
      service.search(store, &filtered).unwrap().hits,
      reference(store, &filtered)
    );
  }

  /// What an index of every stored message finds, as search did before the local index: built in
  /// memory for the search, every match, newest first.
  fn reference(store: &Store, query: &SearchQuery) -> Vec<SearchMatch> {
    let mut messages = Vec::new();
    for (key, _) in store.list(Table::Conversations).unwrap() {
      let conversation: ConversationId = key.parse().unwrap();
      if !query.conversations.includes(conversation) {
        continue;
      }
      messages.extend(
        store
          .messages(conversation, MessageFilter::All)
          .unwrap()
          .into_iter()
          .filter(is_searchable),
      );
    }
    let bundle = export_segment(messages.iter()).unwrap();
    let mut with_segment = SearchService::new();
    let scratch = MemoryStore::new();
    for message in &messages {
      scratch.upsert_message(message).unwrap();
    }
    scratch.put(Table::SearchSegments, "all", &bundle).unwrap();
    let mut hits = with_segment.search(&scratch, query).unwrap().hits;
    hits.sort_by_key(|hit| {
      (
        std::cmp::Reverse(hit.sent_at_ms),
        *hit.client_message_id.as_bytes(),
      )
    });
    hits
  }

  fn search(service: &mut SearchService, store: &Store, words: &[&str]) -> Vec<SearchMatch> {
    let query = SearchQuery {
      terms: words.iter().map(|word| word.to_string()).collect(),
      ..SearchQuery::default()
    };
    service.search(store, &query).unwrap().hits
  }

  /// `conversations` listed conversations of `per_conversation` messages each, with words of
  /// [`WORDS`], some in threads and with links, from two senders.
  fn store_with(conversations: i64, per_conversation: i64) -> Store {
    let store = TrackedStore::new(Arc::new(MemoryStore::new()));
    for c in 1..=conversations {
      store
        .put(Table::Conversations, &conversation_id(c).to_string(), b"{}")
        .unwrap();
      for n in 0..per_conversation {
        let at = usize::try_from(n).unwrap();
        let body = format!(
          "{} {} {}{}",
          WORDS[at % WORDS.len()],
          WORDS[(at * 7 + 3) % WORDS.len()],
          WORDS[(at * 5 + 1) % WORDS.len()],
          if n % 9 == 0 { " https://example.com" } else { "" }
        );
        store.upsert_message(&message(c, n, &body)).unwrap();
      }
    }
    store
  }

  fn message(conversation: i64, n: i64, body: &str) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation_id(conversation),
      client_message_id: client_id(conversation, n),
      maybe_server_id: Some(id(1_000_000 * conversation + n + 1)),
      sender_user_id: id(1 + n % 2),
      maybe_thread_id: (n % 4 == 0).then(|| id::<ThreadId>(77)),
      sent_at_ms: 1_000_000 + n * 60_000 + conversation,
      content: MessageContent::text(body, Vec::new()),
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  fn client_id(conversation: i64, n: i64) -> ClientMessageId {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&conversation.to_be_bytes());
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    ClientMessageId::from_bytes(bytes)
  }

  fn conversation_id(n: i64) -> ConversationId {
    id(100 + n)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
