//! The user's mention preferences, kept in the sealed local store so that every process that opens
//! it (the app and the iOS notification extension) applies the same ones. The matching itself is
//! `teamcraft_client_api::notify::is_mentioned`, shared with the desktop; what counts as a mention
//! beyond `@username` is decided on the device, on the decrypted text (docs/notifications.md).

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const MENTION_PREFERENCES_RECORD: &str = "mention_rules";

/// What counts as a mention besides `@username`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MentionPreferences {
  /// A message that contains the user's first name counts as a mention.
  pub first_name_is_mention: bool,
  /// `@channel`, `@here` and `@all` count as a mention.
  pub channel_mentions_notify: bool,
  /// Words or phrases that count as a mention (case-insensitive substring).
  pub keywords: Vec<String>,
}

impl Default for MentionPreferences {
  /// Desktop's defaults: the first name and `@channel` notify, no keywords.
  fn default() -> Self {
    Self {
      first_name_is_mention: true,
      channel_mentions_notify: true,
      keywords: Vec::new(),
    }
  }
}

impl MentionPreferences {
  /// The preferences without empty or duplicate keywords, for saving.
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

/// The saved preferences, or the defaults if none were saved.
pub fn load_mention_preferences(store: &impl LocalStore) -> Result<MentionPreferences, CoreError> {
  Ok(records::get_json(store, Table::Account, MENTION_PREFERENCES_RECORD)?.unwrap_or_default())
}

pub fn save_mention_preferences(
  store: &impl LocalStore,
  preferences: &MentionPreferences,
) -> Result<(), CoreError> {
  records::put_json(
    store,
    Table::Account,
    MENTION_PREFERENCES_RECORD,
    &preferences.clone().normalized(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn preferences_round_trip_normalized() {
    let store = MemoryStore::new();
    assert_eq!(
      load_mention_preferences(&store).unwrap(),
      MentionPreferences::default()
    );
    let preferences = MentionPreferences {
      keywords: vec![" deploy ".into(), "".into(), "DEPLOY".into(), "release".into()],
      ..MentionPreferences::default()
    };
    save_mention_preferences(&store, &preferences).unwrap();
    assert_eq!(
      load_mention_preferences(&store).unwrap().keywords,
      ["deploy", "release"]
    );
  }
}
