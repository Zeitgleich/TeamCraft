//! The UI's snapshot of the workspace, kept per conversation (issue #246).
//!
//! Building the whole [`ChatSnapshot`] reads every conversation's messages, members and people:
//! work that grows with the workspace. The [`SnapshotCache`] keeps the parts it built, re-reads
//! only what the store's writes touched ([`StoreChanges`]) and answers a reader with the parts that
//! differ from what it read last ([`SnapshotChanges`]). A new message costs one conversation's
//! re-read and an answer with one entry; a presence event one person's entry and the live state.

use std::collections::{BTreeMap, BTreeSet};

use teamcraft_client_api::backend::ChatBackend;
use teamcraft_client_api::live::LiveSnapshot;
use teamcraft_client_api::model::{ConversationId, Space, SpaceId, User, UserId};
use teamcraft_client_api::remote::{ChatSnapshot, SecuritySnapshot};
use teamcraft_client_api::security::{KeyBackupState, RecoveryMethod};
use teamcraft_client_api::snapshot_changes::{
  ConversationChange, ConversationIndex, SnapshotChanges, SnapshotCursor,
};
use teamcraft_crypto::backup::WrapMethod;
use teamcraft_proto::{
  ConversationKind as ConversationKindProto, ConversationResponse, MessageId, UserSummary,
};

use crate::account_data::AccountData;
use crate::client::{WRAPPED_KEYS_KEY, decode_wrapped_keys, stored_account_data};
use crate::error::CoreError;
use crate::notify_rules::MentionPreferences;
use crate::read_state::PresenceIndex;
use crate::records;
use crate::store::{LocalStore, StoreChanges, Table};
use crate::ui_backend::{CoreChatBackend, is_deleted};

/// The parts of the snapshot as last built, each with the revision it last changed at.
pub struct SnapshotCache {
  /// Tells readers of an earlier cache (another tab's core, a restarted shell) to start over.
  epoch: u64,
  revision: u64,
  maybe_parts: Option<Parts>,
  /// The presence log by revision, so a reader gets what is newer than its cursor without a pass
  /// over the whole log on every presence change.
  presence: PresenceIndex,
}

struct Parts {
  me: Revised<User>,
  users: BTreeMap<UserId, Revised<User>>,
  /// When somebody last left the list of people.
  user_ids_at: u64,
  spaces: Revised<Vec<Space>>,
  conversations: BTreeMap<ConversationId, Revised<ConversationChange>>,
  /// When each conversation's messages were last written, entry changed or not: a thread reply
  /// or an edit leaves the entry as it was but changes what the reader shows of the feed.
  feeds: BTreeMap<ConversationId, u64>,
  order: Revised<Vec<ConversationId>>,
  security: Revised<SecuritySnapshot>,
  /// When the live state (counts, threads, presence log, receipts) was last written.
  live_at: u64,
  /// When muted conversations' mutes end by themselves: their entries change then without a
  /// write, so they are read again once the time has come.
  mute_ends: BTreeMap<ConversationId, i64>,
}

#[derive(Clone, Debug, PartialEq)]
struct Revised<T> {
  value: T,
  at: u64,
}

/// What every conversation entry reads, read once per refresh.
struct Shared {
  users: BTreeMap<UserId, UserSummary>,
  preferences: MentionPreferences,
  account: AccountData,
}

impl SnapshotCache {
  /// `epoch` must differ between caches a reader could meet in turn (the time of creation will do).
  pub fn new(epoch: u64) -> Self {
    Self {
      epoch,
      revision: 0,
      maybe_parts: None,
      presence: PresenceIndex::default(),
    }
  }

  /// The whole snapshot, after taking `changes`.
  pub fn snapshot<S: LocalStore>(
    &mut self,
    backend: &CoreChatBackend<S>,
    changes: StoreChanges,
    security: impl FnOnce() -> Option<SecuritySnapshot>,
  ) -> ChatSnapshot {
    self.refresh(backend, changes, security);
    match &self.maybe_parts {
      Some(parts) => parts.snapshot(backend.live_snapshot()),
      // Invariant: `refresh` always leaves parts; a fresh read is the same thing anyway.
      None => ChatSnapshot::read(backend, SecuritySnapshot::default()),
    }
  }

  /// Takes `changes` and answers a reader at `maybe_since` with what differs: everything when the
  /// reader has nothing of this cache (`None`, or another epoch).
  pub fn changes_since<S: LocalStore>(
    &mut self,
    backend: &CoreChatBackend<S>,
    changes: StoreChanges,
    security: impl FnOnce() -> Option<SecuritySnapshot>,
    maybe_since: Option<SnapshotCursor>,
  ) -> SnapshotChanges {
    self.refresh(backend, changes, security);
    let (epoch, revision) = (self.epoch, self.revision);
    let Some(parts) = &self.maybe_parts else {
      return SnapshotChanges::default();
    };
    let Some(since) = maybe_since.filter(|since| since.epoch == epoch) else {
      let live = backend.live_snapshot();
      let presence = live.as_ref().map_or(0, |live| live.presence.revision);
      return SnapshotChanges {
        cursor: SnapshotCursor {
          epoch,
          revision,
          presence,
        },
        maybe_full: Some(Box::new(parts.snapshot(live))),
        ..SnapshotChanges::default()
      };
    };
    let mut answer = parts.changed_since(since.revision);
    let mut presence = since.presence;
    if parts.live_at > since.revision {
      let live = live_since(backend, &self.presence, since.presence);
      presence = live.presence.revision;
      answer.maybe_live = Some(live);
    }
    answer.cursor = SnapshotCursor {
      epoch,
      revision,
      presence,
    };
    answer
  }

  /// Re-reads what `changes` touched (everything the first time) under the next revision, which
  /// becomes current if anything differs. The security part is asked for only when it may have
  /// changed; `None` (a sync holds the client) keeps what is known.
  fn refresh<S: LocalStore>(
    &mut self,
    backend: &CoreChatBackend<S>,
    changes: StoreChanges,
    security: impl FnOnce() -> Option<SecuritySnapshot>,
  ) {
    let next = self.revision + 1;
    backend.read(
      self
        .presence
        .update(backend.store_ref(), changes.announced.iter().copied()),
    );
    let maybe_security = if self.maybe_parts.is_none() || changes.is_security {
      security()
    } else {
      None
    };
    let mut is_changed = match &mut self.maybe_parts {
      Some(parts) if !changes.is_everything => parts.update(backend, &changes, next),
      Some(parts) => parts.replace_with(Parts::read(backend, next), changes.is_live, next),
      None => {
        self.maybe_parts = Some(Parts::read(backend, next));
        true
      },
    };
    if let Some(parts) = &mut self.maybe_parts {
      is_changed |= parts.note_feeds(&changes.conversations, next);
    }
    if let (Some(parts), Some(security)) = (&mut self.maybe_parts, maybe_security) {
      is_changed |= parts.security.set(security, next);
    }
    if is_changed {
      self.revision = next;
    }
  }
}

impl Parts {
  /// Everything but the security part (the host's), read at `revision`.
  fn read<S: LocalStore>(backend: &CoreChatBackend<S>, revision: u64) -> Self {
    let shared = Shared::read(backend, backend.summaries());
    let spaces = backend.spaces();
    let responses = backend.conversations_raw();
    let order = order_of(&spaces, &responses);
    let by_id: BTreeMap<ConversationId, &ConversationResponse> =
      responses.iter().map(|response| (response.id, response)).collect();
    let mut mute_ends = BTreeMap::new();
    let conversations = order
      .iter()
      .filter_map(|id| by_id.get(id))
      .map(|response| {
        if let Some(end) = backend.mute_end(response) {
          mute_ends.insert(response.id, end);
        }
        (
          response.id,
          Revised::new(part_of(backend, response, &shared), revision),
        )
      })
      .collect();
    let users = shared
      .users
      .values()
      .map(|summary| (summary.id, Revised::new(backend.to_user(summary), revision)))
      .collect();
    Self {
      me: Revised::new(backend.me(), revision),
      users,
      user_ids_at: revision,
      spaces: Revised::new(spaces, revision),
      conversations,
      feeds: BTreeMap::new(),
      order: Revised::new(order, revision),
      security: Revised::new(SecuritySnapshot::default(), revision),
      live_at: revision,
      mute_ends,
    }
  }

  /// Takes a full re-read, keeping the revision of every part that came out the same. The security
  /// part stays (the caller's).
  fn replace_with(&mut self, fresh: Self, is_live: bool, revision: u64) -> bool {
    let mut is_changed = self.me.set(fresh.me.value, revision);
    let has_left = self.users.keys().any(|user| !fresh.users.contains_key(user));
    if has_left {
      self.user_ids_at = revision;
      is_changed = true;
    }
    let mut users = BTreeMap::new();
    for (id, user) in fresh.users {
      let mut part = self
        .users
        .remove(&id)
        .unwrap_or_else(|| Revised::new(user.value.clone(), revision));
      is_changed |= part.set(user.value, revision) || part.at == revision;
      users.insert(id, part);
    }
    self.users = users;
    is_changed |= self.spaces.set(fresh.spaces.value, revision);
    let mut conversations = BTreeMap::new();
    for (id, conversation) in fresh.conversations {
      let mut part = self
        .conversations
        .remove(&id)
        .unwrap_or_else(|| Revised::new(conversation.value.clone(), revision));
      is_changed |= part.set(conversation.value, revision) || part.at == revision;
      conversations.insert(id, part);
    }
    self.conversations = conversations;
    self.feeds.retain(|id, _| self.conversations.contains_key(id));
    is_changed |= self.order.set(fresh.order.value, revision);
    self.mute_ends = fresh.mute_ends;
    if is_live {
      self.live_at = revision;
      is_changed = true;
    }
    is_changed
  }

  /// Notes that the messages of `conversations` were written at `revision`; returns whether any of
  /// them is a known conversation (a reader then has feeds to re-read).
  fn note_feeds(&mut self, conversations: &BTreeSet<ConversationId>, revision: u64) -> bool {
    let mut is_noted = false;
    for conversation in conversations {
      if self.conversations.contains_key(conversation) {
        self.feeds.insert(*conversation, revision);
        is_noted = true;
      }
    }
    is_noted
  }

  /// Re-reads only the conversations and people `changes` name.
  fn update<S: LocalStore>(
    &mut self,
    backend: &CoreChatBackend<S>,
    changes: &StoreChanges,
    revision: u64,
  ) -> bool {
    let mut is_changed = false;
    for user in &changes.users {
      let maybe_summary: Option<UserSummary> = backend.read(records::get_json(
        backend.store_ref(),
        Table::Users,
        &user.to_string(),
      ));
      let Some(summary) = maybe_summary else {
        continue;
      };
      let view = backend.to_user(&summary);
      if *user == backend.me_id() {
        is_changed |= self.me.set(view.clone(), revision);
      }
      match self.users.get_mut(user) {
        Some(part) => is_changed |= part.set(view, revision),
        None => {
          self.users.insert(*user, Revised::new(view, revision));
          is_changed = true;
        },
      }
    }
    let now = backend.now_ms();
    let unmuted = self
      .mute_ends
      .iter()
      .filter(|(_, end)| **end <= now)
      .map(|(conversation, _)| *conversation);
    let known: BTreeSet<ConversationId> = changes
      .conversations
      .iter()
      .copied()
      .chain(unmuted)
      .filter(|conversation| self.conversations.contains_key(conversation))
      .collect();
    if !known.is_empty() {
      let shared = Shared::read(backend, BTreeMap::new());
      for conversation in known {
        let maybe_response: Option<ConversationResponse> = backend.read(records::get_json(
          backend.store_ref(),
          Table::Conversations,
          &conversation.to_string(),
        ));
        let (Some(response), Some(part)) = (maybe_response, self.conversations.get_mut(&conversation)) else {
          continue;
        };
        let shared = shared.with_people_of(backend, &response);
        is_changed |= part.set(part_of(backend, &response, &shared), revision);
        match backend.mute_end(&response) {
          Some(end) => self.mute_ends.insert(conversation, end),
          None => self.mute_ends.remove(&conversation),
        };
      }
    }
    if changes.is_live {
      self.live_at = revision;
      is_changed = true;
    }
    is_changed
  }

  fn snapshot(&self, maybe_live: Option<LiveSnapshot>) -> ChatSnapshot {
    let entries: Vec<&ConversationChange> = self
      .order
      .value
      .iter()
      .filter_map(|id| self.conversations.get(id))
      .map(|part| &part.value)
      .collect();
    ChatSnapshot {
      me: self.me.value.clone(),
      users: self.users.values().map(|part| part.value.clone()).collect(),
      spaces: self.spaces.value.clone(),
      conversations: entries.iter().map(|entry| entry.conversation.clone()).collect(),
      message_counts: entries
        .iter()
        .map(|entry| (entry.conversation.id, entry.message_count))
        .collect(),
      last_activity: entries
        .iter()
        .filter_map(|entry| entry.maybe_last_activity.map(|at| (entry.conversation.id, at)))
        .collect(),
      security: self.security.value.clone(),
      maybe_live,
      index: ConversationIndex::default(),
    }
  }

  /// Every part that changed after `revision` (the live state is the caller's).
  fn changed_since(&self, revision: u64) -> SnapshotChanges {
    let is_newer = |at: u64| at > revision;
    SnapshotChanges {
      maybe_me: is_newer(self.me.at).then(|| self.me.value.clone()),
      users: self
        .users
        .values()
        .filter(|part| is_newer(part.at))
        .map(|part| part.value.clone())
        .collect(),
      maybe_user_ids: is_newer(self.user_ids_at).then(|| self.users.keys().copied().collect()),
      maybe_spaces: is_newer(self.spaces.at).then(|| self.spaces.value.clone()),
      conversations: self
        .order
        .value
        .iter()
        .filter_map(|id| self.conversations.get(id))
        .filter(|part| is_newer(part.at))
        .map(|part| part.value.clone())
        .collect(),
      // Those whose entry is sent anyway are re-read for it.
      feeds: self
        .feeds
        .iter()
        .filter(|(id, at)| {
          is_newer(**at) && self.conversations.get(*id).is_none_or(|part| !is_newer(part.at))
        })
        .map(|(id, _)| *id)
        .collect(),
      maybe_order: is_newer(self.order.at).then(|| self.order.value.clone()),
      maybe_security: is_newer(self.security.at).then(|| self.security.value.clone()),
      ..SnapshotChanges::default()
    }
  }
}

impl<T: PartialEq> Revised<T> {
  fn new(value: T, at: u64) -> Self {
    Self { value, at }
  }

  /// Takes `value`; returns whether it differs (then it changed at `revision`).
  fn set(&mut self, value: T, revision: u64) -> bool {
    if self.value == value {
      return false;
    }
    self.value = value;
    self.at = revision;
    true
  }
}

impl Shared {
  fn read<S: LocalStore>(backend: &CoreChatBackend<S>, users: BTreeMap<UserId, UserSummary>) -> Self {
    Self {
      users,
      preferences: backend.mention_preferences(),
      account: backend.read(crate::client::stored_account_data(backend.store_ref())),
    }
  }

  /// With the people one conversation's entry names: its members and the user (mentions).
  fn with_people_of<S: LocalStore>(
    &self,
    backend: &CoreChatBackend<S>,
    response: &ConversationResponse,
  ) -> Self {
    let mut users = BTreeMap::new();
    for user in backend.members(response.id).into_iter().chain([backend.me_id()]) {
      let maybe_summary: Option<UserSummary> = backend.read(records::get_json(
        backend.store_ref(),
        Table::Users,
        &user.to_string(),
      ));
      if let Some(summary) = maybe_summary {
        users.insert(user, summary);
      }
    }
    Self {
      users,
      preferences: self.preferences.clone(),
      account: self.account.clone(),
    }
  }
}

/// One conversation's entry, count and newest message, from one read of its messages.
fn part_of<S: LocalStore>(
  backend: &CoreChatBackend<S>,
  response: &ConversationResponse,
  shared: &Shared,
) -> ConversationChange {
  let top_level = backend.top_level(response.id);
  let shown = top_level.iter().filter(|message| !is_deleted(message));
  let message_count = shown.clone().count();
  let maybe_last_activity = shown.map(|message| message.sent_at_ms).max();
  let maybe_mark: Option<MessageId> = shared.account.unread_mark(response.id);
  let unread = backend.unread_among(response, maybe_mark, top_level);
  ConversationChange {
    conversation: backend.conversation_view(response, &shared.users, &shared.preferences, &unread),
    message_count,
    maybe_last_activity,
  }
}

/// The conversations in the order the snapshot lists them: each space's channels, then direct and
/// group messages (like [`ChatSnapshot::read`]).
fn order_of(spaces: &[Space], responses: &[ConversationResponse]) -> Vec<ConversationId> {
  let in_space = |space: SpaceId| {
    responses
      .iter()
      .filter(move |response| response.maybe_space_id == Some(space))
  };
  let direct = responses.iter().filter(|response| {
    matches!(
      response.kind,
      ConversationKindProto::Dm | ConversationKindProto::Group
    )
  });
  spaces
    .iter()
    .flat_map(|space| in_space(space.id))
    .chain(direct)
    .map(|response| response.id)
    .collect()
}

/// The live state with only the presence announcements newer than `presence`.
fn live_since<S: LocalStore>(
  backend: &CoreChatBackend<S>,
  index: &PresenceIndex,
  presence: u64,
) -> LiveSnapshot {
  let store = backend.store_ref();
  LiveSnapshot {
    maybe_unread: backend.read(crate::read_state::unread(store)),
    threads: backend.read(crate::read_state::threads(store)),
    presence: backend.read(index.since(store, presence)),
    receipts: backend.read(crate::read_state::all_read_receipts(store)),
    marks: backend.read(crate::read_state::marks(store)),
  }
}

/// The key backup as the engine last stored it (e2ee.md §6, issue #410), for the security part of
/// the snapshot. `None` while this device doesn't know the wraps yet (before its first key bag
/// sync).
pub fn key_backup_state(store: &impl LocalStore) -> Result<Option<KeyBackupState>, CoreError> {
  let Some(blob) =
    records::get_json::<teamcraft_proto::Base64Bytes>(store, Table::SyncState, WRAPPED_KEYS_KEY)?
  else {
    return Ok(None);
  };
  let sync = records::load_sync_state(store)?;
  let mut recovery = Vec::new();
  for wrapped in decode_wrapped_keys(blob.as_slice())? {
    let maybe_method = match wrapped.method() {
      WrapMethod::Passphrase => Some(RecoveryMethod::Passphrase),
      WrapMethod::RecoveryKey => Some(RecoveryMethod::RecoveryKey),
      WrapMethod::PasskeyPrf => Some(RecoveryMethod::Passkey),
      WrapMethod::OrgRecovery => Some(RecoveryMethod::Organization),
      // The device's own wrap restores nothing on another device.
      WrapMethod::DeviceKeystore => None,
    };
    if let Some(method) = maybe_method
      && !recovery.contains(&method)
    {
      recovery.push(method);
    }
  }
  let account_data = stored_account_data(store)?;
  let maybe_passphrase_set_at = account_data
    .maybe_recovery_passphrase_set_at
    .filter(|_| recovery.contains(&RecoveryMethod::Passphrase));
  let maybe_recovery_key_set_at = account_data
    .maybe_recovery_key_set_at
    .filter(|_| recovery.contains(&RecoveryMethod::RecoveryKey));
  Ok(Some(KeyBackupState {
    is_stored: sync.key_bag_version > 0,
    has_pending_upload: sync.is_key_bag_dirty,
    recovery,
    maybe_passphrase_set_at,
    maybe_recovery_key_set_at,
    is_passphrase_allowed: crate::workspace::is_passphrase_allowed(store),
  }))
}
