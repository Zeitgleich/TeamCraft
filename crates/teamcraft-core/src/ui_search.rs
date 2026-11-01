//! [`SearchBackend`] of `teamcraft-ui` over the local store and shared segments (feature `ui`).
//!
//! Terms are matched by the tantivy index ([`crate::search`], with English and German word forms);
//! the UI's filters (`from:`, `in:`, `has:`, dates, `is:thread`) are applied to the decrypted message
//! with the same rules the UI's own search uses. In the browser, where tantivy doesn't run, terms are
//! matched as plain substrings instead.

#[cfg(not(target_arch = "wasm32"))]
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::backend::SearchBackend;
use teamcraft_client_api::model::{Conversation, ConversationId, Message, SpaceId};
use teamcraft_client_api::search::{
  Filter, ParsedQuery, SavedSearch, SearchHit, SearchResults, find_ci, has_file, has_link, is_in_space,
  make_snippet,
};

use crate::client::stored_account_data;
use crate::error::CoreError;
use crate::records;
use crate::search_scope::ConversationScope;
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand, UnreadCounts};

/// Hits shown per search.
const MAX_HITS: usize = 50;

#[derive(Serialize, Deserialize)]
struct SavedSearchRecord {
  id: u64,
  name: String,
  query: String,
}

/// What hits of one search share, read once per search instead of once per hit.
struct HitContext {
  conversations: HashMap<ConversationId, Conversation>,
  space_names: HashMap<SpaceId, String>,
  messages: HashMap<ConversationId, Vec<StoredMessage>>,
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
    let mut context = self.hit_context();
    let scope = conversation_scope(query, &context);
    let candidates = self.term_candidates(query, &scope);
    let mut hits: Vec<SearchHit> = candidates
      .iter()
      .filter_map(|stored| self.to_hit(stored, query, &mut context))
      .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.message.sent_at));
    let total = hits.len();
    hits.truncate(MAX_HITS);
    SearchResults {
      hits,
      total,
      is_pending: false,
    }
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
    let mut context = self.hit_context();
    mentions
      .iter()
      .filter_map(|stored| self.to_hit(stored, &empty, &mut context))
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
  /// Messages of the conversations in `scope` whose text matches all terms.
  #[cfg(not(target_arch = "wasm32"))]
  fn term_candidates(&self, query: &ParsedQuery, scope: &ConversationScope) -> Vec<StoredMessage> {
    let core_query = crate::search::SearchQuery {
      terms: query.terms().iter().map(|term| term.to_string()).collect(),
      conversations: scope.clone(),
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
  fn term_candidates(&self, query: &ParsedQuery, scope: &ConversationScope) -> Vec<StoredMessage> {
    let terms = query.terms();
    self
      .conversation_ids()
      .into_iter()
      .filter(|conversation| scope.includes(*conversation))
      .flat_map(|conversation| self.stored(conversation))
      .filter(|stored| {
        terms
          .iter()
          .all(|term| find_ci(&searchable_text(stored), term).is_some())
      })
      .collect()
  }

  /// Conversations and space names as the sidebar shows them, read once for a whole search. No
  /// unread counts: search doesn't show them, and counting reads every message.
  fn hit_context(&self) -> HitContext {
    let users = self.summaries();
    HitContext {
      conversations: self
        .conversations_raw()
        .iter()
        .map(|response| {
          let conversation = self.conversation_view(response, &users, UnreadCounts::default());
          (conversation.id, conversation)
        })
        .collect(),
      space_names: ChatBackend::spaces(self)
        .into_iter()
        .map(|space| (space.id, space.name))
        .collect(),
      messages: HashMap::new(),
    }
  }

  fn to_hit(
    &self,
    stored: &StoredMessage,
    query: &ParsedQuery,
    context: &mut HitContext,
  ) -> Option<SearchHit> {
    if !matches!(
      stored.content,
      MessageContent::Text { .. } | MessageContent::File { .. }
    ) {
      return None;
    }
    let conversation = context.conversations.get(&stored.conversation_id)?.clone();
    let all = context
      .messages
      .entry(stored.conversation_id)
      .or_insert_with(|| self.stored(stored.conversation_id));
    let message: Message = self.message_view(stored, all);
    // The author the hit shows: the original one of an imported message.
    let author = ChatBackend::user(self, message.author);
    let (username, display_name) = author
      .map(|user| (user.username, user.display_name))
      .unwrap_or_default();
    let maybe_space_name = conversation
      .maybe_space
      .and_then(|space| context.space_names.get(&space).cloned());
    let passes = query.filters().iter().all(|filter| match filter {
      Filter::From(name) => username.eq_ignore_ascii_case(name) || find_ci(&display_name, name).is_some(),
      Filter::In(name) => is_named(&conversation, name),
      Filter::InSpace(name) => is_in_space(maybe_space_name.as_deref().unwrap_or_default(), name),
      Filter::HasFile => matches!(stored.content, MessageContent::File { .. }) || has_file(&message.body),
      Filter::HasLink => has_link(&message.body),
      Filter::Before(limit) => message.sent_at < *limit,
      Filter::After(limit) => message.sent_at >= *limit,
      Filter::InThread => message.maybe_thread.is_some(),
    });
    if !passes {
      return None;
    }
    Some(SearchHit {
      snippet: make_snippet(&message.body, &query.terms()),
      message,
      conversation_name: conversation.name,
      maybe_space_name,
    })
  }

  pub(crate) fn conversation_ids(&self) -> Vec<ConversationId> {
    self
      .store_ref()
      .list(Table::Conversations)
      .unwrap_or_default()
      .into_iter()
      .filter_map(|(key, _)| serde_json::from_value(serde_json::Value::String(key)).ok())
      .collect()
  }

  pub(crate) fn stored(&self, conversation: ConversationId) -> Vec<StoredMessage> {
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

/// The conversations the query's `in:` and `space:` filters leave, so the term search only looks
/// there: matches elsewhere would otherwise fill its candidates and leave none here (issue #873).
fn conversation_scope(query: &ParsedQuery, context: &HitContext) -> ConversationScope {
  let filters = query.filters();
  let is_scoped = filters
    .iter()
    .any(|filter| matches!(filter, Filter::In(_) | Filter::InSpace(_)));
  if !is_scoped {
    return ConversationScope::All;
  }
  let conversations: BTreeSet<ConversationId> = context
    .conversations
    .values()
    .filter(|conversation| {
      filters.iter().all(|filter| match filter {
        Filter::In(name) => is_named(conversation, name),
        Filter::InSpace(name) => conversation
          .maybe_space
          .and_then(|space| context.space_names.get(&space))
          .is_some_and(|space_name| is_in_space(space_name, name)),
        _ => true,
      })
    })
    .map(|conversation| conversation.id)
    .collect();
  ConversationScope::Only(conversations)
}

/// `in:name` matches a conversation whose name contains `name`, ignoring case.
fn is_named(conversation: &Conversation, name: &str) -> bool {
  find_ci(&conversation.name, name).is_some()
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
    HistoryPolicy, Id, MessageId, Snowflake, SpaceResponse, UserKind, UserSummary,
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
  fn search_is_scoped_to_a_space() {
    let backend = backend();
    assert_eq!(
      backend.search(&parse_query(r#"budget space:"Project A""#)).total,
      3
    );
    let scoped = backend.search(&parse_query("budget space:project"));
    assert_eq!(scoped.hits[0].maybe_space_name.as_deref(), Some("Project A"));
    assert_eq!(
      backend.search(&parse_query(r#"budget space:"Project B""#)).total,
      0
    );
  }

  /// #873: `in:"@Name"` with a word finds the message in that direct message, and the channel's
  /// messages don't crowd it out.
  #[test]
  fn a_direct_message_filter_with_a_word_finds_the_message() {
    let backend = backend();
    assert_eq!(backend.search(&parse_query("3000")).total, 2);

    for raw in [r#"in:"@Bob" 3000"#, "in:@bob 3000", r#"3000 in:@"bob""#] {
      let results = backend.search(&parse_query(raw));
      assert_eq!(results.total, 1, "{raw}");
      assert_eq!(results.hits[0].message.body, "Order 3000 shipped", "{raw}");
      assert_eq!(results.hits[0].conversation_name, "bob", "{raw}");
    }
    let in_channel = backend.search(&parse_query("in:#finance 3000"));
    assert_eq!(in_channel.total, 1);
    assert_eq!(in_channel.hits[0].conversation_name, "finance");
    assert_eq!(backend.search(&parse_query("in:@bob budget")).total, 0);
    assert_eq!(backend.search(&parse_query("in:@carol 3000")).total, 0);
    assert_eq!(backend.search(&parse_query(r#"in:"@Bob""#)).total, 1);
    // Direct messages are in no space.
    assert_eq!(backend.search(&parse_query(r#"3000 space:"Project A""#)).total, 1);
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
    let mut summaries = Vec::new();
    for (user, name) in [(alice, "alice"), (bob, "bob")] {
      let summary = UserSummary {
        id: user,
        maybe_username: Some(name.into()),
        display_name: name.into(),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_deactivated: false,
        is_deleted: false,
      };
      records::put_json(&store, Table::Users, &user.to_string(), &summary).unwrap();
      summaries.push(summary);
    }
    let space = id::<teamcraft_proto::SpaceId>(5);
    let project = SpaceResponse {
      id: space,
      name: "Project A".into(),
      maybe_icon: None,
      maybe_color: None,
      maybe_description: None,
      maybe_image: None,
      join_policy: teamcraft_proto::JoinPolicy::Open,
      is_archived: false,
      maybe_brand_id: None,
      maybe_my_role: Some(teamcraft_proto::SpaceRole::Member),
      maybe_my_notifications: None,
      member_count: 1,
      can_add_members: false,
      can_invite_guests: false,
      can_manage_space: false,
      can_create_channels: false,
    };
    records::put_json(&store, Table::Spaces, &space.to_string(), &project).unwrap();
    let conversation = ConversationResponse {
      id: channel,
      maybe_space_id: Some(space),
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
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_importer_user_id: None,
      maybe_imported_through: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: None,
    };
    records::put_json(&store, Table::Conversations, &channel.to_string(), &conversation).unwrap();
    // A DM has no name of its own: it is named after the other person.
    let direct = id::<ConversationId>(11);
    let dm = ConversationResponse {
      id: direct,
      maybe_space_id: None,
      kind: ConversationKind::Dm,
      maybe_name: None,
      participants: summaries,
      ..conversation
    };
    records::put_json(&store, Table::Conversations, &direct.to_string(), &dm).unwrap();
    for (conversation_id, client, sender, body, mentions) in [
      (channel, 1u8, alice, "The budget draft is attached", vec![]),
      (channel, 2, bob, "Budget review moved", vec![]),
      (channel, 3, bob, "@alice the budget is final", vec![alice]),
      (channel, 4, alice, "Lunch?", vec![]),
      (
        channel,
        5,
        alice,
        "Quarter closed at 3000 units, not 2000",
        vec![],
      ),
      (direct, 6, bob, "Order 3000 shipped", vec![]),
    ] {
      store
        .upsert_message(&StoredMessage {
          conversation_id,
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
          maybe_edit_id: None,
          is_pinned: false,
          maybe_verified_commit: None,
          maybe_server_change: None,
          maybe_unreadable: None,
        })
        .unwrap();
    }
    CoreChatBackend::new(store, alice, Box::new(ManualClock::new(0)))
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
