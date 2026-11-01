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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
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
struct ContentHead {
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
    let seen = records::get_json(&self.store, Table::SyncState, SEEN_HEADS_KEY)?.unwrap_or_default();
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
    records::put_json(&self.store, Table::SyncState, SEEN_HEADS_KEY, &heads.seen)
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
  /// failure, an import not loaded yet). A conversation that needs nothing counts as caught up for the outbox (its queue
  /// holds nothing newer, and what comes now arrives as gateway events).
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
    if !is_unchanged
      || parked.contains(&conversation)
      || !self.has_group(conversation)?
      || !self.is_history_loaded(conversation)?
      || self.is_unfinished_dm_founding(conversation)?
      || self.has_pending_commit(conversation)?
      || self.has_requested_rejoin(conversation)?
      || self.is_marked_for_resync(conversation, ResyncPart::Content)?
      || self.unloaded_import(conversation)?.is_some()
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

fn content_head(head: &ConversationHead) -> ContentHead {
  ContentHead {
    maybe_last_message_id: head.maybe_last_message_id,
    maybe_last_mls_message_id: head.maybe_last_mls_message_id,
  }
}
