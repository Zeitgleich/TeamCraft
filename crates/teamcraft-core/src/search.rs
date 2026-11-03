//! Full-text search (e2ee.md §10.1) with tantivy, over the decrypted local store and over shared
//! index segments.
//!
//! - Every search builds an in-memory index of the local store's messages (native only; the
//!   browser scans the store instead, see the crate README). The index never touches the disk.
//! - A *segment* is the index of one conversation and month, exported as a bundle of tantivy's files.
//!   Members publish closed months encrypted like attachments; other devices import them, so full
//!   history is searchable without re-indexing (see [`crate::Client::publish_index_segments`]).
//! - Hits are only shown from messages this device decrypted itself: a manipulated segment can at
//!   most point to a wrong message, never invent content. Hits from segments whose message isn't
//!   local yet are reported so the engine can fetch them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use tantivy::Term;
use tantivy::collector::TopDocs;
use tantivy::directory::error::{DeleteError, OpenReadError, OpenWriteError};
use tantivy::directory::{FileHandle, RamDirectory, WatchCallback, WatchHandle, WritePtr};
use tantivy::query::{AllQuery, BooleanQuery, EmptyQuery, FuzzyTermQuery, Occur, Query};
use tantivy::schema::{
  Field, IndexRecordOption, NumericOptions, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{AsciiFoldingFilter, Language, LowerCaser, SimpleTokenizer, Stemmer, TextAnalyzer};
use tantivy::{Directory, Index, TantivyDocument};
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, Snowflake, UserId};

pub use crate::search_scope::ConversationScope;

use crate::error::CoreError;
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};

/// Matches per search, counted after the filters: a filter applied after the cap would drop
/// everything when other conversations fill it (issue #873).
const MAX_CANDIDATES: usize = 2_000;
/// Indexing memory per writer (tantivy's minimum is 15 MB).
const WRITER_MEMORY_BYTES: usize = 15_000_000;
const ENGLISH: &str = "teamcraft_en";
const GERMAN: &str = "teamcraft_de";
const FLAG_FILE: u64 = 1;
const FLAG_LINK: u64 = 2;
const FLAG_THREAD: u64 = 4;

/// What to look for. Terms must all match (in English or German word forms); the rest filters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
  pub terms: Vec<String>,
  pub conversations: ConversationScope,
  pub maybe_sender: Option<UserId>,
  /// Only messages sent before this time (Unix milliseconds).
  pub maybe_before_ms: Option<i64>,
  pub maybe_after_ms: Option<i64>,
  pub has_file: bool,
  pub has_link: bool,
  pub in_thread: bool,
}

/// A matching message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub maybe_server_id: Option<MessageId>,
  pub sent_at_ms: i64,
}

/// Search results: hits from local messages, newest first, and segment hits whose messages this
/// device still has to fetch (and decrypt) before showing them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchResults {
  pub hits: Vec<SearchMatch>,
  pub to_fetch: Vec<SearchMatch>,
}

/// Imported segments, kept open between searches.
#[derive(Default)]
pub struct SearchService {
  segments: HashMap<String, Index>,
}

#[derive(Clone, Copy)]
struct Fields {
  client_message_id: Field,
  conversation_id: Field,
  sender_id: Field,
  server_id: Field,
  sent_at: Field,
  flags: Field,
  body_en: Field,
  body_de: Field,
}

/// A tantivy directory in memory that remembers which files exist, so an index can be exported.
#[derive(Clone, Debug, Default)]
struct BundleDirectory {
  inner: RamDirectory,
  files: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl SearchService {
  pub fn new() -> Self {
    Self::default()
  }

  /// Searches the local store and every imported segment.
  pub fn search(&mut self, store: &impl LocalStore, query: &SearchQuery) -> Result<SearchResults, CoreError> {
    let local_messages = searchable_messages(store, &query.conversations)?;
    let local_index = build_index(local_messages.iter())?;
    let mut matches: BTreeMap<[u8; 16], SearchMatch> = BTreeMap::new();
    for found in run_query(&local_index, query)? {
      matches.insert(*found.client_message_id.as_bytes(), found);
    }
    self.load_segments(store)?;
    let mut to_fetch = Vec::new();
    for index in self.segments.values() {
      for found in run_query(index, query)? {
        if matches.contains_key(found.client_message_id.as_bytes()) {
          continue;
        }
        match store.message(found.conversation_id, found.client_message_id)? {
          // Only what this device decrypted is shown; the segment just pointed to it.
          Some(stored) if is_searchable(&stored) && passes_filters(&stored, query) => {
            matches.insert(*found.client_message_id.as_bytes(), found);
          },
          Some(_) => {},
          None => to_fetch.push(found),
        }
      }
    }
    let mut hits: Vec<SearchMatch> = matches.into_values().collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.sent_at_ms));
    Ok(SearchResults { hits, to_fetch })
  }

  fn load_segments(&mut self, store: &impl LocalStore) -> Result<(), CoreError> {
    for (key, bundle) in store.list(Table::SearchSegments)? {
      if let std::collections::hash_map::Entry::Vacant(slot) = self.segments.entry(key) {
        match import_segment(&bundle) {
          Ok(index) => {
            slot.insert(index);
          },
          Err(error) => tracing::warn!(%error, "ignored an unreadable index segment"),
        }
      }
    }
    Ok(())
  }
}

impl std::fmt::Debug for SearchService {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SearchService")
      .field("segments", &self.segments.len())
      .finish()
  }
}

/// Builds the segment of the given messages and exports it as a bundle of tantivy's files.
pub fn export_segment<'a>(messages: impl Iterator<Item = &'a StoredMessage>) -> Result<Vec<u8>, CoreError> {
  let directory = BundleDirectory::default();
  let (schema, _) = schema();
  let mut writer = Index::builder()
    .schema(schema)
    .tokenizers(tokenizers())
    .single_segment_index_writer(directory.clone(), WRITER_MEMORY_BYTES)
    .map_err(search_error)?;
  let fields = fields();
  for message in messages.filter(|message| is_searchable(message)) {
    writer
      .add_document(document(&fields, message))
      .map_err(search_error)?;
  }
  writer.finalize().map_err(search_error)?;
  directory.bundle()
}

/// The month (`YYYY-MM`, UTC) a message belongs to, for segments.
pub fn month_of(sent_at_ms: i64) -> String {
  let days = sent_at_ms.div_euclid(86_400_000);
  // Civil-from-days (Howard Hinnant's algorithm).
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted - era * 146_097;
  let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_index = (5 * day_of_year + 2) / 153;
  let month = if month_index < 10 {
    month_index + 3
  } else {
    month_index - 9
  };
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  format!("{year:04}-{month:02}")
}

/// Messages search covers: text and file names, not reactions, edits, deletes or announcements.
pub fn is_searchable(message: &StoredMessage) -> bool {
  matches!(
    message.content,
    MessageContent::Text { .. } | MessageContent::File { .. }
  )
}

fn searchable_messages(
  store: &impl LocalStore,
  scope: &ConversationScope,
) -> Result<Vec<StoredMessage>, CoreError> {
  let conversations: Vec<ConversationId> = match scope {
    ConversationScope::Only(conversations) => conversations.iter().copied().collect(),
    ConversationScope::All => store
      .list(Table::Conversations)?
      .into_iter()
      .filter_map(|(key, _)| serde_json::from_value(serde_json::Value::String(key)).ok())
      .collect(),
  };
  let mut messages = Vec::new();
  for conversation in conversations {
    messages.extend(
      store
        .messages(conversation, MessageFilter::All)?
        .into_iter()
        .filter(is_searchable),
    );
  }
  Ok(messages)
}

fn build_index<'a>(messages: impl Iterator<Item = &'a StoredMessage>) -> Result<Index, CoreError> {
  let bundle = export_segment(messages)?;
  import_segment(&bundle)
}

fn import_segment(bundle: &[u8]) -> Result<Index, CoreError> {
  let directory = BundleDirectory::from_bundle(bundle)?;
  let index = Index::open(directory).map_err(search_error)?;
  for (name, analyzer) in analyzers() {
    index.tokenizers().register(name, analyzer);
  }
  Ok(index)
}

fn run_query(index: &Index, query: &SearchQuery) -> Result<Vec<SearchMatch>, CoreError> {
  let fields = fields();
  let reader = index.reader().map_err(search_error)?;
  let searcher = reader.searcher();
  // Automaton (fuzzy) queries panic in tantivy on a segment without documents.
  if searcher.num_docs() == 0 {
    return Ok(Vec::new());
  }
  let parsed: Box<dyn Query> = if query.terms.is_empty() {
    Box::new(AllQuery)
  } else {
    fuzzy_query(&query.terms, &fields)
  };
  // Every match in score order, so the filters see all of them before the cap.
  let all_docs = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
  let top = searcher
    .search(&parsed, &TopDocs::with_limit(all_docs).order_by_score())
    .map_err(search_error)?;
  let mut found = Vec::new();
  for (_, address) in top {
    let document: TantivyDocument = searcher.doc(address).map_err(search_error)?;
    if let Some(hit) = to_match(&fields, &document, query) {
      found.push(hit);
      if found.len() == MAX_CANDIDATES {
        break;
      }
    }
  }
  Ok(found)
}

/// Every analysed word of every term must match (AND), in its English or German form (OR), allowing
/// typos: edit distance 0 up to 3 characters, 1 up to 7, then 2. Words with digits (amounts, years,
/// ticket numbers) match exactly: `3000` must not find `2000`. User input is never parsed as query
/// syntax, only tokenised.
fn fuzzy_query(terms: &[String], fields: &Fields) -> Box<dyn Query> {
  let mut analyzers = analyzers();
  let mut required: Vec<(Occur, Box<dyn Query>)> = Vec::new();
  for term in terms {
    let mut words: Vec<Vec<(tantivy::schema::Field, String)>> = Vec::new();
    for (field, (_, analyzer)) in [fields.body_en, fields.body_de]
      .into_iter()
      .zip(analyzers.iter_mut())
    {
      let mut stream = analyzer.token_stream(term);
      let mut index = 0;
      while stream.advance() {
        if words.len() <= index {
          words.resize_with(index + 1, Vec::new);
        }
        words[index].push((field, stream.token().text.clone()));
        index += 1;
      }
    }
    for alternatives in words {
      let clauses = alternatives
        .into_iter()
        .map(|(field, text)| {
          let distance = match text.chars().count() {
            _ if text.contains(|ch: char| ch.is_ascii_digit()) => 0,
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
          };
          let query: Box<dyn Query> = Box::new(FuzzyTermQuery::new(
            Term::from_field_text(field, &text),
            distance,
            true,
          ));
          (Occur::Should, query)
        })
        .collect();
      required.push((Occur::Must, Box::new(BooleanQuery::new(clauses))));
    }
  }
  if required.is_empty() {
    Box::new(EmptyQuery)
  } else {
    Box::new(BooleanQuery::new(required))
  }
}

fn to_match(fields: &Fields, document: &TantivyDocument, query: &SearchQuery) -> Option<SearchMatch> {
  let client_message_id =
    ClientMessageId::from_hex(document.get_first(fields.client_message_id)?.as_str()?).ok()?;
  let conversation_id = ConversationId::from_snowflake(
    Snowflake::new(i64::try_from(document.get_first(fields.conversation_id)?.as_u64()?).ok()?).ok()?,
  );
  let sender = UserId::from_snowflake(
    Snowflake::new(i64::try_from(document.get_first(fields.sender_id)?.as_u64()?).ok()?).ok()?,
  );
  let server_id = document.get_first(fields.server_id)?.as_i64()?;
  let sent_at_ms = document.get_first(fields.sent_at)?.as_i64()?;
  let flags = document.get_first(fields.flags)?.as_u64()?;
  let is_wanted = query.conversations.includes(conversation_id)
    && query.maybe_sender.is_none_or(|wanted| wanted == sender)
    && query.maybe_before_ms.is_none_or(|before| sent_at_ms < before)
    && query.maybe_after_ms.is_none_or(|after| sent_at_ms > after)
    && (!query.has_file || flags & FLAG_FILE != 0)
    && (!query.has_link || flags & FLAG_LINK != 0)
    && (!query.in_thread || flags & FLAG_THREAD != 0);
  is_wanted.then(|| SearchMatch {
    conversation_id,
    client_message_id,
    maybe_server_id: Snowflake::new(server_id)
      .ok()
      .filter(|_| server_id > 0)
      .map(MessageId::from_snowflake),
    sent_at_ms,
  })
}

fn passes_filters(message: &StoredMessage, query: &SearchQuery) -> bool {
  query
    .maybe_sender
    .is_none_or(|sender| sender == message.sender_user_id)
    && query
      .maybe_before_ms
      .is_none_or(|before| message.sent_at_ms < before)
    && query
      .maybe_after_ms
      .is_none_or(|after| message.sent_at_ms > after)
}

fn document(fields: &Fields, message: &StoredMessage) -> TantivyDocument {
  let (body, mut flags) = match &message.content {
    MessageContent::Text { body, .. } => (body.clone(), 0),
    MessageContent::File { attachment } => (attachment.file_name.clone(), FLAG_FILE),
    _ => (String::new(), 0),
  };
  if body.contains("http://") || body.contains("https://") {
    flags |= FLAG_LINK;
  }
  if message.maybe_thread_id.is_some() {
    flags |= FLAG_THREAD;
  }
  let mut document = TantivyDocument::default();
  document.add_text(fields.client_message_id, message.client_message_id.to_hex());
  document.add_u64(fields.conversation_id, raw(message.conversation_id));
  document.add_u64(fields.sender_id, raw(message.sender_user_id));
  document.add_i64(
    fields.server_id,
    message.maybe_server_id.map_or(0, |id| id.snowflake().get()),
  );
  document.add_i64(fields.sent_at, message.sent_at_ms);
  document.add_u64(fields.flags, flags);
  document.add_text(fields.body_en, &body);
  document.add_text(fields.body_de, &body);
  document
}

fn raw(id: impl Id) -> u64 {
  u64::try_from(id.snowflake().get()).unwrap_or(0)
}

fn schema() -> (Schema, Fields) {
  let mut builder = Schema::builder();
  let stored_number = NumericOptions::default().set_stored();
  let text = |tokenizer: &str| {
    TextOptions::default().set_indexing_options(
      TextFieldIndexing::default()
        .set_tokenizer(tokenizer)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    )
  };
  let fields = Fields {
    client_message_id: builder.add_text_field("client_message_id", STRING | STORED),
    conversation_id: builder.add_u64_field("conversation_id", stored_number.clone()),
    sender_id: builder.add_u64_field("sender_id", stored_number.clone()),
    server_id: builder.add_i64_field("server_id", stored_number.clone()),
    sent_at: builder.add_i64_field("sent_at", stored_number.clone()),
    flags: builder.add_u64_field("flags", stored_number),
    body_en: builder.add_text_field("body_en", text(ENGLISH)),
    body_de: builder.add_text_field("body_de", text(GERMAN)),
  };
  (builder.build(), fields)
}

fn fields() -> Fields {
  schema().1
}

fn analyzers() -> [(&'static str, TextAnalyzer); 2] {
  let analyzer = |language| {
    TextAnalyzer::builder(SimpleTokenizer::default())
      .filter(LowerCaser)
      .filter(AsciiFoldingFilter)
      .filter(Stemmer::new(language))
      .build()
  };
  [
    (ENGLISH, analyzer(Language::English)),
    (GERMAN, analyzer(Language::German)),
  ]
}

fn tokenizers() -> tantivy::tokenizer::TokenizerManager {
  let manager = tantivy::tokenizer::TokenizerManager::default();
  for (name, analyzer) in analyzers() {
    manager.register(name, analyzer);
  }
  manager
}

fn search_error(error: tantivy::TantivyError) -> CoreError {
  CoreError::Corrupt(format!("search index: {error}"))
}

impl BundleDirectory {
  fn record(&self, path: &Path, is_present: bool) {
    let mut files = self.files.lock().unwrap_or_else(PoisonError::into_inner);
    if is_present {
      files.insert(path.to_path_buf());
    } else {
      files.remove(path);
    }
  }

  /// `count (u32) || (name length (u16) || name || data length (u32) || data)*`, lock files left out.
  fn bundle(&self) -> Result<Vec<u8>, CoreError> {
    let files: Vec<PathBuf> = self
      .files
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .cloned()
      .collect();
    let mut entries = Vec::new();
    for path in files
      .iter()
      .filter(|path| !path.to_string_lossy().ends_with(".lock"))
    {
      let data = self
        .inner
        .open_read(path)
        .and_then(|slice| {
          slice
            .read_bytes()
            .map_err(|error| OpenReadError::wrap_io_error(error, path.clone()))
        })
        .map_err(|error| CoreError::Corrupt(error.to_string()))?;
      entries.push((path.to_string_lossy().into_owned(), data.as_slice().to_vec()));
    }
    let mut out = (entries.len() as u32).to_be_bytes().to_vec();
    for (name, data) in entries {
      let name_len =
        u16::try_from(name.len()).map_err(|_| CoreError::Corrupt("file name too long".into()))?;
      out.extend_from_slice(&name_len.to_be_bytes());
      out.extend_from_slice(name.as_bytes());
      out.extend_from_slice(&(data.len() as u32).to_be_bytes());
      out.extend_from_slice(&data);
    }
    Ok(out)
  }

  fn from_bundle(bundle: &[u8]) -> Result<Self, CoreError> {
    let corrupt = || CoreError::Corrupt("malformed index segment".into());
    let directory = Self::default();
    let (count, mut rest) = bundle.split_first_chunk::<4>().ok_or_else(corrupt)?;
    for _ in 0..u32::from_be_bytes(*count) {
      let (name_len, tail) = rest.split_first_chunk::<2>().ok_or_else(corrupt)?;
      let (name, tail) = tail
        .split_at_checked(usize::from(u16::from_be_bytes(*name_len)))
        .ok_or_else(corrupt)?;
      let name = std::str::from_utf8(name).map_err(|_| corrupt())?;
      // Only plain file names: a segment must not address anything outside its directory.
      if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(corrupt());
      }
      let (data_len, tail) = tail.split_first_chunk::<4>().ok_or_else(corrupt)?;
      let (data, tail) = tail
        .split_at_checked(u32::from_be_bytes(*data_len) as usize)
        .ok_or_else(corrupt)?;
      directory
        .atomic_write(Path::new(name), data)
        .map_err(|_| corrupt())?;
      rest = tail;
    }
    if !rest.is_empty() {
      return Err(corrupt());
    }
    Ok(directory)
  }
}

impl Directory for BundleDirectory {
  fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
    self.inner.get_file_handle(path)
  }

  fn delete(&self, path: &Path) -> Result<(), DeleteError> {
    self.inner.delete(path)?;
    self.record(path, false);
    Ok(())
  }

  fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
    self.inner.exists(path)
  }

  fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
    let writer = self.inner.open_write(path)?;
    self.record(path, true);
    Ok(writer)
  }

  fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
    self.inner.atomic_read(path)
  }

  fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
    self.inner.atomic_write(path, data)?;
    self.record(path, true);
    Ok(())
  }

  fn sync_directory(&self) -> io::Result<()> {
    self.inner.sync_directory()
  }

  fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
    self.inner.watch(watch_callback)
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::ThreadId;

  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn finds_words_in_both_languages_and_applies_filters() {
    let store = MemoryStore::new();
    store.put(Table::Conversations, "10", b"{}").unwrap();
    let mut messages = vec![
      message(1, 1, "The deployment failed again", None),
      message(2, 2, "Die Häuser werden gebaut", None),
      message(3, 1, "see https://example.com for the deployments", Some(9)),
    ];
    messages[1].sent_at_ms = 2_000;
    for stored in &messages {
      store.upsert_message(stored).unwrap();
    }
    let mut service = SearchService::new();

    // Stemming: "deploy" finds "deployment" and "deployments"; "Haus" finds "Häuser".
    assert_eq!(
      ids(service.search(&store, &terms(&["deploy"])).unwrap()),
      vec![3, 1]
    );
    assert_eq!(ids(service.search(&store, &terms(&["häuser"])).unwrap()), vec![2]);
    // Fuzzy: one typo still finds the word; short words must match exactly.
    assert_eq!(
      ids(service.search(&store, &terms(&["failled"])).unwrap()),
      vec![1]
    );
    assert_eq!(ids(service.search(&store, &terms(&["gebaut"])).unwrap()), vec![2]);
    assert_eq!(ids(service.search(&store, &terms(&["gebuat"])).unwrap()), vec![2]);
    assert_eq!(
      ids(service.search(&store, &terms(&["haeuser"])).unwrap()),
      vec![2]
    );
    assert!(service.search(&store, &terms(&["fai"])).unwrap().hits.is_empty());
    let link_in_thread = SearchQuery {
      has_link: true,
      in_thread: true,
      ..terms(&["deployments"])
    };
    assert_eq!(ids(service.search(&store, &link_in_thread).unwrap()), vec![3]);
    let from_two = SearchQuery {
      maybe_sender: Some(id(2)),
      ..SearchQuery::default()
    };
    assert_eq!(ids(service.search(&store, &from_two).unwrap()), vec![2]);
    // Query syntax in user input is treated as text.
    assert!(
      service
        .search(&store, &terms(&["\"OR AND:(*"]))
        .unwrap()
        .hits
        .is_empty()
    );
  }

  #[test]
  fn numbers_match_exactly() {
    let store = MemoryStore::new();
    store.put(Table::Conversations, "10", b"{}").unwrap();
    for stored in [
      message(1, 1, "Invoice over 3000 sent", None),
      message(2, 1, "Invoice over 2000 sent", None),
      message(3, 1, "Ticket 30001 closed", None),
    ] {
      store.upsert_message(&stored).unwrap();
    }
    let mut service = SearchService::new();
    assert_eq!(ids(service.search(&store, &terms(&["3000"])).unwrap()), vec![1]);
    assert_eq!(
      ids(service.search(&store, &terms(&["invoice", "2000"])).unwrap()),
      vec![2]
    );
  }

  /// #873: matches elsewhere must not use up the candidates before the filters run.
  #[test]
  fn filters_apply_before_the_candidate_cap() {
    let store = MemoryStore::new();
    let (busy, direct) = (id::<ConversationId>(10), id::<ConversationId>(11));
    for conversation in [busy, direct] {
      store
        .put(Table::Conversations, &conversation.to_string(), b"{}")
        .unwrap();
    }
    for index in 0..=MAX_CANDIDATES {
      let mut stored = message(1, 1, "deploy 3000 done", None);
      stored.client_message_id = ClientMessageId::from_bytes(numbered(index));
      store.upsert_message(&stored).unwrap();
    }
    let mut wanted = message(7, 2, "deploy 3000 done", None);
    wanted.conversation_id = direct;
    store.upsert_message(&wanted).unwrap();
    let mut service = SearchService::new();

    let everywhere = service.search(&store, &terms(&["3000"])).unwrap();
    assert_eq!(everywhere.hits.len(), MAX_CANDIDATES);
    let from_two = SearchQuery {
      maybe_sender: Some(id(2)),
      ..terms(&["3000"])
    };
    assert_eq!(ids(service.search(&store, &from_two).unwrap()), vec![7]);
    let in_direct = SearchQuery {
      conversations: ConversationScope::Only(BTreeSet::from([direct])),
      ..terms(&["3000"])
    };
    assert_eq!(ids(service.search(&store, &in_direct).unwrap()), vec![7]);
    let nowhere = SearchQuery {
      conversations: ConversationScope::Only(BTreeSet::new()),
      ..terms(&["3000"])
    };
    assert!(service.search(&store, &nowhere).unwrap().hits.is_empty());
  }

  #[test]
  fn segments_round_trip_and_only_point_to_local_messages() {
    let local = message(1, 1, "budget planning for spring", None);
    let remote = message(2, 1, "budget numbers attached", None);
    let bundle = export_segment([&local, &remote].into_iter()).unwrap();

    let store = MemoryStore::new();
    store.upsert_message(&local).unwrap();
    store.put(Table::SearchSegments, "10/2026-01", &bundle).unwrap();
    let results = SearchService::new().search(&store, &terms(&["budget"])).unwrap();
    assert_eq!(ids(results.clone()), vec![1]);
    assert_eq!(
      results
        .to_fetch
        .iter()
        .map(|m| m.client_message_id.as_bytes()[0])
        .collect::<Vec<_>>(),
      vec![2]
    );
  }

  #[test]
  fn malformed_bundles_are_rejected() {
    for bundle in [
      &[][..],
      &[0, 0, 0, 1],
      &[0, 0, 0, 1, 0, 5, b'.', b'.', b'/', b'x', b'y', 0, 0, 0, 0],
    ] {
      assert!(import_segment(bundle).is_err());
    }
  }

  #[test]
  fn months_follow_the_calendar() {
    assert_eq!(month_of(0), "1970-01");
    assert_eq!(month_of(1_767_225_600_000), "2026-01");
    assert_eq!(month_of(1_772_323_199_000), "2026-02");
    assert_eq!(month_of(1_772_323_200_000), "2026-03");
  }

  fn message(client: u8, sender: i64, body: &str, maybe_thread: Option<i64>) -> StoredMessage {
    StoredMessage {
      conversation_id: id(10),
      client_message_id: ClientMessageId::from_bytes([client; 16]),
      maybe_server_id: Some(id(100 + i64::from(client))),
      sender_user_id: id(sender),
      maybe_thread_id: maybe_thread.map(id::<ThreadId>),
      sent_at_ms: 1_000 * i64::from(client),
      content: MessageContent::Text {
        body: body.into(),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  /// A client message ID unique per `index`, starting with a byte no other test message uses.
  fn numbered(index: usize) -> [u8; 16] {
    let mut bytes = [0xEE; 16];
    bytes[8..].copy_from_slice(&(index as u64).to_be_bytes());
    bytes
  }

  fn terms(words: &[&str]) -> SearchQuery {
    SearchQuery {
      terms: words.iter().map(|word| word.to_string()).collect(),
      ..SearchQuery::default()
    }
  }

  fn ids(results: SearchResults) -> Vec<u8> {
    results
      .hits
      .iter()
      .map(|hit| hit.client_message_id.as_bytes()[0])
      .collect()
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
