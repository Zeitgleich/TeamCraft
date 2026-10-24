//! Live state a client core hands its UI next to the chat (issue #183). Unread counts and threads
//! travel as the API's own types; these are what the core adds from gateway events. They cross the
//! desktop shell's IPC and the in-page core's message channel as JSON, so they live with the other
//! wire types, shared by `teamcraft-core` and `teamcraft-client-api`.

use serde::{Deserialize, Serialize};

use crate::{ConversationId, MessageId, UserId, UserPresence};

/// Counters that move when the UI should read something from the server once: after each gateway
/// connection (events in between may be lost) and when the announcement changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveMarks {
  pub connections: u64,
  pub announcements: u64,
}

/// How far another member of a DM has read, from `read_receipt` events.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveReceipt {
  pub conversation: ConversationId,
  pub user: UserId,
  pub message: MessageId,
}

/// Presence and custom status as `presence_update` events announced them, newest last, one entry
/// per user and at most a configured number of entries. Each announcement gets the next revision,
/// so a reader applies only what is newer than what it saw instead of comparing everything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncedPresence {
  #[serde(default)]
  pub entries: Vec<RevisedPresence>,
  /// The highest revision handed out so far.
  #[serde(default)]
  pub revision: u64,
  /// The highest revision dropped to stay within the bound. A reader that saw less missed an
  /// announcement and reads presence from the server again.
  #[serde(default)]
  pub evicted_up_to: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisedPresence {
  pub revision: u64,
  pub presence: UserPresence,
}

impl AnnouncedPresence {
  /// Records an announcement: the user's previous entry is replaced, the oldest entries beyond
  /// `max_entries` are dropped.
  pub fn note(&mut self, presence: UserPresence, max_entries: usize) {
    self
      .entries
      .retain(|entry| entry.presence.user_id != presence.user_id);
    self.revision += 1;
    self.entries.push(RevisedPresence {
      revision: self.revision,
      presence,
    });
    let excess = self.entries.len().saturating_sub(max_entries);
    if let Some(newest_dropped) = self.entries.drain(..excess).next_back() {
      self.evicted_up_to = newest_dropped.revision;
    }
  }

  /// Announcements newer than `revision`, oldest first.
  pub fn since(&self, revision: u64) -> impl Iterator<Item = &RevisedPresence> {
    // Entries are in revision order: the seen prefix is skipped without looking at each entry.
    let start = self.entries.partition_point(|entry| entry.revision <= revision);
    self.entries[start..].iter()
  }

  /// Whether a reader that saw up to `revision` missed announcements that were dropped since.
  pub fn has_gap_since(&self, revision: u64) -> bool {
    self.evicted_up_to > revision
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, PresenceState, Snowflake};

  #[test]
  fn a_new_announcement_replaces_the_users_entry() {
    let mut announced = AnnouncedPresence::default();
    announced.note(presence(1, PresenceState::Online), 10);
    announced.note(presence(2, PresenceState::Away), 10);
    announced.note(presence(1, PresenceState::Offline), 10);
    let entries: Vec<_> = announced
      .entries
      .iter()
      .map(|entry| (entry.revision, entry.presence.state.clone()))
      .collect();
    assert_eq!(
      entries,
      vec![(2, PresenceState::Away), (3, PresenceState::Offline)]
    );
    assert!(!announced.has_gap_since(0), "replaced, not dropped");
  }

  #[test]
  fn readers_get_only_what_is_newer() {
    let mut announced = AnnouncedPresence::default();
    for user in 1..=4 {
      announced.note(presence(user, PresenceState::Online), 10);
    }
    let revisions: Vec<u64> = announced.since(2).map(|entry| entry.revision).collect();
    assert_eq!(revisions, vec![3, 4]);
    assert_eq!(announced.since(4).count(), 0);
  }

  #[test]
  fn the_bound_drops_the_oldest_and_reports_the_gap() {
    let mut announced = AnnouncedPresence::default();
    for user in 1..=5 {
      announced.note(presence(user, PresenceState::Online), 3);
    }
    assert_eq!(announced.entries.len(), 3);
    assert_eq!(announced.evicted_up_to, 2);
    assert!(announced.has_gap_since(1));
    assert!(!announced.has_gap_since(2));
  }

  #[test]
  fn the_format_is_stable() {
    let mut announced = AnnouncedPresence::default();
    announced.note(presence(7, PresenceState::Away), 10);
    assert_eq!(
      serde_json::to_value(&announced).unwrap(),
      serde_json::json!({
        "entries": [{ "revision": 1, "presence": { "user_id": "7", "state": "away" } }],
        "revision": 1,
        "evicted_up_to": 0
      })
    );
    assert_eq!(
      serde_json::from_value::<AnnouncedPresence>(serde_json::json!({})).unwrap(),
      AnnouncedPresence::default()
    );
  }

  fn presence(user: i64, state: PresenceState) -> UserPresence {
    UserPresence {
      user_id: UserId::from_snowflake(Snowflake::new(user).unwrap()),
      state,
      maybe_status: None,
      maybe_last_seen: None,
    }
  }
}
