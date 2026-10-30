//! Mock implementation of [`SearchBackend`] over the demo messages. Child module of `mock`.

use super::MockBackend;
use crate::backend::SearchBackend;
use crate::model::{Message, MessageKind};
use crate::search::{
  MatchContext, ParsedQuery, SavedSearch, SearchHit, SearchResults, make_snippet, matches,
};

/// Hits returned per search; the rest is only counted.
const MAX_HITS: usize = 100;

pub(super) struct SearchState {
  saved: Vec<SavedSearch>,
  next_id: u64,
}

impl SearchState {
  pub(super) fn new() -> Self {
    Self {
      saved: vec![
        SavedSearch {
          id: 1,
          name: "Files from Sam".into(),
          query: "from:sam has:file".into(),
        },
        SavedSearch {
          id: 2,
          name: "Links in engineering".into(),
          query: "in:engineering has:link".into(),
        },
      ],
      next_id: 3,
    }
  }
}

impl MockBackend {
  /// All user messages and replies that match, newest first.
  fn search_messages(&self, query: &ParsedQuery, limit: usize) -> SearchResults {
    let state = self.state.borrow();
    let mut hits: Vec<(Message, usize)> = Vec::new();
    for (conversation, messages) in &state.messages {
      let Some(info) = state.conversations.iter().find(|conv| conv.id == *conversation) else {
        continue;
      };
      // Integration channels (logs) are generated on demand and not part of the demo index.
      if info.kind == crate::model::ConversationKind::Integration {
        continue;
      }
      let replies = state
        .replies
        .get(conversation)
        .map(Vec::as_slice)
        .unwrap_or_default();
      for message in messages.iter().chain(replies) {
        if !matches!(message.kind, MessageKind::User) {
          continue;
        }
        let Some(author) = state.users.iter().find(|user| user.id == message.author) else {
          continue;
        };
        let context = MatchContext {
          author_username: &author.username,
          author_name: &author.display_name,
          conversation_name: &info.name,
          space_name: info
            .maybe_space
            .and_then(|space| state.spaces.iter().find(|candidate| candidate.id == space))
            .map_or("", |space| space.name.as_str()),
        };
        if matches(query, message, &context) {
          hits.push((message.clone(), 0));
        }
      }
    }
    hits.sort_by_key(|(message, _)| std::cmp::Reverse(message.sent_at));
    let total = hits.len();
    let terms = query.terms();
    let hits = hits
      .into_iter()
      .take(limit)
      .filter_map(|(message, _)| {
        let info = state
          .conversations
          .iter()
          .find(|conv| conv.id == message.conversation)?;
        let maybe_space_name = info
          .maybe_space
          .and_then(|space| state.spaces.iter().find(|candidate| candidate.id == space))
          .map(|space| space.name.clone());
        Some(SearchHit {
          snippet: make_snippet(&message.body, &terms),
          conversation_name: info.name.clone(),
          maybe_space_name,
          message,
        })
      })
      .collect();
    SearchResults {
      hits,
      total,
      is_pending: false,
    }
  }
}

impl SearchBackend for MockBackend {
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    if query.is_empty() {
      return SearchResults::default();
    }
    self.search_messages(query, MAX_HITS)
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    let username = self.state.borrow().users[0].username.clone();
    let query = crate::search::parse_query(&format!("@{username}"));
    self.search_messages(&query, 30).hits
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    self.search.borrow().saved.clone()
  }

  fn save_search(&self, name: &str, query: &str) {
    let (name, query) = (name.trim(), query.trim());
    if name.is_empty() || query.is_empty() {
      return;
    }
    let mut state = self.search.borrow_mut();
    if state.saved.iter().any(|saved| saved.query == query) {
      return;
    }
    let id = state.next_id;
    state.next_id += 1;
    state.saved.push(SavedSearch {
      id,
      name: name.to_owned(),
      query: query.to_owned(),
    });
  }

  fn delete_saved_search(&self, id: u64) {
    self.search.borrow_mut().saved.retain(|saved| saved.id != id);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::search::parse_query;

  const NOW: i64 = 1_792_584_000_000;

  #[test]
  fn filters_narrow_the_demo_data() {
    let backend = MockBackend::new(NOW);
    let all = backend.search(&parse_query("deploy"));
    assert!(all.total > 0);
    assert!(
      all
        .hits
        .windows(2)
        .all(|pair| pair[0].message.sent_at >= pair[1].message.sent_at)
    );
    let in_design = backend.search(&parse_query("deploy in:design"));
    assert!(in_design.total < all.total);
    assert!(in_design.hits.iter().all(|hit| hit.conversation_name == "design"));
    assert!(backend.search(&parse_query("zzzznothing")).hits.is_empty());
    assert!(backend.search(&parse_query("")).hits.is_empty());
  }

  #[test]
  fn highlights_come_with_hits() {
    let backend = MockBackend::new(NOW);
    let hit = backend
      .search(&parse_query("checklist"))
      .hits
      .into_iter()
      .next()
      .expect("a hit");
    assert!(
      hit
        .snippet
        .iter()
        .any(|part| part.is_match && part.text.eq_ignore_ascii_case("checklist"))
    );
  }

  #[test]
  fn mentions_and_saved_searches_work() {
    let backend = MockBackend::new(NOW);
    assert!(
      backend
        .recent_mentions()
        .iter()
        .all(|hit| hit.message.body.to_lowercase().contains("@alex"))
    );
    let before = backend.saved_searches().len();
    backend.save_search("Mine", "from:alex");
    backend.save_search("Duplicate", "from:alex");
    assert_eq!(backend.saved_searches().len(), before + 1);
    let id = backend
      .saved_searches()
      .last()
      .map(|saved| saved.id)
      .expect("saved");
    backend.delete_saved_search(id);
    assert_eq!(backend.saved_searches().len(), before);
  }
}
