//! Coming back costs what changed (issue #714): a full sync first asks the server where each
//! conversation and space stands (`GET /sync/heads`, one request) and compares that with the heads
//! this device caught up to last time. Only conversations whose newest message or MLS entry moved,
//! or whose member list changed, are read again; the rest cost no request at all.
//!
//! The server's word is used only to skip reads, never to trust anything: a head that lies makes
//! this device miss changes until the next gateway event or a head that moves, as if the server had
//! withheld them, which it can do anyway. Whatever is read is verified as before.
//!
//! What this device caught up to is kept in one [`Table::SyncState`] record. A head is noted only
//! for a read that started after the heads were read, so a change in between is read next time.
//!
//! Between full syncs the gateway's events move the heads too (issue #778), so a busy conversation
//! isn't read again at the next fresh session only because people kept writing in it. An event's
//! entry says nothing about the entries before it, so only conversations this device had without a
//! hole when the sync ended are followed: their heads equalled the server's then, and from then on
//! every event reached this device. That holds across a resumed session (the server refuses a
//! resume with a gap and replays the rest), a failed event (it isn't counted as applied and comes
//! again) and a restart (what was followed is kept with the heads). A conversation not followed
//! keeps its old head, which can only cost a read.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use teamcraft_crypto::mls::StoredGroupState;
use teamcraft_proto::{
  ConversationHead, ConversationId, MessageId, MlsMessageId, SpaceId, SyncHeadsResponse, UserId,
};

use super::Client;
use super::sync::ResyncPart;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The heads this device caught up to, in [`Table::SyncState`].
const SEEN_HEADS_KEY: &str = "seen_heads";
/// The head of a followed conversation as the gateway's events moved it since the last full sync,
/// in [`Table::SyncState`]: `live_head/<conversation>`, one small record per event instead of the
/// whole workspace's heads.
const LIVE_HEAD_PREFIX: &str = "live_head/";

/// The heads of the sync in progress and what this device caught up to before.
#[derive(Debug, Default)]
pub(super) struct HeadsInSync {
  fresh: HashMap<ConversationId, ConversationHead>,
  fresh_spaces: HashMap<SpaceId, String>,
  seen: SeenHeads,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SeenHeads {
  #[serde(default)]
  conversations: BTreeMap<ConversationId, SeenConversation>,
  #[serde(default)]
  spaces: BTreeMap<SpaceId, SeenSpace>,
  /// The conversations whose content head equalled the server's when the last full sync ended:
  /// the gateway's events move their heads (see the module docs).
  #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
  followed: BTreeSet<ConversationId>,
}

/// The heads the gateway's events move between full syncs (issue #778).
#[derive(Debug, Default)]
pub(super) enum LiveHeads {
  /// Not read from the store yet in this process: read at the first event.
  #[default]
  Unloaded,
  /// A full sync runs: what it reads decides what is followed next.
  Paused,
  /// The followed conversations and the newest entries this device has of each.
  Following(BTreeMap<ConversationId, ContentHead>),
}

/// The entry a gateway event brought into a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LiveEntry {
  Message(MessageId),
  Mls(MlsMessageId),
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SeenConversation {
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_members_digest: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_content: Option<ContentHead>,
}

/// Where a conversation's history and MLS queue stood when this device caught them up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ContentHead {
  maybe_last_message_id: Option<MessageId>,
  maybe_last_mls_message_id: Option<MlsMessageId>,
}

/// A space's member list as caught up: the people it named, for the people lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SeenSpace {
  members_digest: String,
  members: Vec<UserId>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads the heads for the full sync that starts. A server without the endpoint (older than
  /// #714) leaves them unknown: then everything is read, as before.
  pub(super) fn begin_heads(&mut self, read: Result<SyncHeadsResponse, ApiError>) -> Result<(), CoreError> {
    self.maybe_heads = None;
    // What the sync reads decides what is followed after it; meanwhile events move no head.
    self.live_heads = LiveHeads::Paused;
    let response = match read {
      Ok(response) => response,
      Err(error) if error.is_transient() || error.is_sign_in_required() => return Err(error.into()),
      Err(error) => {
        // An older server without the endpoint (404), or one that failed to answer it: the sync
        // reads everything, as before.
        tracing::debug!(%error, "no sync heads; reading every conversation");
        return Ok(());
      },
    };
    let mut seen: SeenHeads =
      records::get_json(&self.store, Table::SyncState, SEEN_HEADS_KEY)?.unwrap_or_default();
    for (conversation, live) in self.stored_live_heads()? {
      seen.move_followed(conversation, &live);
    }
    seen.followed.clear();
    self.maybe_heads = Some(HeadsInSync {
      fresh: response
        .conversations
        .into_iter()
        .map(|head| (head.conversation_id, head))
        .collect(),
      fresh_spaces: response
        .spaces
        .into_iter()
        .map(|head| (head.space_id, head.members_digest))
        .collect(),
      seen,
    });
    Ok(())
  }

  /// Keeps what this sync caught up to and ends the comparison: reads after the sync (events,
  /// the upkeep) note nothing, their heads weren't read before them.
  pub(super) fn finish_heads(&mut self) -> Result<(), CoreError> {
    let Some(mut heads) = self.maybe_heads.take() else {
      return Ok(());
    };
    let fresh_spaces = &heads.fresh_spaces;
    heads
      .seen
      .spaces
      .retain(|space, _| fresh_spaces.contains_key(space));
    let following: BTreeMap<ConversationId, ContentHead> = heads
      .seen
      .conversations
      .iter()
      .filter_map(|(conversation, seen)| {
        let fresh = content_head(heads.fresh.get(conversation)?);
        (seen.maybe_content.as_ref() == Some(&fresh)).then_some((*conversation, fresh))
      })
      .collect();
    heads.seen.followed = following.keys().copied().collect();
    records::put_json(&self.store, Table::SyncState, SEEN_HEADS_KEY, &heads.seen)?;
    // The heads just kept include what the events moved; a record left by a crash in between is
    // merged again next time, and the newer head wins.
    for (key, _) in self.store.list_prefix(Table::SyncState, LIVE_HEAD_PREFIX)? {
      self.store.delete(Table::SyncState, &key)?;
    }
    self.live_heads = LiveHeads::Following(following);
    Ok(())
  }

  /// A gateway event brought `entry` into `conversation` and was applied: a followed
  /// conversation's head moves to it (issue #778).
  pub(super) fn note_live_entry(
    &mut self,
    conversation: ConversationId,
    entry: LiveEntry,
  ) -> Result<(), CoreError> {
    if matches!(self.live_heads, LiveHeads::Unloaded) {
      self.live_heads = LiveHeads::Following(self.load_live_heads()?);
    }
    let LiveHeads::Following(following) = &mut self.live_heads else {
      return Ok(());
    };
    let Some(head) = following.get_mut(&conversation) else {
      return Ok(());
    };
    if !head.take(entry) {
      return Ok(());
    }
    records::put_json(&self.store, Table::SyncState, &live_head_key(conversation), head)
  }

  /// The conversation's local history and group are gone: its head is no longer followed until a
  /// full sync caught it up again.
  pub(super) fn stop_following(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if let LiveHeads::Following(following) = &mut self.live_heads {
      following.remove(&conversation);
    } else if matches!(self.live_heads, LiveHeads::Unloaded) {
      let mut following = self.load_live_heads()?;
      following.remove(&conversation);
      self.live_heads = LiveHeads::Following(following);
    }
    // The record of the followed set still names it until the next full sync, which then finds no
    // local history or group and reads the conversation anyway.
    self
      .store
      .delete(Table::SyncState, &live_head_key(conversation))?;
    Ok(())
  }

  /// The followed conversations as the last full sync left them and the events moved them since.
  fn load_live_heads(&self) -> Result<BTreeMap<ConversationId, ContentHead>, CoreError> {
    let mut seen: SeenHeads =
      records::get_json(&self.store, Table::SyncState, SEEN_HEADS_KEY)?.unwrap_or_default();
    for (conversation, live) in self.stored_live_heads()? {
      seen.move_followed(conversation, &live);
    }
    Ok(
      seen
        .followed
        .iter()
        .filter_map(|conversation| {
          let content = seen.conversations.get(conversation)?.maybe_content.clone()?;
          Some((*conversation, content))
        })
        .collect(),
    )
  }

  fn stored_live_heads(&self) -> Result<Vec<(ConversationId, ContentHead)>, CoreError> {
    let mut heads = Vec::new();
    for (key, value) in self.store.list_prefix(Table::SyncState, LIVE_HEAD_PREFIX)? {
      let Some(conversation) = key.strip_prefix(LIVE_HEAD_PREFIX).and_then(|id| id.parse().ok()) else {
        continue;
      };
      match serde_json::from_slice(&value) {
        Ok(head) => heads.push((conversation, head)),
        // Unreadable: the old head stays, which costs a read at most.
        Err(error) => {
          tracing::debug!(%error, conversation_id = %conversation, "unreadable live head ignored")
        },
      }
    }
    Ok(heads)
  }

  /// The newest message of `conversation` this sync's heads name: where the server stood when the
  /// sync started (lines about changes the server reports, issue #818).
  pub(super) fn head_message(&self, conversation: ConversationId) -> Option<MessageId> {
    self
      .maybe_heads
      .as_ref()?
      .fresh
      .get(&conversation)?
      .maybe_last_message_id
  }

  /// Forgets the heads of conversations the user is no longer in.
  pub(super) fn keep_listed_heads(&mut self, listed: impl IntoIterator<Item = ConversationId>) {
    if let Some(heads) = &mut self.maybe_heads {
      let listed: BTreeSet<ConversationId> = listed.into_iter().collect();
      heads
        .seen
        .conversations
        .retain(|conversation, _| listed.contains(conversation));
    }
  }

  /// Whether this sync must read the conversation's member list.
  pub(super) fn are_members_due(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let Some(heads) = &self.maybe_heads else {
      return Ok(true);
    };
    let (Some(fresh), Some(seen)) = (
      heads.fresh.get(&conversation),
      heads.seen.conversations.get(&conversation),
    ) else {
      return Ok(true);
    };
    if self.is_marked_for_resync(conversation, ResyncPart::Members)? {
      return Ok(true);
    }
    Ok(seen.maybe_members_digest.as_deref() != Some(fresh.members_digest.as_str()))
  }

  /// Whether this sync must read the conversation's MLS queue and history: their heads moved, or
  /// there is local work only a read finishes (no group yet, a commit of this device not landed, a
  /// request to be added back, messages waiting for keys, the first page of history, an earlier
  /// failure, an import not loaded yet, new messages an earlier catch-up left, issue #1423). A
  /// conversation that needs nothing counts as caught up for the outbox (its queue holds nothing
  /// newer, and what comes now arrives as gateway events).
  pub(super) fn is_content_due(
    &mut self,
    conversation: ConversationId,
    parked: &BTreeSet<ConversationId>,
  ) -> Result<bool, CoreError> {
    let Some(heads) = &self.maybe_heads else {
      return Ok(true);
    };
    let (Some(fresh), Some(seen)) = (
      heads.fresh.get(&conversation),
      heads.seen.conversations.get(&conversation),
    ) else {
      return Ok(true);
    };
    let is_unchanged = seen.maybe_content.as_ref() == Some(&content_head(fresh))
      && seen.maybe_members_digest.as_deref() == Some(fresh.members_digest.as_str());
    // The group's stored state, not the group: loading every group costs more than the rest of an
    // unchanged sync.
    let maybe_group = self.stored_group_state(conversation)?;
    if !is_unchanged
      || parked.contains(&conversation)
      || maybe_group.is_none()
      || maybe_group == Some(StoredGroupState::PendingCommit)
      || !self.is_history_loaded(conversation)?
      || self.is_unfinished_dm_founding(conversation)?
      || self.has_requested_rejoin(conversation)?
      || self.is_marked_for_resync(conversation, ResyncPart::Content)?
      || self.unloaded_import(conversation)?.is_some()
      || self.has_backlog(conversation)?
    {
      return Ok(true);
    }
    self.mls_current.insert(conversation);
    Ok(false)
  }

  /// The conversations with messages waiting for keys.
  /// Read from the keys ("<conversation>/<client message ID>"), without the parked ciphertext.
  pub(super) fn parked_conversations(&self) -> Result<BTreeSet<ConversationId>, CoreError> {
    Ok(
      self
        .store
        .list(Table::Undecrypted)?
        .into_iter()
        .filter_map(|(key, _)| key.split_once('/')?.0.parse().ok())
        .collect(),
    )
  }

  /// The member list just stored was read during this sync.
  pub(super) fn note_members_seen(&mut self, conversation: ConversationId) {
    if let Some(heads) = &mut self.maybe_heads
      && let Some(fresh) = heads.fresh.get(&conversation)
    {
      heads
        .seen
        .conversations
        .entry(conversation)
        .or_default()
        .maybe_members_digest = Some(fresh.members_digest.clone());
    }
  }

  /// The conversation's queue and history were caught up during this sync.
  pub(super) fn note_content_seen(&mut self, conversation: ConversationId) {
    if let Some(heads) = &mut self.maybe_heads
      && let Some(fresh) = heads.fresh.get(&conversation)
    {
      heads
        .seen
        .conversations
        .entry(conversation)
        .or_default()
        .maybe_content = Some(content_head(fresh));
    }
  }

  /// The people a space's member list named when it was last read, if it is unchanged since.
  pub(super) fn unchanged_space_members(&self, space: SpaceId) -> Option<Vec<UserId>> {
    let heads = self.maybe_heads.as_ref()?;
    let fresh = heads.fresh_spaces.get(&space)?;
    heads
      .seen
      .spaces
      .get(&space)
      .filter(|seen| &seen.members_digest == fresh)
      .map(|seen| seen.members.clone())
  }

  /// Whether the server gave heads but none for this space: it doesn't let the caller list its
  /// members (a guest), so there is nothing to read.
  pub(super) fn is_space_unlisted(&self, space: SpaceId) -> bool {
    self
      .maybe_heads
      .as_ref()
      .is_some_and(|heads| !heads.fresh_spaces.contains_key(&space))
  }

  pub(super) fn note_space_seen(&mut self, space: SpaceId, members: Vec<UserId>) {
    if let Some(heads) = &mut self.maybe_heads
      && let Some(fresh) = heads.fresh_spaces.get(&space)
    {
      let seen = SeenSpace {
        members_digest: fresh.clone(),
        members,
      };
      heads.seen.spaces.insert(space, seen);
    }
  }
}

impl SeenHeads {
  /// A followed conversation's head as the events moved it; the newer entries win, so a record
  /// older than the head kept changes nothing.
  fn move_followed(&mut self, conversation: ConversationId, live: &ContentHead) {
    if !self.followed.contains(&conversation) {
      return;
    }
    if let Some(content) = self
      .conversations
      .get_mut(&conversation)
      .and_then(|seen| seen.maybe_content.as_mut())
    {
      content.maybe_last_message_id = content.maybe_last_message_id.max(live.maybe_last_message_id);
      content.maybe_last_mls_message_id = content
        .maybe_last_mls_message_id
        .max(live.maybe_last_mls_message_id);
    }
  }
}

impl ContentHead {
  /// Moves the head to `entry` if it is newer; whether it moved.
  fn take(&mut self, entry: LiveEntry) -> bool {
    match entry {
      LiveEntry::Message(id) => take_newer(&mut self.maybe_last_message_id, id),
      LiveEntry::Mls(id) => take_newer(&mut self.maybe_last_mls_message_id, id),
    }
  }
}

fn take_newer<T: Ord + Copy>(slot: &mut Option<T>, id: T) -> bool {
  let is_newer = slot.is_none_or(|last| id > last);
  if is_newer {
    *slot = Some(id);
  }
  is_newer
}

fn live_head_key(conversation: ConversationId) -> String {
  format!("{LIVE_HEAD_PREFIX}{conversation}")
}

fn content_head(head: &ConversationHead) -> ContentHead {
  ContentHead {
    maybe_last_message_id: head.maybe_last_message_id,
    maybe_last_mls_message_id: head.maybe_last_mls_message_id,
  }
}
