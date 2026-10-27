//! The user's personal data that follows them across devices (e2ee.md §6.4): saved items, drafts
//! and saved searches, and the trust decisions (pinned identity keys and the workspace root of
//! trust, security review A2), so a new or linked device doesn't trust the server on first use
//! again. It travels inside the encrypted key bag, so the server sees neither content nor count.
//!
//! Devices change it independently and upload whole key bags, so every entry carries the time of
//! its last change and removals stay as tombstones; merging keeps the newer side per entry. Clocks
//! of one user's devices are close enough for "last edit wins" on personal data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  BackgroundId, Base64Bytes, ClientMessageId, ConversationId, MessageId, ThreadId, UserId,
};

/// Tombstones older than this are dropped when merging (every device has synced by then).
pub const TOMBSTONE_LIFETIME_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// How far the activity read mark may lie ahead of this device's clock.
const MAX_ACTIVITY_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountData {
  /// Saved messages, keyed by [`saved_key`].
  #[serde(default)]
  pub saved: BTreeMap<String, Stamped<SavedItem>>,
  /// Unsent text per conversation or thread, keyed by [`draft_key`]; an empty body is "no draft".
  #[serde(default)]
  pub drafts: BTreeMap<String, Stamped<Draft>>,
  /// Saved searches, keyed by their ID (random, so devices don't collide).
  #[serde(default)]
  pub saved_searches: BTreeMap<u64, Stamped<SavedSearchEntry>>,
  /// Conversations the user marked unread, keyed by conversation ID (the server's read position
  /// only moves forward, so the mark is the user's own).
  #[serde(default)]
  pub marked_unread: BTreeMap<String, Stamped<UnreadMark>>,
  /// Identity keys the user's devices pinned, keyed by user ID; an accepted key change is a newer
  /// entry.
  #[serde(default)]
  pub pins: BTreeMap<String, Stamped<PinnedKey>>,
  /// The user's own call background pictures, keyed by their ID: the key that opens each
  /// encrypted picture lives only here.
  #[serde(default)]
  pub backgrounds: BTreeMap<String, Stamped<BackgroundEntry>>,
  /// Superseded (issue #534): the feed's unread state follows the read positions of its targets.
  /// Still merged and written back so that older devices keep their value.
  #[serde(rename = "activity_read", default, skip_serializing_if = "Option::is_none")]
  pub maybe_activity_read: Option<Stamped<i64>>,
  /// Per conversation ID, the newest MLS epoch in which one of the user's devices switched the
  /// content key (a removal, a "from join" addition, a rejoin). A new device of the user refuses
  /// group information older than that: a server could hand out an old one to fork the device into
  /// a group that still holds a removed member (e2ee.md §4.6). Only grows.
  #[serde(default)]
  pub group_epochs: BTreeMap<String, u64>,
  /// When devices left conversations and came back, keyed `conversation/user/device`, in the server
  /// times of the commits, as the user's devices processed them: a new device of the user refuses
  /// messages a device signs while it is out (e2ee.md §4.4). Merging keeps the later time of each,
  /// so no device's clock decides.
  #[serde(default)]
  pub removed_senders: BTreeMap<String, SenderAbsence>,
  /// Removal records dropped because they can't matter any more (issue #186), keyed like
  /// `removed_senders`: an absence that left at or before the tombstone's time is not taken back
  /// from another device or an older key bag. Kept for [`TOMBSTONE_LIFETIME_MS`].
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub pruned_senders: BTreeMap<String, PrunedSender>,
  /// The workspace's first owner, as the first of the user's devices verified it.
  #[serde(rename = "workspace_root", default, skip_serializing_if = "Option::is_none")]
  pub maybe_workspace_root: Option<PinnedKey>,
  /// When one of the user's devices last set the recovery passphrase (its clock), for the security
  /// settings (issue #410). Merging keeps the later time; an older app drops the field on upload,
  /// and the next newer device puts it back.
  #[serde(
    rename = "recovery_passphrase_set_at",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_recovery_passphrase_set_at: Option<i64>,
}

/// When a device last left a conversation, and when it was last added back, in server times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderAbsence {
  pub left_at_ms: i64,
  #[serde(rename = "back_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_back_at_ms: Option<i64>,
}

impl SenderAbsence {
  /// Whether a message the device signed at `sent_at_ms` falls into its absence: after it left
  /// (with `grace_ms` for messages on the way) and before it came back, if it did.
  pub fn covers(&self, sent_at_ms: i64, grace_ms: i64) -> bool {
    let is_after_leaving = sent_at_ms > self.left_at_ms.saturating_add(grace_ms);
    let is_before_return = match self.maybe_back_at_ms {
      Some(back) if back >= self.left_at_ms => sent_at_ms < back,
      _ => true,
    };
    is_after_leaving && is_before_return
  }

  /// Takes the later time of each from `other`; returns whether that changed anything.
  pub fn merge(&mut self, other: &Self) -> bool {
    let before = *self;
    self.left_at_ms = self.left_at_ms.max(other.left_at_ms);
    self.maybe_back_at_ms = self.maybe_back_at_ms.max(other.maybe_back_at_ms);
    *self != before
  }
}

/// A dropped removal record: when the device had left (server time) and when it was dropped
/// (the pruning device's clock, for forgetting the tombstone like the others).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrunedSender {
  pub left_at_ms: i64,
  pub pruned_at_ms: i64,
}

impl PrunedSender {
  /// Whether an absence that started at `left_at_ms` was dropped with this tombstone.
  pub fn covers(&self, left_at_ms: i64) -> bool {
    left_at_ms <= self.left_at_ms
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedKey {
  pub user_id: UserId,
  pub public_key: Base64Bytes,
}

/// A value with the time of its last change; `is_removed` marks a deletion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped<T> {
  pub value: T,
  pub updated_at_ms: i64,
  #[serde(default)]
  pub is_removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedItem {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
  pub conversation_id: ConversationId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadMark {
  pub conversation_id: ConversationId,
  /// Unread from this message on.
  pub from_message: MessageId,
}

/// One of the user's own call backgrounds: the encrypted picture on the server and its key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundEntry {
  pub name: String,
  /// The picture's key (an attachment key, e2ee.md §4.5).
  pub key: Base64Bytes,
  pub ciphertext_sha256: Base64Bytes,
  /// Size of the picture (a JPEG) before encryption.
  pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearchEntry {
  pub name: String,
  pub query: String,
}

impl AccountData {
  /// Decodes the key bag's account data; an empty field (new or version 1 key bag) is empty data.
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
    if bytes.is_empty() {
      Ok(Self::default())
    } else {
      serde_json::from_slice(bytes)
    }
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(self)
  }

  /// Takes the newer side of every entry from `other`, then forgets old tombstones.
  pub fn merge(&mut self, other: &Self, now_ms: i64) {
    merge_map(&mut self.saved, &other.saved);
    merge_map(&mut self.drafts, &other.drafts);
    merge_map(&mut self.saved_searches, &other.saved_searches);
    merge_map(&mut self.marked_unread, &other.marked_unread);
    merge_map(&mut self.pins, &other.pins);
    merge_map(&mut self.backgrounds, &other.backgrounds);
    for (key, theirs) in &other.pruned_senders {
      let entry = self.pruned_senders.entry(key.clone()).or_insert(*theirs);
      entry.left_at_ms = entry.left_at_ms.max(theirs.left_at_ms);
      entry.pruned_at_ms = entry.pruned_at_ms.max(theirs.pruned_at_ms);
    }
    for (key, theirs) in &other.removed_senders {
      self
        .removed_senders
        .entry(key.clone())
        .or_insert(*theirs)
        .merge(theirs);
    }
    // Pruned on either side stays pruned, unless the device left again since.
    let pruned = &self.pruned_senders;
    self.removed_senders.retain(|key, absence| {
      !pruned
        .get(key)
        .is_some_and(|tombstone| tombstone.covers(absence.left_at_ms))
    });
    for (conversation, epoch) in &other.group_epochs {
      let entry = self.group_epochs.entry(conversation.clone()).or_insert(*epoch);
      *entry = (*entry).max(*epoch);
    }
    // Reading only moves forward: the later read time wins whatever device recorded it.
    let newer_read = [self.maybe_activity_read.take(), other.maybe_activity_read.clone()]
      .into_iter()
      .flatten()
      .max_by_key(|entry| (entry.value, entry.updated_at_ms));
    // A mark from the future (another device, or older versions) is capped like a new one.
    self.maybe_activity_read = newer_read.map(|mut entry| {
      entry.value = entry.value.min(now_ms.saturating_add(MAX_ACTIVITY_CLOCK_SKEW_MS));
      entry
    });
    // The root never changes; the first one a device of the user verified stays.
    if self.maybe_workspace_root.is_none() {
      self.maybe_workspace_root = other.maybe_workspace_root.clone();
    }
    self.maybe_recovery_passphrase_set_at = self
      .maybe_recovery_passphrase_set_at
      .max(other.maybe_recovery_passphrase_set_at);
    self.forget_tombstones(now_ms);
  }

  pub fn forget_tombstones(&mut self, now_ms: i64) {
    let is_fresh = |entry_removed: bool, updated_at_ms: i64| {
      !entry_removed || now_ms.saturating_sub(updated_at_ms) < TOMBSTONE_LIFETIME_MS
    };
    self
      .saved
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .drafts
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .saved_searches
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .marked_unread
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .backgrounds
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .pruned_senders
      .retain(|_, tombstone| is_fresh(true, tombstone.pruned_at_ms));
  }

  /// Drops the removal records `is_spent` picks (issue #186) and leaves tombstones, so the same
  /// absence isn't taken back from another device's records or an older key bag. Returns whether
  /// anything was dropped.
  pub fn prune_removed_senders(
    &mut self,
    is_spent: impl Fn(&str, &SenderAbsence) -> bool,
    now_ms: i64,
  ) -> bool {
    let spent: Vec<(String, i64)> = self
      .removed_senders
      .iter()
      .filter(|(key, absence)| is_spent(key, absence))
      .map(|(key, absence)| (key.clone(), absence.left_at_ms))
      .collect();
    for (key, left_at_ms) in &spent {
      self.removed_senders.remove(key);
      let tombstone = self.pruned_senders.entry(key.clone()).or_insert(PrunedSender {
        left_at_ms: *left_at_ms,
        pruned_at_ms: now_ms,
      });
      tombstone.left_at_ms = tombstone.left_at_ms.max(*left_at_ms);
      tombstone.pruned_at_ms = tombstone.pruned_at_ms.max(now_ms);
    }
    !spent.is_empty()
  }

  /// Whether the absence of `key` that started at `left_at_ms` was dropped as spent.
  pub fn is_pruned_sender(&self, key: &str, left_at_ms: i64) -> bool {
    self
      .pruned_senders
      .get(key)
      .is_some_and(|tombstone| tombstone.covers(left_at_ms))
  }

  /// When the device `key` (`conversation/user/device`) left its conversation, if it is out (not
  /// added back since).
  pub fn removed_sender(&self, key: &str) -> Option<i64> {
    self
      .removed_senders
      .get(key)
      .filter(|entry| entry.maybe_back_at_ms.is_none_or(|back| back < entry.left_at_ms))
      .map(|entry| entry.left_at_ms)
  }

  /// Records that a device left a conversation at `left_at_ms` (server time); returns whether that
  /// changed anything. A removal before the one recorded, or one already pruned, changes nothing.
  pub fn note_removed_sender(&mut self, key: &str, left_at_ms: i64) -> bool {
    if self.is_pruned_sender(key, left_at_ms) {
      return false;
    }
    match self.removed_senders.get_mut(key) {
      Some(entry) if entry.left_at_ms >= left_at_ms => false,
      Some(entry) => {
        entry.left_at_ms = left_at_ms;
        true
      },
      None => {
        self.removed_senders.insert(
          key.to_string(),
          SenderAbsence {
            left_at_ms,
            maybe_back_at_ms: None,
          },
        );
        true
      },
    }
  }

  /// Records that a device that had left was added back at `back_at_ms` (server time); returns
  /// whether that changed anything.
  pub fn note_sender_back(&mut self, key: &str, back_at_ms: i64) -> bool {
    let Some(entry) = self.removed_senders.get_mut(key) else {
      return false;
    };
    if entry.maybe_back_at_ms.is_some_and(|back| back >= back_at_ms) {
      return false;
    }
    entry.maybe_back_at_ms = Some(back_at_ms);
    true
  }

  /// The newest epoch in which the user's devices switched the conversation's content key; 0 if
  /// none did.
  pub fn group_epoch(&self, conversation: ConversationId) -> u64 {
    self
      .group_epochs
      .get(&conversation.to_string())
      .copied()
      .unwrap_or(0)
  }

  /// Records that the conversation's content key changed in `epoch`; returns whether that moved
  /// the floor.
  pub fn note_group_epoch(&mut self, conversation: ConversationId, epoch: u64) -> bool {
    if epoch <= self.group_epoch(conversation) {
      return false;
    }
    self.group_epochs.insert(conversation.to_string(), epoch);
    true
  }

  /// Activity entries sent up to this time (Unix milliseconds) were read; 0 if never.
  pub fn activity_read_at_ms(&self) -> i64 {
    self.maybe_activity_read.as_ref().map_or(0, |entry| entry.value)
  }

  /// Records that the feed was read up to `read_at_ms`; returns whether that moved the mark. The
  /// times come from server-assigned message IDs, so the mark never goes past now (plus some clock
  /// skew): one message with a far-future ID must not mark everything after it read.
  pub fn mark_activity_read(&mut self, read_at_ms: i64, now_ms: i64) -> bool {
    let read_at_ms = read_at_ms.min(now_ms.saturating_add(MAX_ACTIVITY_CLOCK_SKEW_MS));
    if read_at_ms <= self.activity_read_at_ms() {
      return false;
    }
    self.maybe_activity_read = Some(Stamped {
      value: read_at_ms,
      updated_at_ms: now_ms,
      is_removed: false,
    });
    true
  }

  /// The identity key the user's devices pinned for `user`.
  pub fn pinned_key(&self, user: UserId) -> Option<[u8; 32]> {
    self
      .pins
      .get(&user.to_string())
      .filter(|entry| !entry.is_removed)
      .and_then(|entry| <[u8; 32]>::try_from(entry.value.public_key.as_slice()).ok())
  }

  /// Records a pin; returns whether it changed anything.
  pub fn pin(&mut self, user: UserId, public_key: [u8; 32], now_ms: i64) -> bool {
    if self.pinned_key(user) == Some(public_key) {
      return false;
    }
    self.pins.insert(
      user.to_string(),
      Stamped {
        value: PinnedKey {
          user_id: user,
          public_key: Base64Bytes::new(public_key.to_vec()),
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
    true
  }

  /// Where the user marked the conversation unread, if they did and haven't read it since.
  pub fn unread_mark(&self, conversation: ConversationId) -> Option<MessageId> {
    self
      .marked_unread
      .get(&conversation.to_string())
      .filter(|entry| !entry.is_removed)
      .map(|entry| entry.value.from_message)
  }

  pub fn mark_unread(&mut self, conversation: ConversationId, from: MessageId, now_ms: i64) {
    self.marked_unread.insert(
      conversation.to_string(),
      Stamped {
        value: UnreadMark {
          conversation_id: conversation,
          from_message: from,
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
  }

  /// Removes the mark (the user read the conversation); returns whether there was one.
  pub fn clear_unread_mark(&mut self, conversation: ConversationId, now_ms: i64) -> bool {
    match self.marked_unread.get_mut(&conversation.to_string()) {
      Some(entry) if !entry.is_removed => {
        entry.is_removed = true;
        entry.updated_at_ms = now_ms;
        true
      },
      _ => false,
    }
  }

  /// Saved items, newest first.
  pub fn saved_items(&self) -> Vec<(&SavedItem, i64)> {
    let mut items: Vec<(&SavedItem, i64)> = self
      .saved
      .values()
      .filter(|entry| !entry.is_removed)
      .map(|entry| (&entry.value, entry.updated_at_ms))
      .collect();
    items.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
    items
  }

  pub fn is_saved(&self, conversation: ConversationId, message: ClientMessageId) -> bool {
    self
      .saved
      .get(&saved_key(conversation, message))
      .is_some_and(|entry| !entry.is_removed)
  }

  pub fn set_saved(
    &mut self,
    conversation: ConversationId,
    message: ClientMessageId,
    is_saved: bool,
    now_ms: i64,
  ) {
    self.saved.insert(
      saved_key(conversation, message),
      Stamped {
        value: SavedItem {
          conversation_id: conversation,
          client_message_id: message,
        },
        updated_at_ms: now_ms,
        is_removed: !is_saved,
      },
    );
  }

  pub fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<&str> {
    self
      .drafts
      .get(&draft_key(conversation, maybe_thread))
      .filter(|entry| !entry.is_removed && !entry.value.body.is_empty())
      .map(|entry| entry.value.body.as_str())
  }

  /// Stores (or, with an empty body, clears) the draft; returns whether anything changed.
  pub fn set_draft(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
    now_ms: i64,
  ) -> bool {
    if self.draft(conversation, maybe_thread).unwrap_or_default() == body {
      return false;
    }
    self.drafts.insert(
      draft_key(conversation, maybe_thread),
      Stamped {
        value: Draft {
          conversation_id: conversation,
          maybe_thread_id: maybe_thread,
          body: body.to_string(),
        },
        updated_at_ms: now_ms,
        is_removed: body.is_empty(),
      },
    );
    true
  }

  pub fn saved_searches(&self) -> Vec<(u64, &SavedSearchEntry)> {
    self
      .saved_searches
      .iter()
      .filter(|(_, entry)| !entry.is_removed)
      .map(|(id, entry)| (*id, &entry.value))
      .collect()
  }

  pub fn save_search(&mut self, id: u64, name: &str, query: &str, now_ms: i64) {
    self.saved_searches.insert(
      id,
      Stamped {
        value: SavedSearchEntry {
          name: name.to_string(),
          query: query.to_string(),
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
  }

  /// The user's backgrounds, oldest first (IDs grow with time).
  pub fn backgrounds(&self) -> Vec<(BackgroundId, &BackgroundEntry)> {
    let mut backgrounds: Vec<_> = self
      .backgrounds
      .iter()
      .filter(|(_, entry)| !entry.is_removed)
      .filter_map(|(id, entry)| Some((id.parse::<BackgroundId>().ok()?, &entry.value)))
      .collect();
    backgrounds.sort_by_key(|(id, _)| *id);
    backgrounds
  }

  pub fn background(&self, id: BackgroundId) -> Option<&BackgroundEntry> {
    self
      .backgrounds
      .get(&id.to_string())
      .filter(|entry| !entry.is_removed)
      .map(|entry| &entry.value)
  }

  pub fn add_background(&mut self, id: BackgroundId, entry: BackgroundEntry, now_ms: i64) {
    self.backgrounds.insert(
      id.to_string(),
      Stamped {
        value: entry,
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
  }

  /// Marks the background removed; returns whether it existed.
  pub fn remove_background(&mut self, id: BackgroundId, now_ms: i64) -> bool {
    match self.backgrounds.get_mut(&id.to_string()) {
      Some(entry) if !entry.is_removed => {
        entry.is_removed = true;
        entry.updated_at_ms = now_ms;
        true
      },
      _ => false,
    }
  }

  pub fn delete_saved_search(&mut self, id: u64, now_ms: i64) {
    if let Some(entry) = self.saved_searches.get_mut(&id) {
      entry.is_removed = true;
      entry.updated_at_ms = now_ms;
    }
  }
}

pub fn saved_key(conversation: ConversationId, message: ClientMessageId) -> String {
  let hex: String = message
    .as_bytes()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  format!("{conversation}/{hex}")
}

pub fn draft_key(conversation: ConversationId, maybe_thread: Option<ThreadId>) -> String {
  match maybe_thread {
    Some(thread) => format!("{conversation}/{thread}"),
    None => conversation.to_string(),
  }
}

/// Per key, the entry with the later change wins; on a tie the removal wins (it is the safer
/// outcome for personal data and keeps merging symmetric).
fn merge_map<K: Ord + Clone, T: Clone>(
  local: &mut BTreeMap<K, Stamped<T>>,
  remote: &BTreeMap<K, Stamped<T>>,
) {
  for (key, theirs) in remote {
    let is_newer = match local.get(key) {
      None => true,
      Some(ours) => {
        theirs.updated_at_ms > ours.updated_at_ms
          || (theirs.updated_at_ms == ours.updated_at_ms && theirs.is_removed && !ours.is_removed)
      },
    };
    if is_newer {
      local.insert(key.clone(), theirs.clone());
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn newer_changes_win_per_entry_in_both_directions() {
    let (conversation, message) = (id(1), ClientMessageId::from_bytes([7; 16]));
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    phone.set_saved(conversation, message, true, 100);
    laptop.set_draft(conversation, None, "half a thought", 150);
    laptop.set_saved(conversation, message, false, 200);
    phone.set_draft(conversation, None, "a later thought", 300);

    let mut merged_on_phone = phone.clone();
    merged_on_phone.merge(&laptop, 400);
    let mut merged_on_laptop = laptop.clone();
    merged_on_laptop.merge(&phone, 400);

    assert_eq!(merged_on_phone, merged_on_laptop, "merging is symmetric");
    assert!(
      !merged_on_phone.is_saved(conversation, message),
      "the later unsave wins"
    );
    assert_eq!(merged_on_phone.draft(conversation, None), Some("a later thought"));
  }

  #[test]
  fn removals_and_returns_merge_by_server_time_not_by_device_clock() {
    let key = "9/1/2";
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    // The laptop saw the device leave at 100 and come back at 200; the phone (whatever its clock)
    // saw it leave again at 300.
    assert!(laptop.note_removed_sender(key, 100));
    assert!(laptop.note_sender_back(key, 200));
    assert!(phone.note_removed_sender(key, 300));
    let mut merged = laptop.clone();
    merged.merge(&phone, 0);
    let mut merged_other_way = phone.clone();
    merged_other_way.merge(&laptop, 0);
    assert_eq!(merged, merged_other_way);
    assert_eq!(merged.removed_sender(key), Some(300));
    // Back after the last removal: a member again.
    assert!(merged.note_sender_back(key, 400));
    assert_eq!(merged.removed_sender(key), None);
  }

  mod pruned_senders {
    use super::*;

    const KEY: &str = "9/1/2";

    #[test]
    fn a_pruned_absence_is_not_taken_back_from_an_older_bag() {
      let mut older = AccountData::default();
      assert!(older.note_removed_sender(KEY, 100));
      assert!(older.note_sender_back(KEY, 200));
      // A device that missed the return still has only the removal.
      let mut stale = AccountData::default();
      assert!(stale.note_removed_sender(KEY, 100));

      let mut pruned = older.clone();
      assert!(pruned.prune_removed_senders(|key, _| key == KEY, 1_000));
      assert!(!pruned.removed_senders.contains_key(KEY));
      for other in [&older, &stale] {
        let mut merged = pruned.clone();
        merged.merge(other, 1_000);
        assert!(!merged.removed_senders.contains_key(KEY), "stays pruned");
        let mut merged_other_way = other.clone();
        merged_other_way.merge(&pruned, 1_000);
        assert_eq!(merged, merged_other_way, "merging is symmetric");
      }
      assert!(!pruned.note_removed_sender(KEY, 100), "the same absence");
    }

    #[test]
    fn a_later_removal_after_pruning_counts() {
      let mut data = AccountData::default();
      assert!(data.note_removed_sender(KEY, 100));
      assert!(data.prune_removed_senders(|_, _| true, 1_000));
      assert!(data.note_removed_sender(KEY, 300));
      assert_eq!(data.removed_sender(KEY), Some(300));
      let mut other = AccountData::default();
      other.merge(&data, 1_000);
      assert_eq!(other.removed_sender(KEY), Some(300));
    }

    #[test]
    fn only_picked_records_are_pruned() {
      let mut data = AccountData::default();
      assert!(data.note_removed_sender(KEY, 100));
      assert!(data.note_removed_sender("9/1/3", 100));
      assert!(!data.prune_removed_senders(|_, _| false, 1_000));
      assert!(data.prune_removed_senders(|key, _| key == KEY, 1_000));
      assert_eq!(data.removed_sender("9/1/3"), Some(100));
      assert!(data.pruned_senders.contains_key(KEY));
      assert!(!data.pruned_senders.contains_key("9/1/3"));
    }

    #[test]
    fn tombstones_are_forgotten_like_the_others() {
      let mut data = AccountData::default();
      assert!(data.note_removed_sender(KEY, 100));
      assert!(data.prune_removed_senders(|_, _| true, 1_000));
      data.forget_tombstones(1_000 + TOMBSTONE_LIFETIME_MS - 1);
      assert!(data.is_pruned_sender(KEY, 100));
      data.forget_tombstones(1_000 + TOMBSTONE_LIFETIME_MS);
      assert!(data.pruned_senders.is_empty());
    }
  }

  #[test]
  fn group_epoch_floors_only_grow_and_merge_to_the_higher_one() {
    let conversation = ConversationId::from_snowflake(Snowflake::new(9).unwrap());
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    assert!(phone.note_group_epoch(conversation, 7));
    assert!(!phone.note_group_epoch(conversation, 5), "never back");
    assert!(laptop.note_group_epoch(conversation, 4));
    laptop.merge(&phone, 0);
    phone.merge(&laptop, 0);
    assert_eq!(
      (laptop.group_epoch(conversation), phone.group_epoch(conversation)),
      (7, 7)
    );
  }

  #[test]
  fn the_activity_read_mark_only_moves_forward_and_merges_to_the_later_one() {
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    assert!(phone.mark_activity_read(500, 10));
    assert!(!phone.mark_activity_read(400, 11), "never back");
    assert!(!phone.mark_activity_read(500, 12), "unchanged");
    assert!(laptop.mark_activity_read(900, 20));

    let mut merged_on_phone = phone.clone();
    merged_on_phone.merge(&laptop, 30);
    let mut merged_on_laptop = laptop.clone();
    merged_on_laptop.merge(&phone, 30);
    assert_eq!(merged_on_phone, merged_on_laptop);
    assert_eq!(merged_on_phone.activity_read_at_ms(), 900);
    assert_eq!(AccountData::default().activity_read_at_ms(), 0);
  }

  #[test]
  fn the_later_passphrase_change_wins_and_survives_an_older_app() {
    let phone = AccountData {
      maybe_recovery_passphrase_set_at: Some(5_000),
      ..AccountData::default()
    };
    let laptop = AccountData {
      maybe_recovery_passphrase_set_at: Some(9_000),
      ..AccountData::default()
    };
    let mut merged = phone.clone();
    merged.merge(&laptop, 10_000);
    assert_eq!(merged.maybe_recovery_passphrase_set_at, Some(9_000));
    // An older app uploads the account data without the field.
    let mut older_upload: serde_json::Value = serde_json::from_slice(&merged.to_bytes().unwrap()).unwrap();
    older_upload
      .as_object_mut()
      .unwrap()
      .remove("recovery_passphrase_set_at");
    let older = AccountData::from_bytes(&serde_json::to_vec(&older_upload).unwrap()).unwrap();
    assert_eq!(older.maybe_recovery_passphrase_set_at, None);
    let mut kept = laptop.clone();
    kept.merge(&older, 10_000);
    assert_eq!(kept.maybe_recovery_passphrase_set_at, Some(9_000));
  }

  #[test]
  fn the_activity_read_mark_never_runs_ahead_of_the_clock() {
    let now = 1_000_000;
    let mut data = AccountData::default();
    // A message with a far-future (server-assigned) time must not mark everything read.
    assert!(data.mark_activity_read(i64::MAX, now));
    assert_eq!(data.activity_read_at_ms(), now + MAX_ACTIVITY_CLOCK_SKEW_MS);
    let from_elsewhere = AccountData {
      maybe_activity_read: Some(Stamped {
        value: i64::MAX,
        updated_at_ms: now,
        is_removed: false,
      }),
      ..AccountData::default()
    };
    let mut merged = AccountData::default();
    merged.merge(&from_elsewhere, now);
    assert_eq!(merged.activity_read_at_ms(), now + MAX_ACTIVITY_CLOCK_SKEW_MS);
  }

  #[test]
  fn clearing_a_draft_and_old_tombstones() {
    let conversation = id(1);
    let thread: ThreadId = id(9);
    let mut data = AccountData::default();
    assert!(data.set_draft(conversation, Some(thread), "reply", 10));
    assert!(
      !data.set_draft(conversation, Some(thread), "reply", 11),
      "unchanged"
    );
    assert!(data.set_draft(conversation, Some(thread), "", 12));
    assert_eq!(data.draft(conversation, Some(thread)), None);
    assert_eq!(data.drafts.len(), 1, "kept as a tombstone");
    data.forget_tombstones(12 + TOMBSTONE_LIFETIME_MS);
    assert!(data.drafts.is_empty());
  }

  #[test]
  fn saved_searches_and_encoding() {
    let mut data = AccountData::default();
    data.save_search(5, "Mine", "from:@me", 1);
    data.save_search(6, "Files", "has:file", 2);
    data.delete_saved_search(5, 3);
    assert_eq!(
      data
        .saved_searches()
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>(),
      vec![6]
    );
    let decoded = AccountData::from_bytes(&data.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, data);
    assert_eq!(AccountData::from_bytes(&[]).unwrap(), AccountData::default());
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
