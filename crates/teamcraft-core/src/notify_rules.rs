//! Which messages count as a mention for this user beyond `@username`: `@channel` / `@here` /
//! `@all`, the user's first name and their own keywords (docs/notifications.md).
//!
//! The rules live in the local store so a notification extension, which runs without a
//! [`crate::Client`], applies the same rules as the app. Matching is plain text on decrypted
//! content and happens only on the device.

use serde::{Deserialize, Serialize};
use teamcraft_client_api::notify;

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const MENTION_RULES_RECORD: &str = "mention_rules";

/// The user's notification rules, beyond direct `@username` mentions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionRules {
  /// A message that contains the user's first name counts as a mention.
  pub first_name_is_mention: bool,
  /// `@channel`, `@here` and `@all` count as a mention.
  pub channel_mentions_notify: bool,
  /// Words or phrases that count as a mention (case-insensitive substring).
  pub keywords: Vec<String>,
}

impl Default for MentionRules {
  fn default() -> Self {
    Self {
      first_name_is_mention: true,
      channel_mentions_notify: true,
      keywords: Vec::new(),
    }
  }
}

impl MentionRules {
  /// Whether `body` mentions the user (`@username`, or one of the rules matches). The matching
  /// is shared with desktop (teamcraft-client-api `notify`).
  pub fn is_mentioned(&self, body: &str, maybe_username: Option<&str>, display_name: &str) -> bool {
    notify::is_mentioned(
      body,
      &notify::MentionRules {
        username: maybe_username.unwrap_or_default(),
        display_name,
        first_name_is_mention: self.first_name_is_mention,
        channel_mentions_notify: self.channel_mentions_notify,
        keywords: &self.keywords,
      },
    )
  }

  /// The rules without empty or duplicate keywords, for saving.
  #[must_use]
  pub fn normalized(mut self) -> Self {
    let mut seen: Vec<String> = Vec::new();
    self.keywords.retain_mut(|keyword| {
      let trimmed = keyword.trim().to_owned();
      let lower = trimmed.to_lowercase();
      if trimmed.is_empty() || seen.contains(&lower) {
        return false;
      }
      seen.push(lower);
      *keyword = trimmed;
      true
    });
    self
  }
}

/// The saved rules, or the defaults if none were saved.
pub fn load_mention_rules(store: &impl LocalStore) -> Result<MentionRules, CoreError> {
  Ok(records::get_json(store, Table::Account, MENTION_RULES_RECORD)?.unwrap_or_default())
}

pub fn save_mention_rules(store: &impl LocalStore, rules: &MentionRules) -> Result<(), CoreError> {
  records::put_json(
    store,
    Table::Account,
    MENTION_RULES_RECORD,
    &rules.clone().normalized(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn username_channel_first_name_and_keywords_match() {
    let rules = MentionRules {
      keywords: vec!["Deploy".into()],
      ..MentionRules::default()
    };
    assert!(rules.is_mentioned("hi @alice!", Some("alice"), "Alice Example"));
    assert!(rules.is_mentioned("attention @channel", None, "Alice Example"));
    assert!(rules.is_mentioned("Alice, got a minute?", None, "Alice Example"));
    assert!(rules.is_mentioned("the DEPLOY failed", None, "Alice Example"));
    assert!(!rules.is_mentioned("alicexample is someone else", None, "Alice Example"));
    assert!(!rules.is_mentioned("nothing here", Some("alice"), "Alice Example"));
  }

  #[test]
  fn switched_off_rules_do_not_match() {
    let rules = MentionRules {
      first_name_is_mention: false,
      channel_mentions_notify: false,
      keywords: Vec::new(),
    };
    assert!(!rules.is_mentioned("@here Alice", None, "Alice Example"));
    assert!(rules.is_mentioned("@alice", Some("alice"), "Alice Example"));
  }

  #[test]
  fn rules_round_trip_normalized() {
    let store = MemoryStore::new();
    assert_eq!(load_mention_rules(&store).unwrap(), MentionRules::default());
    let rules = MentionRules {
      keywords: vec![" deploy ".into(), "".into(), "DEPLOY".into(), "release".into()],
      ..MentionRules::default()
    };
    save_mention_rules(&store, &rules).unwrap();
    assert_eq!(
      load_mention_rules(&store).unwrap().keywords,
      ["deploy", "release"]
    );
  }
}
