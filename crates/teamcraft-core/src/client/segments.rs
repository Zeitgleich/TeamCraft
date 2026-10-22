//! Shared search index segments (e2ee.md §10.1): publishing closed months, fetching published ones,
//! and fetching the messages a segment search found.

use std::collections::{BTreeMap, BTreeSet};

use teamcraft_proto::{ConversationId, Id, ListMessagesQuery, MessageId, Snowflake};

use super::Client;
use super::attachments::OutgoingFile;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::search::{SearchMatch, export_segment, is_searchable, month_of};
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};

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
    let mut fetched = 0;
    for conversation in self.cached_conversations()?.into_keys() {
      // Oldest announcement per month wins.
      let mut first_per_month = BTreeMap::new();
      for message in self.store.messages(conversation, MessageFilter::All)? {
        if let MessageContent::IndexSegment { month, segment, .. } = message.content {
          first_per_month.entry(month).or_insert(segment);
        }
      }
      for (month, segment) in first_per_month {
        let key = segment_key(conversation, &month);
        if self.store.get(Table::SearchSegments, &key)?.is_some() {
          continue;
        }
        match self.download_decrypted(&segment).await {
          Ok(bundle) => {
            self
              .store
              .put(Table::SearchSegments, &key, bundle.expose_secret())?;
            fetched += 1;
          },
          Err(error) if error.is_transient() => return Err(error),
          Err(error) => tracing::warn!(conversation_id = %conversation, %error, "skipped an index segment"),
        }
      }
    }
    Ok(fetched)
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
