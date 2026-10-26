//! Where the desktop app reopens (#721): the place the user was last in (the conversation or view,
//! an open thread) and, in a conversation they had scrolled up in, the message they were reading.
//! The browser keeps its place in the URL across a reload; the desktop webview always starts at
//! `/`, so the app remembers the route itself, per account, in this device's `localStorage`.
//!
//! Only public object IDs are stored, like in the URL (security.md). A place that is gone or no
//! longer allowed falls back to the start without a notice (`AppState::restore_route`).

use serde::{Deserialize, Serialize};

use crate::model::{ConversationId, MessageId, UserId};
use crate::route::Route;
use crate::settings::{load_json, store_json};

/// `localStorage` key prefix; the account's user ID follows.
const KEY_PREFIX: &str = "teamcraft.lastPlace.";

/// What is remembered for one account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPlace {
  /// The route's path (`/c/12/t/34`), as in the URL.
  #[serde(default)]
  pub maybe_path: Option<String>,
  /// The message at the top of a conversation the user had scrolled up in; `None` while they
  /// follow the newest messages.
  #[serde(default)]
  pub maybe_reading: Option<ReadingPosition>,
}

/// A reading position in a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingPosition {
  pub conversation: ConversationId,
  pub message: MessageId,
}

impl LastPlace {
  /// The place remembered for `user`, if any.
  pub fn load(user: UserId) -> Self {
    load_json(&key(user)).unwrap_or_default()
  }

  /// The route to reopen: the remembered one, if it is a place worth returning to.
  pub fn route(&self) -> Option<Route> {
    self
      .maybe_path
      .as_deref()
      .and_then(Route::parse)
      .and_then(restorable)
  }

  /// The reading position in `conversation`, if the user had scrolled up there.
  pub fn reading_in(&self, conversation: ConversationId) -> Option<MessageId> {
    self
      .maybe_reading
      .filter(|reading| reading.conversation == conversation)
      .map(|reading| reading.message)
  }
}

/// Remembers `route` as the account's place; routes not worth reopening are skipped, so the last
/// real place stays.
pub fn remember_route(user: UserId, route: Route) {
  let Some(route) = restorable(route) else {
    return;
  };
  let mut place = LastPlace::load(user);
  let path = route.path();
  if place.maybe_path.as_deref() == Some(path.as_str()) {
    return;
  }
  place.maybe_path = Some(path);
  store_json(&key(user), &place);
}

/// Remembers where the user reads in `conversation`: `Some(message)` while scrolled up, `None` at
/// the newest messages.
pub fn remember_reading(user: UserId, conversation: ConversationId, maybe_message: Option<MessageId>) {
  let mut place = LastPlace::load(user);
  let next = match maybe_message {
    Some(message) => Some(ReadingPosition {
      conversation,
      message,
    }),
    // Back at the bottom: forget the position only if it was in this conversation.
    None if place.reading_in(conversation).is_some() => None,
    None => return,
  };
  if place.maybe_reading == next {
    return;
  }
  place.maybe_reading = next;
  store_json(&key(user), &place);
}

/// The route the app reopens instead of `route`: places with their own page, a message as its
/// conversation (or thread), and nothing for dialogs and one-off pages (settings, the admin
/// console, search, a meeting link) or the start itself.
pub fn restorable(route: Route) -> Option<Route> {
  match route {
    Route::Start | Route::Settings(_) | Route::Admin(_) | Route::Search | Route::Meeting(_) => None,
    Route::Message {
      conversation,
      maybe_thread: Some(thread),
      ..
    } => Some(Route::Thread { conversation, thread }),
    Route::Message {
      conversation,
      maybe_thread: None,
      ..
    } => Some(Route::Conversation(conversation)),
    other => Some(other),
  }
}

fn key(user: UserId) -> String {
  format!("{KEY_PREFIX}{user}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{Id, id_from_raw};
  use crate::state::{SettingsPage, SettingsTab};

  fn id<T: Id>(raw: u64) -> T {
    id_from_raw(raw)
  }

  #[test]
  fn places_reopen_and_dialogs_do_not() {
    let conversation: ConversationId = id(12);
    for place in [
      Route::Home,
      Route::Activity,
      Route::Threads,
      Route::Calls,
      Route::Files,
      Route::Saved,
      Route::People,
      Route::Meetings,
      Route::Conversation(conversation),
      Route::Direct(conversation),
      Route::Channel {
        space: id(3),
        channel: conversation,
      },
      Route::Thread {
        conversation,
        thread: crate::model::thread_id_of(id(34)),
      },
    ] {
      assert_eq!(restorable(place), Some(place), "{place:?}");
    }
    for skipped in [
      Route::Start,
      Route::Search,
      Route::Settings(SettingsPage::Sections),
      Route::Settings(SettingsPage::Section(SettingsTab::DEFAULT)),
      Route::Meeting(id(5)),
    ] {
      assert_eq!(restorable(skipped), None, "{skipped:?}");
    }
  }

  #[test]
  fn a_linked_message_reopens_as_its_conversation_or_thread() {
    let conversation: ConversationId = id(12);
    let thread = crate::model::thread_id_of(id(34));
    assert_eq!(
      restorable(Route::Message {
        conversation,
        message: id(56),
        maybe_thread: None
      }),
      Some(Route::Conversation(conversation))
    );
    assert_eq!(
      restorable(Route::Message {
        conversation,
        message: id(56),
        maybe_thread: Some(thread)
      }),
      Some(Route::Thread { conversation, thread })
    );
  }

  #[test]
  fn the_stored_form_reads_back_and_tolerates_old_or_broken_entries() {
    let place = LastPlace {
      maybe_path: Some("/c/12/t/34".to_owned()),
      maybe_reading: Some(ReadingPosition {
        conversation: id(12),
        message: id(56),
      }),
    };
    let text = serde_json::to_string(&place).expect("encodes");
    let read: LastPlace = serde_json::from_str(&text).expect("decodes");
    assert_eq!(read, place);
    assert_eq!(read.reading_in(id(12)), Some(id(56)));
    assert_eq!(read.reading_in(id(13)), None);
    assert!(read.route().is_some());

    let empty: LastPlace = serde_json::from_str("{}").expect("missing fields are empty");
    assert_eq!(empty.route(), None);
    let broken = LastPlace {
      maybe_path: Some("/no/such/place".to_owned()),
      maybe_reading: None,
    };
    assert_eq!(broken.route(), None, "an unknown path reopens nothing");
    let settings = LastPlace {
      maybe_path: Some("/settings".to_owned()),
      maybe_reading: None,
    };
    assert_eq!(settings.route(), None);
  }
}
