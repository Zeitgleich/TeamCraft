//! Shared search index segments (e2ee.md §10.1): publishing closed months, fetching published ones,
//! and fetching the messages a segment search found.

use std::collections::{BTreeMap, BTreeSet};

use teamcraft_proto::{ConversationId, Id, ListMessagesQuery, MessageId, Snowflake};

use super::Client;
use super::attachments::OutgoingFile;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::search::{SearchMatch, export_segment, is_searchable, month_of};
use crate::store::{AttachmentRef, LocalStore, MessageContent, MessageFilter, StoredMessage, Table};

const SEGMENT_MEDIA_TYPE: &str = "application/x-teamcraft-index-segment";

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Publishes the segment of every closed month that has no published segment yet. Returns how
  /// many were published. If two members publish the same month, readers use the first one.
  pub async fn publish_index_segments(&mut self) -> Result<usize, CoreError> {
    let current_month = month_of(self.now_ms());
    let mut published = 0;
    for conversation in self.cached_conversations()?.into_keys() {
      let messages = self.store.messages(conversation, MessageFilter::All)?;
      let announced = announced_months(&messages);
      let mut by_month: BTreeMap<String, Vec<&StoredMessage>> = BTreeMap::new();
      for message in messages
        .iter()
        .filter(|message| is_searchable(message) && message.maybe_server_id.is_some())
      {
        by_month
          .entry(month_of(message.sent_at_ms))
          .or_default()
          .push(message);
      }
      for (month, month_messages) in by_month {
        if month >= current_month || announced.contains(&month) {
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
  /// are skipped; a smaller older one may still fit.
  pub async fn prefetch_index_segments(
    &mut self,
    max_total_bytes: u64,
  ) -> Result<SegmentPrefetchReport, CoreError> {
    let (mut used_bytes, missing) = self.segment_inventory()?;
    let mut prefetch = SegmentPrefetchReport::default();
    for candidate in missing {
      let size_bytes = candidate.segment.size_bytes;
      if used_bytes.saturating_add(size_bytes) > max_total_bytes {
        prefetch.skipped += 1;
        continue;
      }
      match self.download_decrypted(&candidate.segment).await {
        Ok(bundle) => {
          self
            .store
            .put(Table::SearchSegments, &candidate.key, bundle.expose_secret())?;
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

  /// The bytes of the segments this device keeps, and the announced segments it lacks (the first
  /// announcement per month wins), newest month first.
  fn segment_inventory(&self) -> Result<(u64, Vec<MissingSegment>), CoreError> {
    let mut used_bytes = 0u64;
    let mut missing = Vec::new();
    for conversation in self.cached_conversations()?.into_keys() {
      let mut first_per_month = BTreeMap::new();
      for message in self.store.messages(conversation, MessageFilter::All)? {
        if let MessageContent::IndexSegment { month, segment, .. } = message.content {
          first_per_month.entry(month).or_insert(segment);
        }
      }
      for (month, segment) in first_per_month {
        let key = segment_key(conversation, &month);
        if self.store.get(Table::SearchSegments, &key)?.is_some() {
          used_bytes = used_bytes.saturating_add(segment.size_bytes);
        } else {
          missing.push(MissingSegment {
            conversation,
            month,
            key,
            segment,
          });
        }
      }
    }
    missing.sort_by(|a, b| b.month.cmp(&a.month));
    Ok((used_bytes, missing))
  }
}

/// What [`Client::prefetch_index_segments`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentPrefetchReport {
  pub fetched: usize,
  /// Missing segments left out because they would exceed the size cap.
  pub skipped: usize,
  /// Bytes of the segments kept on this device afterwards (as uploaded, i.e. encrypted sizes).
  pub stored_bytes: u64,
}

/// An announced segment this device doesn't have.
struct MissingSegment {
  conversation: ConversationId,
  month: String,
  key: String,
  segment: AttachmentRef,
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
