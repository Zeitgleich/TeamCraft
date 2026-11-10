//! Shared search index segments (e2ee.md §10.1): publishing closed months, fetching published ones,
//! and fetching the messages a segment search found.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, Id, ListMessagesQuery, MessageId, Snowflake};

use teamcraft_crypto::secret::SecretBytes;

use super::Client;
use super::attachments::{OutgoingFile, open_blob};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::search::{SearchMatch, export_segment, is_searchable, month_of};
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageFilter, StoredMessage, Table};
use crate::transfers::ByteCounter;

const SEGMENT_MEDIA_TYPE: &str = "application/x-teamcraft-index-segment";
/// [`Table::SyncState`] key set once every conversation's announcements are backfilled.
const BACKFILL_DONE: &str = "segment_announcements/done";
/// [`Table::SyncState`] key prefix marking one conversation as backfilled.
const BACKFILL_PREFIX: &str = "segment_announcements/scanned/";

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Publishes the segment of every closed month that has no published segment yet. Returns how
  /// many were published. If two members publish the same month, readers use the first one.
  pub async fn publish_index_segments(&mut self) -> Result<usize, CoreError> {
    let current_month = month_of(self.now_ms());
    let maybe_importer = crate::imported::importer(&self.store);
    let mut published = 0;
    for conversation in self.cached_conversations()?.into_keys() {
      let messages = self.store.messages(conversation, MessageFilter::All)?;
      let announced = announced_months(&messages);
      // A month with messages this device can't read isn't complete here (issue #888): another
      // device publishes it, or this one once they are read.
      let waiting: BTreeSet<String> = messages
        .iter()
        .filter(|message| message.maybe_unreadable.is_some())
        .map(|message| month_of(message.sent_at_ms))
        .collect();
      let mut by_month: BTreeMap<String, Vec<&StoredMessage>> = BTreeMap::new();
      for message in messages
        .iter()
        .filter(|message| is_searchable(message) && message.maybe_server_id.is_some())
      {
        // Imported history goes under its original month (e2ee.md §4.10).
        let sent_at_ms = crate::imported::trusted_origin(message, maybe_importer)
          .map_or(message.sent_at_ms, |origin| origin.sent_at_ms);
        by_month.entry(month_of(sent_at_ms)).or_default().push(message);
      }
      for (month, month_messages) in by_month {
        if month >= current_month || announced.contains(&month) || waiting.contains(&month) {
          continue;
        }
        let bundle = export_segment(month_messages.iter().copied())?;
        let file = OutgoingFile {
          file_name: format!("index-{month}"),
          media_type: SEGMENT_MEDIA_TYPE.into(),
          content: bundle.clone(),
        };
        let segment = self.upload_encrypted(conversation, file).await?;
        let message_count = u32::try_from(month_messages.len()).unwrap_or(u32::MAX);
        self.enqueue(
          conversation,
          None,
          MessageContent::IndexSegment {
            month: month.clone(),
            segment: segment.clone(),
            message_count,
          },
        )?;
        let key = segment_key(conversation, &month);
        self.store.put(Table::SearchSegments, &key, &bundle)?;
        let announcement = SegmentAnnouncement {
          sent_at_ms: self.now_ms(),
          segment,
        };
        keep_earlier(&self.store, &key, &announcement)?;
        published += 1;
      }
    }
    self.flush_outbox().await?;
    Ok(published)
  }

  /// Downloads the published segments of this device's conversations that it doesn't have yet.
  pub async fn fetch_index_segments(&mut self) -> Result<usize, CoreError> {
    Ok(self.prefetch_index_segments(u64::MAX).await?.fetched)
  }

  /// Downloads missing published segments, newest months first, while the segments kept on this
  /// device stay within `max_total_bytes` (a phone prefetches on unmetered networks with a cap, so
  /// older history stays searchable offline without filling the storage). Segments that don't fit
  /// are skipped; a smaller older one may still fit. A host that keeps syncing meanwhile runs
  /// [`SegmentPlan`] beside its client instead.
  pub async fn prefetch_index_segments(
    &mut self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetch, CoreError> {
    SegmentPlan::read(&self.store)?
      .fetch(&self.api, &self.store, max_total_bytes)
      .await
  }

  /// Fetches and decrypts the messages a search found only in segments, so they can be shown.
  pub async fn fetch_search_hits(&mut self, hits: &[SearchMatch]) -> Result<usize, CoreError> {
    let mut fetched = 0;
    for hit in hits {
      let Some(server_id) = hit.maybe_server_id else {
        continue;
      };
      let Ok(previous) = Snowflake::new(server_id.snowflake().get() - 1) else {
        continue;
      };
      let query = ListMessagesQuery {
        maybe_before: None,
        maybe_after: Some(MessageId::from_snowflake(previous)),
        maybe_limit: Some(1),
      };
      for message in self
        .api
        .list_messages(hit.conversation_id, &query)
        .await?
        .messages
      {
        if message.client_message_id == hit.client_message_id {
          self.receive_message(message)?;
          fetched += 1;
        }
      }
    }
    Ok(fetched)
  }
}

/// What [`Client::prefetch_index_segments`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentPrefetch {
  pub fetched: usize,
  /// Missing segments left out because they would exceed the size cap.
  pub skipped: usize,
  /// Bytes of the segments kept on this device afterwards (as uploaded, i.e. encrypted sizes).
  pub stored_bytes: u64,
}

/// The search segments this device lacks, read from the store alone: a host runs it beside its
/// client, so a prefetch neither stops the sync nor holds up the client's work (issue #1634).
#[derive(Debug, Default)]
pub struct SegmentPlan {
  /// Bytes of the segments this device keeps (as uploaded, i.e. encrypted sizes).
  used_bytes: u64,
  /// The announced segments it lacks (the first announcement per month wins), newest month first.
  missing: Vec<MissingSegment>,
}

/// An announced segment this device doesn't have.
#[derive(Debug)]
struct MissingSegment {
  conversation: ConversationId,
  month: String,
  key: String,
  segment: AttachmentRef,
}

impl SegmentPlan {
  /// Reads the announced segments from [`Table::SegmentAnnouncements`], so the work grows with the
  /// number of segments, not of messages (issue #1636). A store written before that table existed
  /// is backfilled first, once: one conversation at a time, each recorded as done, so a run that
  /// is stopped goes on where it left off. A host gives it a read connection of its own (its few
  /// writes go through the writer), so it holds up no one.
  pub fn read(store: &impl LocalStore) -> Result<Self, CoreError> {
    let _step = crate::timing::step("segment_plan");
    backfill_announcements(store)?;
    let mut plan = Self::default();
    for (key, bytes) in store.list(Table::SegmentAnnouncements)? {
      let Some((conversation, month)) = parse_segment_key(&key) else {
        tracing::warn!("skipped a segment announcement with an unreadable key");
        continue;
      };
      let announcement: SegmentAnnouncement = match serde_json::from_slice(&bytes) {
        Ok(announcement) => announcement,
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "skipped an unreadable segment announcement");
          continue;
        },
      };
      plan.add(store, conversation, month, key, announcement.segment)?;
    }
    plan.missing.sort_by(|a, b| b.month.cmp(&a.month));
    Ok(plan)
  }

  /// How many announced segments the device lacks.
  pub fn missing_count(&self) -> usize {
    self.missing.len()
  }

  /// Downloads the missing segments, newest month first, while those kept stay within
  /// `max_total_bytes`. No store lock is held while a download runs; each segment is one write.
  pub async fn fetch(
    self,
    api: &impl ServerApi,
    store: &impl LocalStore,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetch, CoreError> {
    let mut used_bytes = self.used_bytes;
    let mut prefetch = SegmentPrefetch::default();
    for candidate in self.missing {
      let size_bytes = candidate.segment.size_bytes;
      if used_bytes.saturating_add(size_bytes) > max_total_bytes {
        prefetch.skipped += 1;
        continue;
      }
      match download_segment(api, &candidate.segment).await {
        Ok(bundle) => {
          store.put(Table::SearchSegments, &candidate.key, bundle.expose_secret())?;
          used_bytes = used_bytes.saturating_add(size_bytes);
          prefetch.fetched += 1;
        },
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(conversation_id = %candidate.conversation, %error, "skipped an index segment")
        },
      }
    }
    prefetch.stored_bytes = used_bytes;
    Ok(prefetch)
  }

  fn add(
    &mut self,
    store: &impl LocalStore,
    conversation: ConversationId,
    month: String,
    key: String,
    segment: AttachmentRef,
  ) -> Result<(), CoreError> {
    if store.get(Table::SearchSegments, &key)?.is_some() {
      self.used_bytes = self.used_bytes.saturating_add(segment.size_bytes);
    } else {
      self.missing.push(MissingSegment {
        conversation,
        month,
        key,
        segment,
      });
    }
    Ok(())
  }
}

/// The segment announced for a conversation's month, as [`Table::SegmentAnnouncements`] keeps it.
#[derive(Debug, Serialize, Deserialize)]
struct SegmentAnnouncement {
  /// When the announcement was sent: if two members published the same month, the earlier one
  /// wins, whichever this device stored first.
  sent_at_ms: i64,
  segment: AttachmentRef,
}

/// Records the segment a stored message announces, if it announces one. Called wherever an
/// announcement is stored, so [`SegmentPlan::read`] never has to look through the messages.
pub(crate) fn note_segment_announcement(
  store: &impl LocalStore,
  message: &StoredMessage,
) -> Result<(), CoreError> {
  let MessageContent::IndexSegment { month, segment, .. } = &message.content else {
    return Ok(());
  };
  let announcement = SegmentAnnouncement {
    sent_at_ms: message.sent_at_ms,
    segment: segment.clone(),
  };
  keep_earlier(store, &segment_key(message.conversation_id, month), &announcement)
}

/// Whether a stored message announces a segment (and so needs [`note_segment_announcement`]).
pub(crate) fn announces_segment(message: &StoredMessage) -> bool {
  matches!(message.content, MessageContent::IndexSegment { .. })
}

/// Fills [`Table::SegmentAnnouncements`] from the messages of a store written before it existed.
/// Runs once per device: each conversation is marked when its announcements are in, and the whole
/// backfill when every conversation is. Conversations joined later need none, since their
/// announcements are recorded as they are stored.
fn backfill_announcements(store: &impl LocalStore) -> Result<(), CoreError> {
  if store.get(Table::SyncState, BACKFILL_DONE)?.is_some() {
    return Ok(());
  }
  let _step = crate::timing::step("segment_backfill");
  for (key, _) in store.list(Table::Conversations)? {
    let Ok(conversation) = key.parse::<ConversationId>() else {
      continue;
    };
    let marker = format!("{BACKFILL_PREFIX}{conversation}");
    if store.get(Table::SyncState, &marker)?.is_some() {
      continue;
    }
    backfill_conversation(store, conversation)?;
    store.put(Table::SyncState, &marker, b"1")?;
  }
  store.put(Table::SyncState, BACKFILL_DONE, b"1")?;
  Ok(())
}

/// One conversation's part of [`backfill_announcements`]. The records come before the marker, so
/// a stop in between only means looking through this conversation again.
fn backfill_conversation(store: &impl LocalStore, conversation: ConversationId) -> Result<(), CoreError> {
  let mut found = Vec::new();
  store.visit_messages(conversation, MessageFilter::All, &mut |message| {
    if let MessageContent::IndexSegment { month, segment, .. } = &message.content {
      found.push((
        month.clone(),
        SegmentAnnouncement {
          sent_at_ms: message.sent_at_ms,
          segment: segment.clone(),
        },
      ));
    }
  })?;
  for (month, announcement) in found {
    keep_earlier(store, &segment_key(conversation, &month), &announcement)?;
  }
  Ok(())
}

/// Writes `announcement` unless the month already has an earlier one: the backfill and the
/// announcements stored meanwhile then agree, in whichever order they ran.
fn keep_earlier(
  store: &impl LocalStore,
  key: &str,
  announcement: &SegmentAnnouncement,
) -> Result<(), CoreError> {
  // An unreadable record is replaced, not an error: the record is derived from the messages.
  let maybe_known = store
    .get(Table::SegmentAnnouncements, key)?
    .and_then(|bytes| serde_json::from_slice::<SegmentAnnouncement>(&bytes).ok());
  if maybe_known.is_some_and(|known| known.sent_at_ms <= announcement.sent_at_ms) {
    return Ok(());
  }
  records::put_json(store, Table::SegmentAnnouncements, key, announcement)
}

/// Fetches a segment's blob and decrypts it; listed nowhere as a transfer (nothing the user asked
/// for).
async fn download_segment(api: &impl ServerApi, segment: &AttachmentRef) -> Result<SecretBytes, CoreError> {
  let download = api.download_attachment(segment.attachment_id).await?;
  let ciphertext = api
    .get_blob_detached(&download.download_url, None, ByteCounter::default())
    .await?;
  open_blob(segment, &ciphertext)
}

fn announced_months(messages: &[StoredMessage]) -> BTreeSet<String> {
  messages
    .iter()
    .filter_map(|message| match &message.content {
      MessageContent::IndexSegment { month, .. } => Some(month.clone()),
      _ => None,
    })
    .collect()
}

fn segment_key(conversation: ConversationId, month: &str) -> String {
  format!("{conversation}/{month}")
}

fn parse_segment_key(key: &str) -> Option<(ConversationId, String)> {
  let (conversation, month) = key.split_once('/')?;
  Some((conversation.parse().ok()?, month.to_owned()))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{AttachmentId, Base64Bytes, ClientMessageId, UserId};

  use super::*;
  use crate::store::MemoryStore;

  const CONVERSATION: i64 = 10;
  const OTHER_CONVERSATION: i64 = 11;

  mod backfill {
    use super::*;

    #[test]
    fn finds_the_announcements_of_a_store_written_before_the_table() {
      let store = store_with(&[CONVERSATION, OTHER_CONVERSATION]);
      store_unnoted(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      store_unnoted(&store, OTHER_CONVERSATION, 2, "2026-02", 200, 2_000);

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(missing_months(&plan), ["2026-02", "2026-01"]);
      assert!(store.get(Table::SyncState, BACKFILL_DONE).unwrap().is_some());
    }

    #[test]
    fn runs_once_then_reads_only_the_table() {
      let store = store_with(&[CONVERSATION]);
      store_unnoted(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      SegmentPlan::read(&store).unwrap();
      // The messages are gone and a later announcement was never recorded: a plan that still
      // looked through the messages would lose the first and find the second.
      store.delete_messages(id(CONVERSATION)).unwrap();
      store_unnoted(&store, CONVERSATION, 2, "2026-02", 200, 2_000);

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(missing_months(&plan), ["2026-01"]);
    }

    #[test]
    fn resumes_after_the_conversations_already_done() {
      let store = store_with(&[CONVERSATION, OTHER_CONVERSATION]);
      store_unnoted(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      store_unnoted(&store, OTHER_CONVERSATION, 2, "2026-02", 200, 2_000);
      // A run stopped after the first conversation was recorded as done (without its records,
      // so a second look through it would show).
      store
        .put(
          Table::SyncState,
          &format!("{BACKFILL_PREFIX}{CONVERSATION}"),
          b"1",
        )
        .unwrap();

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(missing_months(&plan), ["2026-02"]);
    }
  }

  mod plan {
    use super::*;

    #[test]
    fn lists_only_segments_the_device_lacks() {
      let store = store_with(&[CONVERSATION]);
      note(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      note(&store, CONVERSATION, 2, "2026-02", 200, 2_000);
      store
        .put(
          Table::SearchSegments,
          &segment_key(id(CONVERSATION), "2026-01"),
          b"bundle",
        )
        .unwrap();

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(missing_months(&plan), ["2026-02"]);
      assert_eq!(plan.used_bytes, 100);
    }

    #[test]
    fn is_empty_when_every_segment_is_here() {
      let store = store_with(&[CONVERSATION]);
      note(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      store
        .put(
          Table::SearchSegments,
          &segment_key(id(CONVERSATION), "2026-01"),
          b"bundle",
        )
        .unwrap();

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(plan.missing_count(), 0);
      assert_eq!(plan.used_bytes, 100);
    }

    #[test]
    fn skips_unreadable_records() {
      let store = store_with(&[CONVERSATION]);
      note(&store, CONVERSATION, 1, "2026-01", 100, 1_000);
      store
        .put(Table::SegmentAnnouncements, "not a key", b"{}")
        .unwrap();
      store
        .put(
          Table::SegmentAnnouncements,
          &segment_key(id(CONVERSATION), "2026-02"),
          b"garbage",
        )
        .unwrap();

      let plan = SegmentPlan::read(&store).unwrap();

      assert_eq!(missing_months(&plan), ["2026-01"]);
    }
  }

  mod announcements {
    use super::*;

    #[test]
    fn the_earlier_of_two_for_a_month_wins_in_either_order() {
      for later_first in [true, false] {
        let store = store_with(&[CONVERSATION]);
        let (first, second) = if later_first {
          ((2, 900), (1, 300))
        } else {
          ((1, 300), (2, 900))
        };
        note(
          &store,
          CONVERSATION,
          first.0,
          "2026-01",
          first.1,
          i64::from(first.0) * 1_000,
        );
        note(
          &store,
          CONVERSATION,
          second.0,
          "2026-01",
          second.1,
          i64::from(second.0) * 1_000,
        );

        let plan = SegmentPlan::read(&store).unwrap();

        assert_eq!(plan.missing.len(), 1);
        assert_eq!(plan.missing[0].segment.size_bytes, 300);
      }
    }

    #[test]
    fn other_messages_are_not_recorded() {
      let store = store_with(&[CONVERSATION]);
      let mut message = announcement(CONVERSATION, 1, "2026-01", 100, 1_000);
      message.content = MessageContent::Text {
        body: "hello".into(),
        mentions: Vec::new(),
        meta: Default::default(),
      };

      note_segment_announcement(&store, &message).unwrap();

      assert!(!announces_segment(&message));
      assert!(store.list(Table::SegmentAnnouncements).unwrap().is_empty());
    }

    #[test]
    fn keys_round_trip() {
      let key = segment_key(id(CONVERSATION), "2026-01");
      assert_eq!(
        parse_segment_key(&key),
        Some((id(CONVERSATION), "2026-01".to_owned()))
      );
      assert_eq!(parse_segment_key("2026-01"), None);
      assert_eq!(parse_segment_key("x/2026-01"), None);
    }
  }

  /// A store holding these conversations.
  fn store_with(conversations: &[i64]) -> MemoryStore {
    let store = MemoryStore::new();
    for conversation in conversations {
      store
        .put(Table::Conversations, &conversation.to_string(), b"{}")
        .unwrap();
    }
    store
  }

  /// An announcement stored the way the receive path does it.
  fn note(store: &MemoryStore, conversation: i64, n: u8, month: &str, size_bytes: u64, at_ms: i64) {
    let message = announcement(conversation, n, month, size_bytes, at_ms);
    store.upsert_message(&message).unwrap();
    note_segment_announcement(store, &message).unwrap();
  }

  /// An announcement stored by a version without the table.
  fn store_unnoted(store: &MemoryStore, conversation: i64, n: u8, month: &str, size_bytes: u64, at_ms: i64) {
    store
      .upsert_message(&announcement(conversation, n, month, size_bytes, at_ms))
      .unwrap();
  }

  fn announcement(conversation: i64, n: u8, month: &str, size_bytes: u64, at_ms: i64) -> StoredMessage {
    StoredMessage {
      conversation_id: id(conversation),
      client_message_id: ClientMessageId::from_bytes([n; 16]),
      maybe_server_id: Some(id(1_000 + i64::from(n))),
      sender_user_id: id::<UserId>(1),
      maybe_thread_id: None,
      sent_at_ms: at_ms,
      content: MessageContent::IndexSegment {
        month: month.into(),
        segment: AttachmentRef {
          attachment_id: id::<AttachmentId>(500 + i64::from(n)),
          key: Base64Bytes::new(Vec::new()),
          ciphertext_sha256: Base64Bytes::new(Vec::new()),
          file_name: format!("index-{month}"),
          media_type: SEGMENT_MEDIA_TYPE.into(),
          size_bytes,
          maybe_thumbnail: None,
          maybe_dimensions: None,
          maybe_duration_ms: None,
          maybe_waveform: None,
        },
        message_count: 1,
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

  fn missing_months(plan: &SegmentPlan) -> Vec<&str> {
    plan
      .missing
      .iter()
      .map(|segment| segment.month.as_str())
      .collect()
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
