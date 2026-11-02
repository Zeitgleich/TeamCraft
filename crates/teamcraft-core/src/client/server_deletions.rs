//! The server says a conversation was deleted (e2ee.md §4.6, "Deleting a channel"). That is its word
//! alone: nothing signs a deletion yet, and a device's local copy may be the last copy there is. So
//! the claim hides the conversation and drops its MLS secrets, as a removal does, but the messages stay
//! for `deleted_conversation_retention` and only then go. Only a deletion the user confirmed on this
//! device goes at once. Notes to self are never purged on a claim. Many claims at once are suspect,
//! and the user is told once. The server listing the conversation again lifts the quarantine.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, ConversationResponse};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageFilter, Table};

const QUARANTINE_PREFIX: &str = "quarantine/";
const OWN_DELETION_PREFIX: &str = "own_deletion/";
const NOTES_PREFIX: &str = "notes_to_self/";
pub(crate) const SUSPECT_DELETIONS_KEY: &str = "suspect_deletions";

/// A conversation the server claimed deleted, hidden here until its retention ran out.
#[derive(Debug, Serialize, Deserialize)]
struct Quarantined {
  conversation: ConversationId,
  since_ms: i64,
}

/// Many claims at once: what the user is told once.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SuspectDeletions {
  pub count: u32,
  pub at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The gateway's `conversation_delete`. A claim about a conversation the server still shows this
  /// user is forged or replayed, and changes nothing.
  pub(crate) async fn note_deletion_event(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !self.has_own_deletion(conversation)? {
      match self.api.conversation(conversation).await {
        Ok(response) if response.maybe_my_membership.is_some() => {
          tracing::warn!(conversation_id = %conversation, "the server claims a deletion of a conversation it still lists; ignored");
          return Ok(());
        },
        // Not reachable now: hiding it is safe, and the next sync lifts it if it was wrong.
        _ => {},
      }
    }
    self.note_server_deletion(conversation)
  }

  /// A deletion the server claims, from the gateway or a sync's `deleted_conversation_ids`. Replays
  /// change nothing: the retention runs from the first claim.
  pub(crate) fn note_server_deletion(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.has_own_deletion(conversation)? {
      return self.purge_conversation(conversation);
    }
    if self
      .store
      .get(Table::AppliedChanges, &notes_key(conversation))?
      .is_some()
    {
      tracing::warn!(conversation_id = %conversation, "the server claims the notes to self were deleted; they stay");
      return Ok(());
    }
    let key = quarantine_key(conversation);
    if self.store.get(Table::AppliedChanges, &key)?.is_some() {
      return Ok(());
    }
    let is_known = self
      .store
      .get(Table::Conversations, &conversation.to_string())?
      .is_some()
      || !self
        .store
        .messages(conversation, MessageFilter::TopLevel)?
        .is_empty();
    if !is_known {
      return Ok(());
    }
    let entry = Quarantined {
      conversation,
      since_ms: self.now_ms(),
    };
    records::put_json(&self.store, Table::AppliedChanges, &key, &entry)?;
    self.forget_conversation(conversation)?;
    self.drop_pending_work(conversation)?;
    tracing::info!(conversation_id = %conversation, "the server says this conversation was deleted; hidden, its messages stay for now");
    self.note_suspect_deletions()
  }

  /// Marks that the user confirmed deleting this conversation here, so the server's answer to it
  /// may purge at once. Set before the request: the answer can come on the gateway first.
  pub(crate) fn note_own_deletion(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .store
      .put(Table::AppliedChanges, &own_deletion_key(conversation), b"1")?;
    Ok(())
  }

  pub(crate) fn has_own_deletion(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self
        .store
        .get(Table::AppliedChanges, &own_deletion_key(conversation))?
        .is_some(),
    )
  }

  /// The server lists these conversations: a quarantine of them was wrong, or is over.
  pub(crate) fn lift_quarantines(&mut self, listed: &[ConversationResponse]) -> Result<(), CoreError> {
    for (key, _) in self.store.list(Table::AppliedChanges)? {
      let Some(id) = key.strip_prefix(QUARANTINE_PREFIX) else {
        continue;
      };
      if listed
        .iter()
        .any(|conversation| conversation.id.to_string() == id)
      {
        self.store.delete(Table::AppliedChanges, &key)?;
      }
    }
    Ok(())
  }

  /// Purges what stayed hidden past its retention. Run with a full sync.
  pub(crate) fn purge_expired_quarantines(&mut self) -> Result<(), CoreError> {
    let retention_ms =
      i64::try_from(self.settings.deleted_conversation_retention.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    for entry in self.quarantined()? {
      if now.saturating_sub(entry.since_ms) >= retention_ms {
        self.purge_conversation(entry.conversation)?;
      }
    }
    Ok(())
  }

  /// Keeps the fact that a conversation is the user's notes: the server can't take it back later by
  /// making the record go first.
  pub(crate) fn remember_notes_to_self(&self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = notes_key(conversation);
    if self.store.get(Table::AppliedChanges, &key)?.is_none() {
      self.store.put(Table::AppliedChanges, &key, b"1")?;
    }
    Ok(())
  }

  pub fn dismiss_suspect_deletions(&mut self) -> Result<(), CoreError> {
    Ok(self.store.delete(Table::AppliedChanges, SUSPECT_DELETIONS_KEY)?)
  }

  fn quarantined(&self) -> Result<Vec<Quarantined>, CoreError> {
    let mut found = Vec::new();
    for (key, bytes) in self.store.list(Table::AppliedChanges)? {
      if key.starts_with(QUARANTINE_PREFIX)
        && let Ok(entry) = serde_json::from_slice::<Quarantined>(&bytes)
      {
        found.push(entry);
      }
    }
    Ok(found)
  }

  /// Counts over a window, not per sync, so claims one by one on the gateway add up too.
  fn note_suspect_deletions(&mut self) -> Result<(), CoreError> {
    let window_ms = i64::try_from(self.settings.suspect_deletion_window.as_millis()).unwrap_or(i64::MAX);
    let now = self.now_ms();
    let recent = self
      .quarantined()?
      .iter()
      .filter(|entry| now.saturating_sub(entry.since_ms) <= window_ms)
      .count();
    let total = self
      .store
      .list(Table::Conversations)?
      .len()
      .saturating_add(recent);
    let is_suspect = recent > self.settings.suspect_deletion_count
      || (recent >= 2
        && recent.saturating_mul(100) > total.saturating_mul(self.settings.suspect_deletion_percent));
    if !is_suspect {
      return Ok(());
    }
    tracing::warn!(
      recent,
      total,
      "the server claims many conversations were deleted at once; kept hidden"
    );
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      SUSPECT_DELETIONS_KEY,
      &SuspectDeletions {
        count: u32::try_from(recent).unwrap_or(u32::MAX),
        at_ms: now,
      },
    )
  }
}

fn quarantine_key(conversation: ConversationId) -> String {
  format!("{QUARANTINE_PREFIX}{conversation}")
}

fn own_deletion_key(conversation: ConversationId) -> String {
  format!("{OWN_DELETION_PREFIX}{conversation}")
}

fn notes_key(conversation: ConversationId) -> String {
  format!("{NOTES_PREFIX}{conversation}")
}

/// The markers a purge removes with the conversation.
pub(crate) fn purge_markers(conversation: ConversationId) -> [String; 2] {
  [quarantine_key(conversation), own_deletion_key(conversation)]
}
