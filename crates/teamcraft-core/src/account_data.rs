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
use teamcraft_crypto::channel_anchor::AnchorFloor;
use teamcraft_proto::synced_settings::StoredSettings;
use teamcraft_proto::{
  BackgroundId, Base64Bytes, ClientMessageId, ConversationId, MessageId, ThreadId, UserId,
};

use crate::store::AttachmentRef;

/// Tombstones older than this are dropped when merging (every device has synced by then).
pub const TOMBSTONE_LIFETIME_MS: i64 = 90 * 24 * 60 * 60 * 1000;
/// How far the activity read mark may lie ahead of this device's clock.
const MAX_ACTIVITY_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
/// Characters of a draft the conversation lists carry (#1220).
const DRAFT_PREVIEW_CHARS: usize = 80;
/// Files of unsent drafts the account data carries (#1220). Each is a few hundred bytes and its
/// thumbnail; more stay on the device they were picked on until they are sent.
pub const MAX_DRAFT_FILES: usize = 30;
/// Largest thumbnail of a draft's file in the account data. A larger one stays on the device that
/// made it; the others show the file without a picture.
pub const MAX_DRAFT_THUMBNAIL_BYTES: usize = 6 * 1024;
/// Share of the server's key bag limit (`limits.max_key_bag_bytes`) the drafts' files may take, so
/// a key bag with them is never refused while keys, drafts' text and trust decisions fit.
const DRAFT_FILES_SHARE_DIVISOR: u64 = 4;
/// The key bag limit assumed of a server that publishes none (the server's default).
const DEFAULT_KEY_BAG_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountData {
  /// Saved messages, keyed by [`saved_key`].
  #[serde(default)]
  pub saved: BTreeMap<String, Stamped<SavedItem>>,
  /// Unsent text per conversation or thread, keyed by [`draft_key`]; an empty body is "no draft".
  #[serde(default)]
  pub drafts: BTreeMap<String, Stamped<Draft>>,
  /// Uploaded files of unsent drafts (#1220), keyed by their ID (`DraftFile::id`): the per-file key
  /// lives only here and in the message once sent. An older app drops the field on upload, and the
  /// next newer device puts it back.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub draft_files: BTreeMap<String, Stamped<DraftFile>>,
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
  /// Per conversation ID, the pin and floor of its channel anchor (e2ee.md §4.6.5, "The pin and
  /// the floor"): the founding the user's devices accepted first, the highest properties version
  /// with its hash and the highest epoch they saw it in. A new device, a reinstall and a recovery
  /// start from it, not from trust on first use. Merged by maximum; two foundings or two hashes for
  /// one version mark a fork, which grants nothing. An older app drops the field on upload, and the
  /// next newer device puts it back.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub anchor_floors: BTreeMap<String, AnchorFloor>,
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
  /// When one of the user's devices last made a recovery kit (e2ee.md §6.5), merged like
  /// `maybe_recovery_passphrase_set_at`.
  #[serde(
    rename = "recovery_key_set_at",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_recovery_key_set_at: Option<i64>,
  /// User settings that follow the account (issue #945), with their own merge rule: logical
  /// versions, no clock, and keys of newer clients are kept. An older app drops the field on
  /// upload, and the next newer device puts it back. An unreadable entry never fails the decoding
  /// of the rest (e2ee.md §6.4).
  #[serde(default, skip_serializing_if = "StoredSettings::is_empty")]
  pub settings: StoredSettings,
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

/// A file of an unsent draft, uploaded and held by the server for the draft (#1220): another device
/// of the user shows it in the composer and sends it without uploading it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftFile {
  pub conversation_id: ConversationId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  /// When it was attached (the attaching device's clock): the composer lists the files in order.
  pub added_at_ms: i64,
  /// The uploaded file with its key; its thumbnail only up to [`MAX_DRAFT_THUMBNAIL_BYTES`].
  pub attachment: AttachmentRef,
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
    merge_map(&mut self.draft_files, &other.draft_files);
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
    for (conversation, floor) in &other.anchor_floors {
      self
        .anchor_floors
        .entry(conversation.clone())
        .or_insert(*floor)
        .merge(floor);
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
    self.maybe_recovery_key_set_at = self
      .maybe_recovery_key_set_at
      .max(other.maybe_recovery_key_set_at);
    self.settings.merge(&other.settings);
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
      .draft_files
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

  /// The pin and floor of the conversation's channel anchor, `None` while none of the user's
  /// devices accepted one.
  pub fn anchor_floor(&self, conversation: ConversationId) -> Option<AnchorFloor> {
    self.anchor_floors.get(&conversation.to_string()).copied()
  }

  /// Takes this device's pin and floor of the conversation's anchor, merged by maximum into what
  /// the user's devices recorded; returns whether that changed anything.
  pub fn note_anchor_floor(&mut self, conversation: ConversationId, floor: &AnchorFloor) -> bool {
    match self.anchor_floors.get_mut(&conversation.to_string()) {
      Some(existing) => existing.merge(floor),
      None => {
        self.anchor_floors.insert(conversation.to_string(), *floor);
        true
      },
    }
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

  /// Drops what the user kept about a conversation that is gone for good: its drafts, saved
  /// messages and unread mark, on all devices. Whether anything changed.
  pub fn forget_conversation(&mut self, conversation: ConversationId, now_ms: i64) -> bool {
    let id = conversation.to_string();
    let prefix = format!("{conversation}/");
    let is_about_it = |key: &str| key == id || key.starts_with(&prefix);
    let mut has_changed = false;
    for (_, entry) in self
      .saved
      .iter_mut()
      .filter(|(key, entry)| is_about_it(key) && !entry.is_removed)
    {
      entry.is_removed = true;
      entry.updated_at_ms = now_ms;
      has_changed = true;
    }
    for (_, entry) in self
      .drafts
      .iter_mut()
      .filter(|(key, entry)| is_about_it(key) && !entry.is_removed)
    {
      entry.is_removed = true;
      entry.value.body = String::new();
      entry.updated_at_ms = now_ms;
      has_changed = true;
    }
    if let Some(entry) = self.marked_unread.get_mut(&id).filter(|entry| !entry.is_removed) {
      entry.is_removed = true;
      entry.updated_at_ms = now_ms;
      has_changed = true;
    }
    has_changed
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

  /// The drafts with something written or attached in them, each with when it was last edited,
  /// newest first (#1220). The edit time is the entry's stamp: it changes with the text only, and
  /// older clients keep it when they rewrite the record, which they wouldn't do for a new field of
  /// [`Draft`]. A draft of files only counts from its newest file.
  pub fn drafts_by_edit(&self) -> Vec<(Draft, i64)> {
    let mut drafts: BTreeMap<String, (Draft, i64)> = self
      .drafts
      .iter()
      .filter(|(_, entry)| !entry.is_removed && !entry.value.body.trim().is_empty())
      .map(|(key, entry)| (key.clone(), (entry.value.clone(), entry.updated_at_ms)))
      .collect();
    let mut file_only: BTreeMap<String, (Draft, i64)> = BTreeMap::new();
    for (_, file) in self.live_draft_files() {
      let key = draft_key(file.conversation_id, file.maybe_thread_id);
      if drafts.contains_key(&key) {
        continue;
      }
      let entry = file_only.entry(key).or_insert_with(|| {
        let draft = Draft {
          conversation_id: file.conversation_id,
          maybe_thread_id: file.maybe_thread_id,
          body: String::new(),
        };
        (draft, file.added_at_ms)
      });
      entry.1 = entry.1.max(file.added_at_ms);
    }
    drafts.extend(file_only);
    let mut drafts: Vec<(Draft, i64)> = drafts.into_values().collect();
    drafts.sort_by_key(|(_, edited_at)| std::cmp::Reverse(*edited_at));
    drafts
  }

  /// The start of the draft of the conversation's own composer, on one line, for its list entry.
  pub fn draft_preview(&self, conversation: ConversationId) -> Option<String> {
    let body = self.draft(conversation, None)?;
    let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
      return None;
    }
    if line.chars().count() <= DRAFT_PREVIEW_CHARS {
      return Some(line);
    }
    let mut preview: String = line.chars().take(DRAFT_PREVIEW_CHARS).collect();
    preview.push('…');
    Some(preview)
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

  /// The uploaded files of the draft, in the order they were attached, with their IDs.
  pub fn draft_files(
    &self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
  ) -> Vec<(&str, &DraftFile)> {
    let mut files: Vec<(&str, &DraftFile)> = self
      .live_draft_files()
      .filter(|(_, file)| file.conversation_id == conversation && file.maybe_thread_id == maybe_thread)
      .collect();
    files.sort_by_key(|(id, file)| (file.added_at_ms, *id));
    files
  }

  /// Files of all drafts, for the limit ([`MAX_DRAFT_FILES`]) and the drafts list.
  pub fn live_draft_files(&self) -> impl Iterator<Item = (&str, &DraftFile)> {
    self
      .draft_files
      .iter()
      .filter(|(_, entry)| !entry.is_removed)
      .map(|(id, entry)| (id.as_str(), &entry.value))
  }

  /// Adds an uploaded file to its draft; its thumbnail travels only when small enough. Returns
  /// whether it was added: not when the drafts already carry [`MAX_DRAFT_FILES`] or fill their share
  /// of the key bag ([`draft_files_budget`], the file then stays on this device), nor when it was
  /// removed meanwhile (a tombstone that is newer).
  pub fn add_draft_file(&mut self, id: &str, mut file: DraftFile, now_ms: i64, budget_bytes: u64) -> bool {
    if self.draft_files.get(id).is_some_and(|entry| entry.is_removed) {
      return false;
    }
    if !self.draft_files.contains_key(id) && self.live_draft_files().count() >= MAX_DRAFT_FILES {
      return false;
    }
    if file
      .attachment
      .maybe_thumbnail
      .as_ref()
      .is_some_and(|thumbnail| thumbnail.as_slice().len() > MAX_DRAFT_THUMBNAIL_BYTES)
    {
      file.attachment.maybe_thumbnail = None;
    }
    let entry = Stamped {
      value: file,
      updated_at_ms: now_ms,
      is_removed: false,
    };
    let used = self.draft_files_bytes(Some(id));
    if used.saturating_add(encoded_len(&entry)) > budget_bytes {
      // Without its picture, then not at all.
      let mut smaller = entry;
      smaller.value.attachment.maybe_thumbnail = None;
      if used.saturating_add(encoded_len(&smaller)) > budget_bytes {
        return false;
      }
      self.draft_files.insert(id.to_owned(), smaller);
      return true;
    }
    self.draft_files.insert(id.to_owned(), entry);
    true
  }

  /// Drops the thumbnails of the drafts' files, as the server refuses the key bag as too large (its
  /// limit was lowered). Returns whether there were any.
  pub fn shed_draft_thumbnails(&mut self, now_ms: i64) -> bool {
    let mut is_changed = false;
    for entry in self.draft_files.values_mut() {
      if entry.value.attachment.maybe_thumbnail.take().is_some() {
        entry.updated_at_ms = entry.updated_at_ms.max(now_ms);
        is_changed = true;
      }
    }
    is_changed
  }

  /// Bytes the drafts' files take in the encoded account data, tombstones included, without `except`.
  fn draft_files_bytes(&self, maybe_except: Option<&str>) -> u64 {
    self
      .draft_files
      .iter()
      .filter(|(id, _)| maybe_except != Some(id.as_str()))
      .map(|(id, entry)| encoded_len(entry).saturating_add(id.len() as u64))
      .sum()
  }

  /// Takes a file out of its draft (removed, or sent): returns it if it was there. The tombstone
  /// keeps another device's older copy from bringing it back.
  pub fn take_draft_file(&mut self, id: &str, now_ms: i64) -> Option<DraftFile> {
    let entry = self.draft_files.get_mut(id).filter(|entry| !entry.is_removed)?;
    entry.is_removed = true;
    entry.updated_at_ms = now_ms;
    Some(entry.value.clone())
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

/// What the drafts' files may take of the key bag, from the limit the server published.
pub fn draft_files_budget(maybe_key_bag_limit: Option<u64>) -> u64 {
  maybe_key_bag_limit.unwrap_or(DEFAULT_KEY_BAG_BYTES) / DRAFT_FILES_SHARE_DIVISOR
}

/// The size of `value` as the account data encodes it.
fn encoded_len<T: Serialize>(value: &T) -> u64 {
  serde_json::to_vec(value).map_or(u64::MAX, |bytes| bytes.len() as u64)
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

  mod settings {
    use serde_json::json;
    use teamcraft_proto::synced_settings::{SettingEntry, SyncedSettings};

    use super::*;

    #[test]
    fn they_merge_with_the_rest_of_the_account_data() {
      let mut mine = AccountData {
        settings: entry("theme", "dark", 2, 1).into(),
        ..AccountData::default()
      };
      let theirs = AccountData {
        settings: entry("theme", "light", 3, 2)
          .into_iter()
          .chain(entry("from.a.newer.client", "x", 1, 2))
          .collect::<SyncedSettings>()
          .into(),
        ..AccountData::default()
      };
      mine.merge(&theirs, NOW);
      assert_eq!(mine.settings["theme"].value, json!("light"));
      assert_eq!(mine.settings["from.a.newer.client"].value, json!("x"));
    }

    #[test]
    fn the_clock_of_the_merging_device_plays_no_part() {
      let theirs = AccountData {
        settings: entry("theme", "light", 3, 2).into(),
        ..AccountData::default()
      };
      for now_ms in [0, NOW, i64::MAX] {
        let mut mine = AccountData {
          settings: entry("theme", "dark", 2, 1).into(),
          ..AccountData::default()
        };
        mine.merge(&theirs, now_ms);
        assert_eq!(mine.settings["theme"].value, json!("light"));
      }
    }

    /// Nothing else in the key bag moves when settings are added, and bags without them read.
    #[test]
    fn storage_is_stable_and_absent_settings_are_left_out() {
      let data = AccountData {
        settings: entry("display.density", "compact", 3, 42).into(),
        ..AccountData::default()
      };
      let bytes = data.to_bytes().unwrap();
      let text = String::from_utf8(bytes.clone()).unwrap();
      assert!(
        text.contains(r#""settings":{"display.density":{"value":"compact","version":3,"writer":"42"}}"#),
        "{text}"
      );
      assert_eq!(AccountData::from_bytes(&bytes).unwrap(), data);
      let empty = AccountData::default().to_bytes().unwrap();
      assert!(!String::from_utf8(empty.clone()).unwrap().contains("settings"));
      assert!(AccountData::from_bytes(&empty).unwrap().settings.is_empty());
    }

    /// An app from before this field decodes without it and uploads without it. Newer devices keep
    /// theirs and upload them again, so the settings aren't lost.
    #[test]
    fn a_bag_from_an_older_app_does_not_take_the_settings_away() {
      let mut mine = AccountData {
        settings: entry("theme", "dark", 2, 1).into(),
        ..AccountData::default()
      };
      let from_old_app = AccountData::from_bytes(br#"{"saved":{}}"#).unwrap();
      mine.merge(&from_old_app, NOW);
      assert_eq!(mine.settings["theme"].value, json!("dark"));
      assert_ne!(mine, from_old_app, "so the merging device uploads again");
    }

    /// Settings travel with keys and pins: a broken one must not take those down with it.
    #[test]
    fn a_broken_setting_leaves_the_rest_of_the_account_data_readable() {
      let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
      let text = format!(
        r#"{{"group_epochs":{{"7":3}},"settings":{{"bad":{{"version":"x"}},"deep":{{"value":{deep},"version":1,"writer":"1"}},"theme":{{"value":"dark","version":1,"writer":"1"}}}}}}"#
      );
      let data = AccountData::from_bytes(text.as_bytes()).unwrap();
      assert_eq!(data.group_epochs["7"], 3);
      assert_eq!(data.settings["theme"].value, json!("dark"));
      assert_eq!(data.settings.opaque_count(), 2);
      assert_eq!(AccountData::from_bytes(&data.to_bytes().unwrap()).unwrap(), data);
    }

    const NOW: i64 = 1_800_000_000_000;

    fn entry(key: &str, value: &str, version: u64, writer: i64) -> SyncedSettings {
      [(
        key.to_owned(),
        SettingEntry {
          value: json!(value),
          version,
          writer: id(writer),
        },
      )]
      .into()
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

  /// e2ee.md §4.6.5: the pin and floor merge across two devices by maximum, survive the key bag's
  /// JSON, and two foundings mark a fork that grants nothing.
  #[test]
  fn anchor_floors_merge_across_devices_by_maximum() {
    use teamcraft_crypto::channel_anchor::{AnchorHash, FloorState};
    let conversation = ConversationId::from_snowflake(Snowflake::new(9).unwrap());
    let floor = |version, hash, epoch| AnchorFloor {
      founding_hash: AnchorHash::from_bytes([1; 32]),
      properties_version: version,
      properties_hash: AnchorHash::from_bytes([hash; 32]),
      epoch,
      state: FloorState::Agreed,
    };
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    assert!(phone.note_anchor_floor(conversation, &floor(2, 2, 10)));
    assert!(laptop.note_anchor_floor(conversation, &floor(1, 1, 30)));
    assert!(
      !laptop.note_anchor_floor(conversation, &floor(1, 1, 20)),
      "never back"
    );
    laptop.merge(&phone, 0);
    phone.merge(&laptop, 0);
    let merged = floor(2, 2, 30);
    assert_eq!(phone.anchor_floor(conversation), Some(merged));
    assert_eq!(laptop.anchor_floor(conversation), Some(merged));
    let json = serde_json::to_string(&phone).unwrap();
    assert_eq!(serde_json::from_str::<AccountData>(&json).unwrap(), phone);
    assert!(
      !serde_json::to_string(&AccountData::default())
        .unwrap()
        .contains("anchor_floors"),
      "no field without floors"
    );

    let mut other = floor(2, 2, 30);
    other.founding_hash = AnchorHash::from_bytes([9; 32]);
    let mut forked = AccountData::default();
    forked.note_anchor_floor(conversation, &other);
    phone.merge(&forked, 0);
    let after = phone.anchor_floor(conversation).unwrap();
    assert_eq!(after.state, FloorState::ForkedFounding);
    assert!(!after.grants());
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
  fn the_later_recovery_kit_wins() {
    let phone = AccountData {
      maybe_recovery_key_set_at: Some(5_000),
      ..AccountData::default()
    };
    let laptop = AccountData {
      maybe_recovery_key_set_at: Some(9_000),
      ..AccountData::default()
    };
    let mut merged = laptop.clone();
    merged.merge(&phone, 10_000);
    assert_eq!(merged.maybe_recovery_key_set_at, Some(9_000));
    let json: serde_json::Value = serde_json::from_slice(&merged.to_bytes().unwrap()).unwrap();
    assert_eq!(json["recovery_key_set_at"], 9_000);
    let without: serde_json::Value =
      serde_json::from_slice(&AccountData::default().to_bytes().unwrap()).unwrap();
    assert!(without.get("recovery_key_set_at").is_none());
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
  fn drafts_are_listed_by_their_last_edit_without_empty_or_cleared_ones() {
    let mut data = AccountData::default();
    let (first, second, third) = (id(1), id(2), id(3));
    let thread: ThreadId = id(5);
    data.set_draft(first, None, "older", 10);
    data.set_draft(second, Some(thread), "newer", 20);
    data.set_draft(third, None, "   \n ", 30);
    data.set_draft(first, None, "older", 40);
    let listed: Vec<(String, i64)> = data
      .drafts_by_edit()
      .into_iter()
      .map(|(draft, edited_at)| (draft.body, edited_at))
      .collect();
    assert_eq!(
      listed,
      [("newer".to_owned(), 20), ("older".to_owned(), 10)],
      "saving the same text is no edit"
    );
    data.set_draft(second, Some(thread), "", 50);
    assert_eq!(data.drafts_by_edit().len(), 1);
  }

  #[test]
  fn the_list_entry_shows_the_start_of_the_conversations_own_draft() {
    let mut data = AccountData::default();
    let conversation = id(1);
    assert_eq!(data.draft_preview(conversation), None);
    data.set_draft(conversation, Some(id(5)), "in a thread", 10);
    assert_eq!(
      data.draft_preview(conversation),
      None,
      "the thread lists mark that one"
    );
    data.set_draft(conversation, None, "  first\n\n second  ", 20);
    assert_eq!(data.draft_preview(conversation).as_deref(), Some("first second"));
    data.set_draft(conversation, None, &"x".repeat(200), 30);
    let preview = data.draft_preview(conversation).unwrap();
    assert_eq!(preview.chars().count(), DRAFT_PREVIEW_CHARS + 1);
    assert!(preview.ends_with('…'));
    data.set_draft(conversation, None, "   ", 40);
    assert_eq!(data.draft_preview(conversation), None);
  }

  #[test]
  fn a_draft_record_of_an_older_client_lists_with_its_stamp_as_the_edit_time() {
    let older =
      r#"{"drafts":{"1":{"value":{"conversation_id":"1","body":"written earlier"},"updated_at_ms":77}}}"#;
    let data = AccountData::from_bytes(older.as_bytes()).unwrap();
    assert_eq!(data.drafts_by_edit()[0].1, 77);
    let rewritten = AccountData::from_bytes(&data.to_bytes().unwrap()).unwrap();
    assert_eq!(rewritten, data, "the format did not change");
  }

  mod draft_files {
    use super::*;

    const BUDGET: u64 = 256 * 1024;

    #[test]
    fn the_drafts_files_stay_within_their_share_of_the_key_bag() {
      assert_eq!(draft_files_budget(None), 256 * 1024);
      assert_eq!(draft_files_budget(Some(64 * 1024)), 16 * 1024);
      let mut data = AccountData::default();
      let budget = draft_files_budget(Some(36 * 1024));
      for (index, name) in ["first", "second"].into_iter().enumerate() {
        let file = draft_file(id(1), index as i64, MAX_DRAFT_THUMBNAIL_BYTES);
        assert!(data.add_draft_file(name, file, 10, budget));
      }
      let thumbnails = data
        .live_draft_files()
        .filter(|(_, file)| file.attachment.maybe_thumbnail.is_some())
        .count();
      assert_eq!(thumbnails, 1, "the second one travels without its picture");
      assert!(data.draft_files_bytes(None) <= budget);
      assert!(
        !data.add_draft_file("none", draft_file(id(1), 3, 10), 10, 300),
        "no room at all"
      );
      assert!(data.shed_draft_thumbnails(20));
      assert!(
        data
          .live_draft_files()
          .all(|(_, file)| file.attachment.maybe_thumbnail.is_none())
      );
      assert!(!data.shed_draft_thumbnails(30), "nothing left to drop");
    }

    #[test]
    fn a_drafts_files_list_in_order_and_a_draft_of_files_only_is_a_draft() {
      let mut data = AccountData::default();
      let conversation = id(1);
      assert!(data.add_draft_file("b", draft_file(conversation, 20, 10), 100, BUDGET));
      assert!(data.add_draft_file("a", draft_file(conversation, 30, 10), 100, BUDGET));
      let order: Vec<&str> = data
        .draft_files(conversation, None)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
      assert_eq!(order, ["b", "a"], "by when they were attached");
      assert!(
        data.draft_files(conversation, Some(id(5))).is_empty(),
        "the thread's draft is another"
      );
      let listed = data.drafts_by_edit();
      assert_eq!(listed.len(), 1);
      assert_eq!(
        (listed[0].0.body.as_str(), listed[0].1),
        ("", 30),
        "edited with its newest file"
      );
    }

    #[test]
    fn the_drafts_carry_a_bounded_number_of_files_and_only_small_thumbnails() {
      let mut data = AccountData::default();
      for index in 0..MAX_DRAFT_FILES {
        assert!(data.add_draft_file(&index.to_string(), draft_file(id(1), 1, 10), 100, BUDGET));
      }
      assert!(!data.add_draft_file("one more", draft_file(id(1), 1, 10), 100, BUDGET));
      data.take_draft_file("0", 200);
      assert!(data.add_draft_file(
        "one more",
        draft_file(id(1), 1, MAX_DRAFT_THUMBNAIL_BYTES + 1),
        300,
        BUDGET
      ));
      let (_, added) = data
        .draft_files(id(1), None)
        .into_iter()
        .find(|(id, _)| *id == "one more")
        .unwrap();
      assert_eq!(added.attachment.maybe_thumbnail, None, "too large to travel");
    }

    #[test]
    fn a_file_taken_out_on_one_device_stays_out_after_merging() {
      let mut phone = AccountData::default();
      phone.add_draft_file("f", draft_file(id(1), 1, 10), 100, BUDGET);
      let mut laptop = phone.clone();
      assert!(phone.take_draft_file("f", 200).is_some());
      assert!(phone.take_draft_file("f", 210).is_none(), "taken once");
      laptop.merge(&phone, 300);
      assert!(laptop.draft_files(id(1), None).is_empty());
      assert!(
        !laptop.add_draft_file("f", draft_file(id(1), 1, 10), 400, BUDGET),
        "a late upload doesn't bring it back"
      );
    }

    #[test]
    fn account_data_without_drafts_files_keeps_its_format() {
      let data = AccountData::default();
      assert!(
        !String::from_utf8(data.to_bytes().unwrap())
          .unwrap()
          .contains("draft_files")
      );
    }

    fn draft_file(conversation: ConversationId, added_at_ms: i64, thumbnail_bytes: usize) -> DraftFile {
      DraftFile {
        conversation_id: conversation,
        maybe_thread_id: None,
        added_at_ms,
        attachment: AttachmentRef {
          attachment_id: id(77),
          key: Base64Bytes::new(vec![1; 32]),
          ciphertext_sha256: Base64Bytes::new(vec![2; 32]),
          file_name: "notes.txt".into(),
          media_type: "text/plain".into(),
          size_bytes: 4,
          maybe_thumbnail: Some(Base64Bytes::new(vec![3; thumbnail_bytes])),
          maybe_dimensions: None,
          maybe_duration_ms: None,
          maybe_waveform: None,
        },
      }
    }
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
