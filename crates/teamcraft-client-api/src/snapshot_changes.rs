//! What changed in a client core's [`ChatSnapshot`] since a reader last asked (issue #246): the
//! core sends only the conversations, people and parts that differ, so a change costs work in
//! proportion to what changed, not to the size of the workspace.
//!
//! A reader keeps the [`SnapshotCursor`] of its last answer and hands it back. The core answers with
//! the whole snapshot when it can't tell what the reader has (the first read, or a core that
//! restarted since: another epoch).

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::live::LiveSnapshot;
use crate::model::{Conversation, ConversationId, Space, TimestampMs, User, UserId};
use crate::remote::{ChatSnapshot, SecuritySnapshot};

/// How far a reader has read the core's snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCursor {
  /// Which core instance answered; another one means the reader must start over.
  pub epoch: u64,
  pub revision: u64,
  /// The presence log's revision the reader has (the log has its own numbering).
  #[serde(default)]
  pub presence: u64,
}

/// The answer to "what changed since this cursor".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotChanges {
  pub cursor: SnapshotCursor,
  /// The whole snapshot, when the reader has to start over. The other fields are empty then.
  #[serde(rename = "full", default, skip_serializing_if = "Option::is_none")]
  pub maybe_full: Option<Box<ChatSnapshot>>,
  #[serde(rename = "me", default, skip_serializing_if = "Option::is_none")]
  pub maybe_me: Option<User>,
  /// People whose entry changed or who are new.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub users: Vec<User>,
  /// Everybody's ID, when somebody left the list.
  #[serde(rename = "user_ids", default, skip_serializing_if = "Option::is_none")]
  pub maybe_user_ids: Option<Vec<UserId>>,
  #[serde(rename = "spaces", default, skip_serializing_if = "Option::is_none")]
  pub maybe_spaces: Option<Vec<Space>>,
  /// Conversations whose entry, message count or newest message changed, or that are new.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub conversations: Vec<ConversationChange>,
  /// Conversations whose messages may differ although their entry did not change: a thread reply,
  /// an edit, a reaction. A reader re-reads what it shows of their feeds and threads.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub feeds: Vec<ConversationId>,
  /// The order of all conversations, when it changed (one left, one moved, one is new).
  #[serde(rename = "order", default, skip_serializing_if = "Option::is_none")]
  pub maybe_order: Option<Vec<ConversationId>>,
  #[serde(rename = "security", default, skip_serializing_if = "Option::is_none")]
  pub maybe_security: Option<SecuritySnapshot>,
  /// The live state, when it changed; its presence holds only what is newer than the cursor.
  #[serde(rename = "live", default, skip_serializing_if = "Option::is_none")]
  pub maybe_live: Option<LiveSnapshot>,
}

/// One conversation's part of the snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationChange {
  pub conversation: Conversation,
  pub message_count: usize,
  #[serde(rename = "last_activity", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_activity: Option<TimestampMs>,
}

/// Where each conversation sits in a [`ChatSnapshot`]'s list of conversations, so applying a
/// change and looking a conversation up cost a hash lookup, not a pass over the workspace (issue
/// #246). `message_counts` and `last_activity` are in the order of the conversations, so their
/// entries are found by binary search over these positions.
///
/// Derived data: [`ChatSnapshot::apply`] keeps it current and rebuilds it once it no longer
/// matches (a snapshot built or changed some other way); a lookup it can't answer falls back to a
/// scan. It takes no part in comparing snapshots.
#[derive(Clone, Debug, Default)]
pub struct ConversationIndex {
  positions: HashMap<ConversationId, usize>,
}

/// What applying changes did to a snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppliedChanges {
  /// Anything differs from before.
  pub is_changed: bool,
  /// The snapshot was replaced: every conversation may have changed.
  pub is_replaced: bool,
  /// Conversations whose feed may differ (their messages changed).
  pub conversations: BTreeSet<ConversationId>,
}

impl SnapshotChanges {
  /// Whether there is nothing to apply.
  pub fn is_empty(&self) -> bool {
    self.maybe_full.is_none()
      && self.maybe_me.is_none()
      && self.users.is_empty()
      && self.maybe_user_ids.is_none()
      && self.maybe_spaces.is_none()
      && self.conversations.is_empty()
      && self.feeds.is_empty()
      && self.maybe_order.is_none()
      && self.maybe_security.is_none()
      && self.maybe_live.is_none()
  }
}

impl AppliedChanges {
  /// Whether a conversation's feed has to be read again.
  pub fn touches(&self, conversation: ConversationId) -> bool {
    self.is_replaced || self.conversations.contains(&conversation)
  }
}

impl ConversationIndex {
  /// The conversation's position in `conversations`, if it is there.
  fn position(&self, conversations: &[Conversation], id: ConversationId) -> Option<usize> {
    match self.positions.get(&id) {
      Some(&index) if conversations.get(index).is_some_and(|known| known.id == id) => Some(index),
      None if self.positions.len() == conversations.len() => None,
      // Out of date: answered by a scan until the next `apply` rebuilds it.
      _ => conversations.iter().position(|known| known.id == id),
    }
  }

  /// Makes sure every position is current; rebuilds the index if one is not.
  fn sync(&mut self, conversations: &[Conversation]) {
    let is_current = self.positions.len() == conversations.len()
      && conversations
        .last()
        .is_none_or(|last| self.positions.get(&last.id) == Some(&(conversations.len() - 1)));
    if !is_current {
      self.rebuild(conversations);
    }
  }

  fn rebuild(&mut self, conversations: &[Conversation]) {
    self.positions = conversations
      .iter()
      .enumerate()
      .map(|(index, conversation)| (conversation.id, index))
      .collect();
  }
}

impl PartialEq for ConversationIndex {
  /// Derived data: two snapshots with the same content are the same, however they were built.
  fn eq(&self, _: &Self) -> bool {
    true
  }
}

impl ChatSnapshot {
  /// The conversation with this ID.
  pub fn conversation(&self, id: ConversationId) -> Option<&Conversation> {
    self
      .index
      .position(&self.conversations, id)
      .map(|index| &self.conversations[index])
  }

  /// The conversation with this ID, to change it in place (not its ID).
  pub fn conversation_mut(&mut self, id: ConversationId) -> Option<&mut Conversation> {
    self
      .index
      .position(&self.conversations, id)
      .map(|index| &mut self.conversations[index])
  }

  /// The conversation's top-level message count, if the snapshot has one.
  pub fn message_count(&self, id: ConversationId) -> Option<usize> {
    self
      .ordered_index(&self.message_counts, id)
      .ok()
      .map(|index| self.message_counts[index].1)
  }

  /// When the conversation's newest top-level message was sent, if it has one.
  pub fn last_activity_of(&self, id: ConversationId) -> Option<TimestampMs> {
    self
      .ordered_index(&self.last_activity, id)
      .ok()
      .map(|index| self.last_activity[index].1)
  }

  /// Takes what changed since this snapshot was read.
  pub fn apply(&mut self, mut changes: SnapshotChanges) -> AppliedChanges {
    if let Some(full) = changes.maybe_full {
      let is_changed = *self != *full;
      *self = *full;
      return AppliedChanges {
        is_changed,
        is_replaced: true,
        conversations: BTreeSet::new(),
      };
    }
    // Feeds are not part of the snapshot: they only tell which conversations to re-read.
    let feeds = std::mem::take(&mut changes.feeds);
    let is_changed = !changes.is_empty();
    if let Some(me) = changes.maybe_me {
      self.me = me;
    }
    self.apply_users(changes.users, changes.maybe_user_ids);
    if let Some(spaces) = changes.maybe_spaces {
      self.spaces = spaces;
    }
    let conversations = changes
      .conversations
      .iter()
      .map(|change| change.conversation.id)
      .chain(feeds)
      .collect();
    for change in changes.conversations {
      self.apply_conversation(change);
    }
    if let Some(order) = changes.maybe_order {
      self.reorder(&order);
    }
    if let Some(security) = changes.maybe_security {
      self.security = security;
    }
    if let Some(live) = changes.maybe_live {
      self.apply_live(live);
    }
    AppliedChanges {
      is_changed,
      is_replaced: false,
      conversations,
    }
  }

  /// People are kept in ID order, like the core lists them.
  fn apply_users(&mut self, users: Vec<User>, maybe_ids: Option<Vec<UserId>>) {
    for user in users {
      match self.users.binary_search_by_key(&user.id, |known| known.id) {
        Ok(index) => self.users[index] = user,
        Err(index) => self.users.insert(index, user),
      }
    }
    if let Some(ids) = maybe_ids {
      let ids: HashSet<UserId> = ids.into_iter().collect();
      self.users.retain(|user| ids.contains(&user.id));
    }
  }

  fn apply_conversation(&mut self, change: ConversationChange) {
    let id = change.conversation.id;
    self.index.sync(&self.conversations);
    match self.index.position(&self.conversations, id) {
      Some(index) => self.conversations[index] = change.conversation,
      None => {
        // New conversations go last, like a full read lists those it didn't know.
        self.index.positions.insert(id, self.conversations.len());
        self.conversations.push(change.conversation);
      },
    }
    match self.ordered_index(&self.message_counts, id) {
      Ok(index) => self.message_counts[index].1 = change.message_count,
      Err(index) => self.message_counts.insert(index, (id, change.message_count)),
    }
    match (
      self.ordered_index(&self.last_activity, id),
      change.maybe_last_activity,
    ) {
      (Ok(index), Some(at)) => self.last_activity[index].1 = at,
      (Ok(index), None) => {
        self.last_activity.remove(index);
      },
      (Err(index), Some(at)) => self.last_activity.insert(index, (id, at)),
      (Err(_), None) => {},
    }
  }

  /// Where the conversation's entry is in a list kept in the order of the conversations, or where
  /// it belongs.
  fn ordered_index<T>(&self, entries: &[(ConversationId, T)], id: ConversationId) -> Result<usize, usize> {
    let position_of = |conversation: ConversationId| {
      self
        .index
        .position(&self.conversations, conversation)
        .unwrap_or(usize::MAX)
    };
    let own = position_of(id);
    if own == usize::MAX {
      // Not a listed conversation: found only by looking.
      return entries
        .iter()
        .position(|(known, _)| *known == id)
        .ok_or(entries.len());
    }
    entries.binary_search_by_key(&own, |(other, _)| position_of(*other))
  }

  /// Puts the conversations in `order`; those not in it are gone.
  fn reorder(&mut self, order: &[ConversationId]) {
    let position: HashMap<ConversationId, usize> =
      order.iter().enumerate().map(|(index, id)| (*id, index)).collect();
    self
      .conversations
      .retain(|conversation| position.contains_key(&conversation.id));
    self
      .conversations
      .sort_by_key(|conversation| position[&conversation.id]);
    self.message_counts.retain(|(id, _)| position.contains_key(id));
    self.message_counts.sort_by_key(|(id, _)| position[id]);
    self.last_activity.retain(|(id, _)| position.contains_key(id));
    self.last_activity.sort_by_key(|(id, _)| position[id]);
    self.index.rebuild(&self.conversations);
  }

  /// The live state is replaced, except presence: the changes carry only newer announcements.
  fn apply_live(&mut self, mut live: LiveSnapshot) {
    if let Some(held) = self.maybe_live.take() {
      let mut presence = held.presence;
      presence.merge(live.presence);
      live.presence = presence;
    }
    self.maybe_live = Some(live);
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::live::AnnouncedPresence;
  use teamcraft_proto::{PresenceState, UserPresence};

  use super::*;
  use crate::model::{ConversationKind, DeviceSetup, PresenceStatus, SidebarCategory, id_from_raw, raw_id};

  #[test]
  fn a_full_answer_replaces_everything() {
    let mut held = snapshot(&[1, 2]);
    let applied = held.apply(SnapshotChanges {
      maybe_full: Some(Box::new(snapshot(&[3]))),
      ..SnapshotChanges::default()
    });
    assert!(applied.is_changed && applied.is_replaced);
    assert!(applied.touches(id_from_raw(1)));
    assert_eq!(held, snapshot(&[3]));
  }

  #[test]
  fn no_changes_change_nothing() {
    let mut held = snapshot(&[1, 2]);
    let applied = held.apply(SnapshotChanges::default());
    assert_eq!(applied, AppliedChanges::default());
    assert_eq!(held, snapshot(&[1, 2]));
  }

  #[test]
  fn a_changed_conversation_is_replaced_in_place() {
    let mut held = snapshot(&[1, 2, 3]);
    let mut renamed = conversation(2);
    renamed.name = "renamed".into();
    let applied = held.apply(SnapshotChanges {
      conversations: vec![ConversationChange {
        conversation: renamed,
        message_count: 9,
        maybe_last_activity: Some(500),
      }],
      ..SnapshotChanges::default()
    });
    assert!(applied.is_changed && !applied.is_replaced);
    assert!(applied.touches(id_from_raw(2)) && !applied.touches(id_from_raw(1)));
    assert_eq!(held.conversations[1].name, "renamed");
    assert_eq!(held.message_counts[1], (id_from_raw(2), 9));
    assert!(held.last_activity.contains(&(id_from_raw(2), 500)));
  }

  #[test]
  fn a_changed_feed_touches_its_conversation_but_not_the_snapshot() {
    let mut held = snapshot(&[1, 2]);
    let applied = held.apply(SnapshotChanges {
      feeds: vec![id_from_raw(2)],
      ..SnapshotChanges::default()
    });
    assert!(!applied.is_changed && !applied.is_replaced);
    assert!(applied.touches(id_from_raw(2)) && !applied.touches(id_from_raw(1)));
    assert_eq!(held, snapshot(&[1, 2]));
  }

  #[test]
  fn newest_message_times_stay_in_the_order_of_the_conversations() {
    let mut held = snapshot(&[1, 2, 3, 4]);
    for (id, at) in [(3, 30), (1, 10), (4, 40), (2, 20)] {
      held.apply(SnapshotChanges {
        conversations: vec![ConversationChange {
          conversation: conversation(id),
          message_count: 1,
          maybe_last_activity: Some(at),
        }],
        ..SnapshotChanges::default()
      });
    }
    let order: Vec<(u64, TimestampMs)> = held
      .last_activity
      .iter()
      .map(|(id, at)| (raw_id(*id), *at))
      .collect();
    assert_eq!(order, vec![(1, 10), (2, 20), (3, 30), (4, 40)]);
    assert_eq!(held.last_activity_of(id_from_raw(3)), Some(30));
    assert_eq!(held.message_count(id_from_raw(4)), Some(1));
    assert_eq!(
      held.conversation(id_from_raw(2)).map(|found| found.id),
      Some(id_from_raw(2))
    );
  }

  #[test]
  fn lookups_survive_a_snapshot_changed_by_hand() {
    let mut held = snapshot(&[1, 2]);
    held.apply(SnapshotChanges {
      conversations: vec![ConversationChange {
        conversation: conversation(3),
        message_count: 5,
        maybe_last_activity: None,
      }],
      ..SnapshotChanges::default()
    });
    held.conversations.swap(0, 2);
    held.message_counts.swap(0, 2);
    assert_eq!(
      held.conversation(id_from_raw(1)).map(|found| found.id),
      Some(id_from_raw(1))
    );
    // The next change rebuilds the index and lands on the right entry.
    held.apply(SnapshotChanges {
      conversations: vec![ConversationChange {
        conversation: conversation(1),
        message_count: 7,
        maybe_last_activity: None,
      }],
      ..SnapshotChanges::default()
    });
    assert_eq!(held.message_count(id_from_raw(1)), Some(7));
    assert_eq!(held.conversations.len(), 3);
  }

  #[test]
  fn a_new_order_drops_and_moves_conversations() {
    let mut held = snapshot(&[1, 2, 3]);
    held.apply(SnapshotChanges {
      conversations: vec![ConversationChange {
        conversation: conversation(4),
        message_count: 0,
        maybe_last_activity: None,
      }],
      maybe_order: Some(vec![id_from_raw(4), id_from_raw(3), id_from_raw(1)]),
      ..SnapshotChanges::default()
    });
    let ids: Vec<ConversationId> = held
      .conversations
      .iter()
      .map(|conversation| conversation.id)
      .collect();
    assert_eq!(ids, vec![id_from_raw(4), id_from_raw(3), id_from_raw(1)]);
    let counted: Vec<ConversationId> = held.message_counts.iter().map(|(id, _)| *id).collect();
    assert_eq!(counted, ids);
  }

  #[test]
  fn people_stay_in_id_order_and_leave_with_the_id_list() {
    let mut held = snapshot(&[]);
    held.users = vec![user(1), user(5)];
    held.apply(SnapshotChanges {
      users: vec![user(3)],
      ..SnapshotChanges::default()
    });
    held.apply(SnapshotChanges {
      maybe_user_ids: Some(vec![id_from_raw(3), id_from_raw(5)]),
      ..SnapshotChanges::default()
    });
    let ids: Vec<UserId> = held.users.iter().map(|user| user.id).collect();
    assert_eq!(ids, vec![id_from_raw(3), id_from_raw(5)]);
  }

  #[test]
  fn presence_of_the_live_state_is_merged() {
    let mut held = snapshot(&[]);
    let mut log = AnnouncedPresence::default();
    log.note(presence(1, PresenceState::Online), 10);
    held.apply(live_with(log.clone()));
    let seen = log.revision;
    log.note(presence(2, PresenceState::Away), 10);
    let newer = AnnouncedPresence::new(log.since(seen).cloned().collect(), log.revision, 0);
    held.apply(live_with(newer));
    assert_eq!(held.maybe_live.unwrap().presence, log);
  }

  #[test]
  fn the_wire_form_leaves_out_what_did_not_change() {
    let changes = SnapshotChanges {
      cursor: SnapshotCursor {
        epoch: 1,
        revision: 2,
        presence: 3,
      },
      ..SnapshotChanges::default()
    };
    assert_eq!(
      serde_json::to_value(&changes).unwrap(),
      serde_json::json!({ "cursor": { "epoch": 1, "revision": 2, "presence": 3 } })
    );
  }

  fn live_with(presence: AnnouncedPresence) -> SnapshotChanges {
    SnapshotChanges {
      maybe_live: Some(LiveSnapshot {
        presence,
        ..LiveSnapshot::default()
      }),
      ..SnapshotChanges::default()
    }
  }

  fn presence(user: u64, state: PresenceState) -> UserPresence {
    UserPresence {
      user_id: id_from_raw(user),
      state,
      maybe_status: None,
      maybe_last_seen: None,
    }
  }

  fn snapshot(conversations: &[u64]) -> ChatSnapshot {
    ChatSnapshot {
      me: user(1),
      users: Vec::new(),
      spaces: Vec::new(),
      conversations: conversations.iter().map(|id| conversation(*id)).collect(),
      message_counts: conversations.iter().map(|id| (id_from_raw(*id), 1)).collect(),
      last_activity: Vec::new(),
      security: SecuritySnapshot::default(),
      maybe_live: None,
      index: ConversationIndex::default(),
    }
  }

  fn conversation(id: u64) -> Conversation {
    Conversation {
      id: id_from_raw(id),
      maybe_space: None,
      kind: ConversationKind::Public,
      category: SidebarCategory::Channels,
      name: format!("c{id}"),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }

  fn user(id: u64) -> User {
    User {
      id: id_from_raw(id),
      username: String::new(),
      display_name: format!("u{id}"),
      presence: PresenceStatus::Offline,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: DeviceSetup::Unknown,
      maybe_email: None,
    }
  }
}
