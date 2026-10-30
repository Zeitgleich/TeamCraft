//! The conversations a search looks in. Apart from [`crate::search`] because the browser, where
//! tantivy doesn't run, scopes its substring search the same way.

use std::collections::BTreeSet;

use teamcraft_proto::ConversationId;

/// The conversations a search looks in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConversationScope {
  #[default]
  All,
  /// Only these; an empty set finds nothing (an `in:` filter that names no conversation).
  Only(BTreeSet<ConversationId>),
}

impl ConversationScope {
  pub fn includes(&self, conversation: ConversationId) -> bool {
    match self {
      Self::All => true,
      Self::Only(conversations) => conversations.contains(&conversation),
    }
  }
}
