//! Shared search index segments (e2ee.md §10.1): publishing closed months, fetching published ones,
//! and fetching the messages a segment search found.

use std::collections::{BTreeMap, BTreeSet};

use teamcraft_proto::{ConversationId, Id, ListMessagesQuery, MessageId, Snowflake};

use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::ConversationResponse;

use super::Client;
use super::attachments::{OutgoingFile, open_blob};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::search::{SearchMatch, export_segment, is_searchable, month_of};
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageFilter, StoredMessage, Table};
use crate::transfers::ByteCounter;

const SEGMENT_MEDIA_TYPE: &str = "application/x-teamcraft-index-segment";

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
            segment,
            message_count,
          },
        )?;
        self
          .store
          .put(Table::SearchSegments, &segment_key(conversation, &month), &bundle)?;
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
  /// Looks through the announcements of every conversation the device has, one conversation at a
  /// time. Reads only: a host gives it a read connection of its own, so it holds up no one.
  pub fn read(store: &impl LocalStore) -> Result<Self, CoreError> {
    let _step = crate::timing::step("segment_plan");
    let mut plan = Self::default();
    let conversations: Vec<ConversationResponse> = records::list_json(store, Table::Conversations)?;
    for conversation in conversations {
      plan.read_conversation(store, conversation.id)?;
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

  fn read_conversation(
    &mut self,
    store: &impl LocalStore,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    let mut first_per_month = BTreeMap::new();
    store.visit_messages(conversation, MessageFilter::All, &mut |message| {
      if let MessageContent::IndexSegment { month, segment, .. } = &message.content {
        first_per_month
          .entry(month.clone())
          .or_insert_with(|| segment.clone());
      }
    })?;
    for (month, segment) in first_per_month {
      let key = segment_key(conversation, &month);
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
    }
    Ok(())
  }
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
