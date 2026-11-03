//! Live state a client core hands its UI next to the chat (issue #183). Unread counts and threads
//! travel as the API's own types; these are what the core adds from gateway events. They cross the
//! desktop shell's IPC and the in-page core's message channel as JSON, so they live with the other
//! wire types, shared by `teamcraft-core` and `teamcraft-client-api`.

use std::collections::{BTreeMap, HashMap};

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use crate::{ConversationId, MessageId, UserId, UserPresence};

/// Counters that move when the UI should read something from the server once: after each gateway
/// connection (events in between may be lost), when the announcement changed and when a meeting
/// changed (or may have: also after each connection).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveMarks {
  pub connections: u64,
  pub announcements: u64,
  #[serde(default)]
  pub meetings: u64,
  /// The open device link requests changed (e2ee.md §5): the UI reads them from the core.
  #[serde(default)]
  pub link_requests: u64,
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
///
/// Kept ordered by revision with an index by user, so taking `k` announcements costs
/// `O(k log n)`, not a pass over the log per announcement. On the wire it is the entries as a list,
/// oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "AnnouncedPresenceWire")]
pub struct AnnouncedPresence {
  entries: BTreeMap<u64, RevisedPresence>,
  revision_of: HashMap<UserId, u64>,
  /// The highest revision handed out so far.
  pub revision: u64,
  /// The highest revision dropped to stay within the bound. A reader that saw less missed an
  /// announcement and reads presence from the server again.
  pub evicted_up_to: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisedPresence {
  pub revision: u64,
  pub presence: UserPresence,
}

/// The wire form of [`AnnouncedPresence`].
#[derive(Deserialize)]
struct AnnouncedPresenceWire {
  #[serde(default)]
  entries: Vec<RevisedPresence>,
  #[serde(default)]
  revision: u64,
  #[serde(default)]
  evicted_up_to: u64,
}

impl AnnouncedPresence {
  /// A log of `entries` (any order; of a user's entries the newest counts).
  pub fn new(entries: Vec<RevisedPresence>, revision: u64, evicted_up_to: u64) -> Self {
    let mut log = Self {
      revision,
      evicted_up_to,
      ..Self::default()
    };
    for entry in entries {
      log.put(entry);
    }
    log
  }

  /// Records an announcement: the user's previous entry is replaced, the oldest entries beyond
  /// `max_entries` are dropped.
  pub fn note(&mut self, presence: UserPresence, max_entries: usize) {
    self.revision += 1;
    self.put(RevisedPresence {
      revision: self.revision,
      presence,
    });
    while self.entries.len() > max_entries {
      let Some((revision, dropped)) = self.entries.pop_first() else {
        break;
      };
      self.revision_of.remove(&dropped.presence.user_id);
      self.evicted_up_to = revision;
    }
  }

  /// Takes a later read of the log that holds only what is newer than this one (the core sends
  /// changes, issue #259): one entry per user, in revision order. Not bounded: a reader keeps one
  /// entry per person it heard of.
  pub fn merge(&mut self, newer: Self) {
    for entry in newer.entries.into_values() {
      self.put(entry);
    }
    self.revision = self.revision.max(newer.revision);
    self.evicted_up_to = self.evicted_up_to.max(newer.evicted_up_to);
  }

  /// Every entry, oldest first.
  pub fn entries(&self) -> impl Iterator<Item = &RevisedPresence> {
    self.entries.values()
  }

  /// How many people the log holds.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// Announcements newer than `revision`, oldest first.
  pub fn since(&self, revision: u64) -> impl Iterator<Item = &RevisedPresence> {
    self
      .entries
      .range(revision.saturating_add(1)..)
      .map(|(_, entry)| entry)
  }

  /// Whether a reader that saw up to `revision` missed announcements that were dropped since.
  pub fn has_gap_since(&self, revision: u64) -> bool {
    self.evicted_up_to > revision
  }

  /// Keeps `entry` as its user's, unless the log holds a newer one of them.
  fn put(&mut self, entry: RevisedPresence) {
    let user = entry.presence.user_id;
    match self.revision_of.get(&user) {
      Some(known) if *known >= entry.revision => return,
      Some(known) => {
        self.entries.remove(known);
      },
      None => {},
    }
    self.revision_of.insert(user, entry.revision);
    self.entries.insert(entry.revision, entry);
  }
}

impl Serialize for AnnouncedPresence {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    /// The entries as a list, without copying them.
    struct Entries<'a>(&'a BTreeMap<u64, RevisedPresence>);

    impl Serialize for Entries<'_> {
      fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.values())
      }
    }

    let mut wire = serializer.serialize_struct("AnnouncedPresence", 3)?;
    wire.serialize_field("entries", &Entries(&self.entries))?;
    wire.serialize_field("revision", &self.revision)?;
    wire.serialize_field("evicted_up_to", &self.evicted_up_to)?;
    wire.end()
  }
}

impl From<AnnouncedPresenceWire> for AnnouncedPresence {
  fn from(wire: AnnouncedPresenceWire) -> Self {
    Self::new(wire.entries, wire.revision, wire.evicted_up_to)
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
      .entries()
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
    assert_eq!(announced.len(), 3);
    assert_eq!(announced.evicted_up_to, 2);
    assert!(announced.has_gap_since(1));
    assert!(!announced.has_gap_since(2));
  }

  #[test]
  fn a_later_read_merges_per_user() {
    let mut held = AnnouncedPresence::default();
    held.note(presence(1, PresenceState::Online), 10);
    held.note(presence(2, PresenceState::Online), 10);
    let mut log = held.clone();
    log.note(presence(1, PresenceState::Away), 10);
    let newer = AnnouncedPresence::new(
      log.since(held.revision).cloned().collect(),
      log.revision,
      log.evicted_up_to,
    );
    held.merge(newer);
    assert_eq!(held, log);
  }

  #[test]
  fn a_merge_keeps_one_entry_per_user_and_ignores_older_ones() {
    let mut held = AnnouncedPresence::default();
    for user in 1..=3 {
      held.note(presence(user, PresenceState::Online), 10);
    }
    let newer = AnnouncedPresence::new(
      vec![
        revised(5, presence(2, PresenceState::Away)),
        revised(4, presence(1, PresenceState::Offline)),
        revised(1, presence(3, PresenceState::Away)),
      ],
      5,
      0,
    );
    held.merge(newer);
    let entries: Vec<(u64, i64)> = held
      .entries()
      .map(|entry| (entry.revision, entry.presence.user_id.snowflake().get()))
      .collect();
    // User 3's entry at revision 3 is newer than the one at revision 1.
    assert_eq!(entries, vec![(3, 3), (4, 1), (5, 2)]);
    assert_eq!(held.revision, 5);
  }

  #[test]
  fn a_read_log_rebuilds_its_index() {
    let mut log = AnnouncedPresence::default();
    log.note(presence(1, PresenceState::Online), 10);
    let mut read: AnnouncedPresence = serde_json::from_value(serde_json::to_value(&log).unwrap()).unwrap();
    assert_eq!(read, log);
    read.note(presence(1, PresenceState::Away), 10);
    assert_eq!(read.len(), 1);
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

  fn revised(revision: u64, presence: UserPresence) -> RevisedPresence {
    RevisedPresence { revision, presence }
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
