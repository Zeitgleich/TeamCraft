//! What members notice about a group that the server's word alone doesn't settle (e2ee.md §4.6,
//! "Removal order"): users still in the group whom the server no longer lists, and a server that
//! lists this user but refuses the group's queue. Neither makes this device sign or drop anything
//! on its own (security review A8); both are recorded for the UI, where an admin decides.

use std::collections::BTreeSet;

use teamcraft_crypto::membership::{MembershipAction, SignedMembershipChange};
use teamcraft_proto::{ConversationId, ListConversationMembersResponse, UserId};

use super::Client;
use super::workspace::{PendingChange, RemovalState, RemovalStep};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// How a removal or leave this device started stands, for the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalStatus {
  /// On its way: waiting for the members' commit, the network or the next try.
  Pending,
  /// Refused too often; waits for the user's "retry".
  Failed,
  /// An admin's removal from outside the group that can't be confirmed (the server lists the
  /// user again, or somebody may have added them back): waits for "remove" or "keep".
  Unconfirmed,
}

/// That the server refused this device the group's queue while listing the user as a member.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct QueueRefusal {
  at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Users with a device in the group whom the server no longer lists, as last noted.
  pub(crate) fn group_leftovers(&self, conversation: ConversationId) -> Result<Vec<UserId>, CoreError> {
    group_leftovers(&self.store, conversation)
  }

  /// Notes who is in the group but not in the server's (cached) member list, leaving out
  /// removals already on their way (this device's own, or an admin's signed change waiting to be
  /// committed). A colleague's removal whose signed change got lost or refused shows up here, so
  /// an admin can sign it again; nobody signs it on the server's word (security review A8).
  pub(crate) fn note_group_leftovers(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let key = leftovers_key(conversation);
    let maybe_members: Option<ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    let (true, Some(members)) = (self.has_group(conversation)?, maybe_members) else {
      self.store.delete(Table::AppliedChanges, &key)?;
      return Ok(());
    };
    let listed: BTreeSet<UserId> = members.members.iter().map(|member| member.user.id).collect();
    let being_removed = self.removals_on_their_way(conversation)?;
    let me = self.account.user_id;
    let leftovers: Vec<UserId> = self
      .group_users(conversation)?
      .into_iter()
      .filter(|user| *user != me && !listed.contains(user) && !being_removed.contains(user))
      .collect();
    if leftovers.is_empty() {
      self.store.delete(Table::AppliedChanges, &key)?;
      return Ok(());
    }
    self.queue_guest_removals(conversation, &leftovers)?;
    if group_leftovers(&self.store, conversation)? != leftovers {
      tracing::warn!(conversation_id = %conversation, count = leftovers.len(), "users in the encryption group whom the server no longer lists");
    }
    records::put_json(&self.store, Table::AppliedChanges, &key, &leftovers)
  }

  /// The server refused the group's queue (403/404). Quiet only when that is expected: this
  /// device's own leave is waiting for the members' commit, or the server says the user is no
  /// longer a member at all (the conversation goes, as on the server's removal event, which the
  /// user sees). Otherwise a server could make a member stop following the group without anybody
  /// noticing; that is logged and shown instead, and the group isn't marked caught up.
  pub(crate) async fn note_queue_refused(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.has_own_pending_leave(conversation)? {
      tracing::debug!(conversation_id = %conversation, "the server stopped serving the queue of a group this device is leaving");
      return Ok(());
    }
    match self.api.conversation(conversation).await {
      Err(error) if error.is_not_found() || error.is_forbidden() => {
        tracing::info!(conversation_id = %conversation, "the server no longer lists this user in the conversation");
        self.forget_conversation(conversation)
      },
      Err(error) => Err(error.into()),
      Ok(_) => {
        tracing::warn!(conversation_id = %conversation, "the server lists this user as a member but refuses the group's queue");
        records::put_json(
          &self.store,
          Table::AppliedChanges,
          &queue_refused_key(conversation),
          &QueueRefusal { at_ms: self.now_ms() },
        )
      },
    }
  }

  /// The group's queue was read again: an earlier refusal is over.
  pub(crate) fn clear_queue_refused(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .store
      .delete(Table::AppliedChanges, &queue_refused_key(conversation))?;
    Ok(())
  }

  /// Removed from the conversation: the group is gone for this device; local history stays
  /// readable.
  pub(crate) fn forget_conversation(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.drop_group(conversation)?;
    self
      .store
      .delete(Table::Conversations, &conversation.to_string())?;
    self.store.delete(Table::Members, &conversation.to_string())?;
    Ok(())
  }

  /// Whether this device's user is leaving the conversation (a pending leave of this device).
  pub(crate) fn has_own_pending_leave(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let key = PendingChange::removal_key(conversation, self.account.user_id);
    Ok(self.stored_pending(&key)?.is_some())
  }

  /// Users whose removal from the group is under way: this device's own removals, and admins'
  /// signed removals waiting in this device's queue to be committed.
  fn removals_on_their_way(&self, conversation: ConversationId) -> Result<BTreeSet<UserId>, CoreError> {
    let mut users = BTreeSet::new();
    for change in records::list_json::<PendingChange>(&self.store, Table::PendingChanges)? {
      match change {
        PendingChange::Removal {
          conversation_id,
          user_id,
          ..
        } if conversation_id == conversation => {
          users.insert(user_id);
        },
        PendingChange::Delegated {
          conversation_id,
          change,
        } if conversation_id == conversation => {
          // Unverified here; it is only a reason not to ask the admin yet.
          if let Ok(signed) = SignedMembershipChange::from_bytes(change.as_slice())
            && let MembershipAction::RemoveUser { user_id } = signed.unverified().action()
          {
            users.insert(convert::user_from_crypto(*user_id)?);
          }
        },
        _ => {},
      }
    }
    Ok(users)
  }
}

/// Users with a device in the conversation's group whom the server no longer lists (as the last
/// sync noted them). Read by the UI from the store; shown to admins.
pub fn group_leftovers(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Vec<UserId>, CoreError> {
  Ok(records::get_json(store, Table::AppliedChanges, &leftovers_key(conversation))?.unwrap_or_default())
}

/// Whether the server lists the user in the conversation but refuses this device the group's
/// queue, so new messages may be missing. Read by the UI from the store.
pub fn is_queue_refused(store: &impl LocalStore, conversation: ConversationId) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::AppliedChanges, &queue_refused_key(conversation))?
      .is_some(),
  )
}

/// Removals and leaves this device started in the conversation that still show: on their way,
/// failed or unconfirmed. An admin's removal from outside the group that is only watched doesn't.
pub fn pending_removals(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Vec<(UserId, RemovalStatus)>, CoreError> {
  let mut removals = Vec::new();
  for change in records::list_json::<PendingChange>(store, Table::PendingChanges)? {
    if let PendingChange::Removal {
      conversation_id,
      user_id,
      step,
      state,
      ..
    } = change
      && conversation_id == conversation
      && (state == RemovalState::Unconfirmed || !matches!(step, RemovalStep::Watch { .. }))
    {
      let status = match state {
        RemovalState::Retrying => RemovalStatus::Pending,
        RemovalState::Failed => RemovalStatus::Failed,
        RemovalState::Unconfirmed => RemovalStatus::Unconfirmed,
      };
      removals.push((user_id, status));
    }
  }
  Ok(removals)
}

/// Unconfirmed removals in every conversation, for the admin console (an admin outside the group
/// doesn't see the conversation).
pub fn unconfirmed_removals(store: &impl LocalStore) -> Result<Vec<(ConversationId, UserId)>, CoreError> {
  Ok(
    records::list_json::<PendingChange>(store, Table::PendingChanges)?
      .into_iter()
      .filter_map(|change| match change {
        PendingChange::Removal {
          conversation_id,
          user_id,
          state: RemovalState::Unconfirmed,
          ..
        } => Some((conversation_id, user_id)),
        _ => None,
      })
      .collect(),
  )
}

pub(crate) fn leftovers_key(conversation: ConversationId) -> String {
  format!("leftovers/{conversation}")
}

pub(crate) fn queue_refused_key(conversation: ConversationId) -> String {
  format!("queue_refused/{conversation}")
}
