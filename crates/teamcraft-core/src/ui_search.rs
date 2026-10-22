//! [`SearchBackend`] of `teamcraft-ui` over the local store and shared segments (feature `ui`).
//!
//! Terms are matched by the tantivy index ([`crate::search`], with English and German word forms);
//! the UI's filters (`from:`, `in:`, `has:`, dates, `is:thread`) are applied to the decrypted message
//! with the same rules the UI's own search uses. In the browser, where tantivy doesn't run, terms are
//! matched as plain substrings instead.

#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use teamcraft_client_api::backend::SearchBackend;
use teamcraft_client_api::model::{ConversationId, Message};
use teamcraft_client_api::search::{
  Filter, ParsedQuery, SavedSearch, SearchHit, SearchResults, find_ci, has_file, has_link, make_snippet,
};

use crate::client::stored_account_data;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};

/// Hits shown per search.
const MAX_HITS: usize = 50;

#[derive(Serialize, Deserialize)]
struct SavedSearchRecord {
  id: u64,
  name: String,
  query: String,
}

/// Search state kept between calls: imported segments stay open.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub(crate) struct UiSearchState {
  service: crate::search::SearchService,
}

impl<S: LocalStore> SearchBackend for CoreChatBackend<S> {
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    if query.is_empty() {
      return SearchResults::default();
    }
    let candidates = self.term_candidates(query);
    let mut hits: Vec<SearchHit> = candidates
      .iter()
      .filter_map(|stored| self.to_hit(stored, query))
      .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.message.sent_at));
    let total = hits.len();
    hits.truncate(MAX_HITS);
    SearchResults { hits, total }
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    let mut mentions: Vec<StoredMessage> = self
      .conversation_ids()
      .into_iter()
      .flat_map(|conversation| self.stored(conversation))
      .filter(|stored| matches!(&stored.content, MessageContent::Text { mentions, .. } if mentions.contains(&self.me_id())))
      .collect();
    mentions.sort_by_key(|message| std::cmp::Reverse(message.sent_at_ms));
    mentions.truncate(MAX_HITS);
    let empty = teamcraft_client_api::search::parse_query("");
    mentions
      .iter()
      .filter_map(|stored| self.to_hit(stored, &empty))
      .collect()
  }

  /// Saved searches live in the synced account data (e2ee.md §6.4); ones saved on this device
  /// before that are still listed.
  fn saved_searches(&self) -> Vec<SavedSearch> {
    let mut searches: Vec<SavedSearch> = self
      .read(stored_account_data(self.store_ref()))
      .saved_searches()
      .into_iter()
      .map(|(id, entry)| SavedSearch {
        id,
        name: entry.name.clone(),
        query: entry.query.clone(),
      })
      .collect();
    let legacy: Vec<SavedSearchRecord> =
      records::list_json(self.store_ref(), Table::SavedSearches).unwrap_or_default();
    for record in legacy {
      if !searches.iter().any(|search| search.id == record.id) {
        searches.push(SavedSearch {
          id: record.id,
          name: record.name,
          query: record.query,
        });
      }
    }
    searches.sort_by(|left, right| left.name.cmp(&right.name));
    searches
  }

  fn save_search(&self, name: &str, query: &str) {
    self.command(CoreCommand::SaveSearch {
      // Random, so two devices saving at the same time don't take the same ID.
      id: rand_core::RngCore::next_u64(&mut rand_core::OsRng) >> 11,
      name: name.to_string(),
      query: query.to_string(),
    });
  }

  fn delete_saved_search(&self, id: u64) {
    if let Err(error) = self
      .store_ref()
      .delete(Table::SavedSearches, &format!("{id:020}"))
    {
      tracing::error!(%error, "could not delete the saved search");
    }
    self.command(CoreCommand::DeleteSavedSearch { id });
  }
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// Messages whose text matches all terms.
  #[cfg(not(target_arch = "wasm32"))]
  fn term_candidates(&self, query: &ParsedQuery) -> Vec<StoredMessage> {
    let core_query = crate::search::SearchQuery {
      terms: query.terms().iter().map(|term| term.to_string()).collect(),
      ..Default::default()
    };
    let results = match self
      .search_state()
      .borrow_mut()
      .service
      .search(self.store_ref(), &core_query)
    {
      Ok(results) => results,
      Err(error) => {
        tracing::error!(%error, "search failed");
        return Vec::new();
      },
    };
    if !results.to_fetch.is_empty() {
      // Hits from shared segments whose messages aren't here yet: the sync task fetches them and
      // the next search shows them.
      self.queue_command(CoreCommand::FetchSearchHits {
        hits: results.to_fetch,
      });
    }
    results
      .hits
      .iter()
      .filter_map(|hit| {
        self
          .store_ref()
          .message(hit.conversation_id, hit.client_message_id)
          .ok()
          .flatten()
      })
      .collect()
  }

  /// Browser: plain substring matching over the local store.
  #[cfg(target_arch = "wasm32")]
  fn term_candidates(&self, query: &ParsedQuery) -> Vec<StoredMessage> {
    let terms = query.terms();
    self
      .conversation_ids()
      .into_iter()
      .flat_map(|conversation| self.stored(conversation))
      .filter(|stored| {
        terms
          .iter()
          .all(|term| find_ci(&searchable_text(stored), term).is_some())
      })
      .collect()
  }

  fn to_hit(&self, stored: &StoredMessage, query: &ParsedQuery) -> Option<SearchHit> {
    if !matches!(
      stored.content,
      MessageContent::Text { .. } | MessageContent::File { .. }
    ) {
      return None;
    }
    let conversation =
      teamcraft_client_api::backend::ChatBackend::conversation(self, stored.conversation_id)?;
    let all = self.stored(stored.conversation_id);
    let message: Message = self.message_view(stored, &all);
    let author = teamcraft_client_api::backend::ChatBackend::user(self, stored.sender_user_id);
    let (username, display_name) = author
      .map(|user| (user.username, user.display_name))
      .unwrap_or_default();
    let passes = query.filters().iter().all(|filter| match filter {
      Filter::From(name) => username.eq_ignore_ascii_case(name) || find_ci(&display_name, name).is_some(),
      Filter::In(name) => find_ci(&conversation.name, name).is_some(),
      Filter::HasFile => matches!(stored.content, MessageContent::File { .. }) || has_file(&message.body),
      Filter::HasLink => has_link(&message.body),
      Filter::Before(limit) => message.sent_at < *limit,
      Filter::After(limit) => message.sent_at >= *limit,
      Filter::InThread => message.maybe_thread.is_some(),
    });
    if !passes {
      return None;
    }
    let maybe_space_name = conversation.maybe_space.and_then(|space| {
      teamcraft_client_api::backend::ChatBackend::spaces(self)
        .into_iter()
        .find(|candidate| candidate.id == space)
        .map(|found| found.name)
    });
    Some(SearchHit {
      snippet: make_snippet(&message.body, &query.terms()),
      message,
      conversation_name: conversation.name,
      maybe_space_name,
    })
  }

  fn conversation_ids(&self) -> Vec<ConversationId> {
    self
      .store_ref()
      .list(Table::Conversations)
      .unwrap_or_default()
      .into_iter()
      .filter_map(|(key, _)| serde_json::from_value(serde_json::Value::String(key)).ok())
      .collect()
  }

  fn stored(&self, conversation: ConversationId) -> Vec<StoredMessage> {
    self
      .store_ref()
      .messages(conversation, MessageFilter::All)
      .map_err(CoreError::from)
      .unwrap_or_default()
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn search_state(&self) -> &RefCell<UiSearchState> {
    &self.search
  }
}

#[cfg(target_arch = "wasm32")]
fn searchable_text(stored: &StoredMessage) -> String {
  match &stored.content {
    MessageContent::Text { body, .. } => body.clone(),
    MessageContent::File { attachment } => attachment.file_name.clone(),
    _ => String::new(),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::search::parse_query;
  use teamcraft_proto::{
    ChannelModeration, ClientMessageId, ConversationKind, ConversationResponse, ConversationVisibility,
    HistoryPolicy, Id, MessageId, Snowflake, UserKind, UserSummary,
  };

  use super::*;
  use crate::clock::ManualClock;
  use crate::store::MemoryStore;

  #[test]
  fn search_matches_word_forms_and_applies_ui_filters() {
    let backend = backend();
    let results = backend.search(&parse_query("budgets"));
    assert_eq!(results.total, 3);
    assert_eq!(results.hits[0].message.body, "Budget review moved");
    assert_eq!(results.hits[0].conversation_name, "finance");

    let from_bob = backend.search(&parse_query("budget from:bob"));
    assert_eq!(from_bob.total, 2);
    assert_eq!(from_bob.hits[0].message.body, "Budget review moved");
    assert_eq!(backend.search(&parse_query("budget in:nowhere")).total, 0);
    assert_eq!(backend.search(&parse_query("")).total, 0);
  }

  #[test]
  fn mentions_and_saved_searches() {
    let backend = backend();
    let mentions = backend.recent_mentions();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].message.body, "@alice the budget is final");

    // Saved searches are account data: the engine applies them (tests/account_data.rs).
    backend.save_search("Budget", "budget in:finance");
    backend.delete_saved_search(42);
    let commands = backend.drain_commands();
    assert!(matches!(&commands[0], CoreCommand::SaveSearch { name, query, .. }
      if name == "Budget" && query == "budget in:finance"));
    assert_eq!(commands[1], CoreCommand::DeleteSavedSearch { id: 42 });
  }

  fn backend() -> CoreChatBackend<MemoryStore> {
    let store = MemoryStore::new();
    let (alice, bob, channel) = (
      id::<teamcraft_client_api::model::UserId>(1),
      id::<teamcraft_client_api::model::UserId>(2),
      id::<ConversationId>(10),
    );
    for (user, name) in [(alice, "alice"), (bob, "bob")] {
      let summary = UserSummary {
        id: user,
        maybe_username: Some(name.into()),
        display_name: name.into(),
        kind: UserKind::Member,
        maybe_email: None,
      };
      records::put_json(&store, Table::Users, &user.to_string(), &summary).unwrap();
    }
    let conversation = ConversationResponse {
      id: channel,
      maybe_space_id: None,
      kind: ConversationKind::Channel,
      maybe_name: Some("finance".into()),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy: HistoryPolicy::Full,
      maybe_category_id: None,
      is_archived: false,
      moderation: ChannelModeration::default(),
      is_integration: false,
      contains_guests: false,
      maybe_my_membership: None,
      participants: Vec::new(),
    };
    records::put_json(&store, Table::Conversations, &channel.to_string(), &conversation).unwrap();
    for (client, sender, body, mentions) in [
      (1u8, alice, "The budget draft is attached", vec![]),
      (2, bob, "Budget review moved", vec![]),
      (3, bob, "@alice the budget is final", vec![alice]),
      (4, alice, "Lunch?", vec![]),
    ] {
      store
        .upsert_message(&StoredMessage {
          conversation_id: channel,
          client_message_id: ClientMessageId::from_bytes([client; 16]),
          maybe_server_id: Some(id::<MessageId>(100 + i64::from(client))),
          sender_user_id: sender,
          maybe_thread_id: None,
          sent_at_ms: 1_000 * i64::from(client) + if client == 2 { 10_000 } else { 0 },
          content: MessageContent::Text {
            body: body.into(),
            mentions,
            meta: Default::default(),
          },
          is_edited: false,
          edits: Vec::new(),
          is_pinned: false,
        })
        .unwrap();
    }
    CoreChatBackend::new(store, alice, Box::new(ManualClock::new(0)))
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
