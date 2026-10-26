//! What members notice about a group that the server's word alone doesn't settle (e2ee.md §4.6,
//! "Removal order"): users still in the group whom the server no longer lists, and a server that
//! lists this user but refuses the group's queue. Neither makes this device sign or drop anything
//! on its own (security review A8). Leftovers are never shown to members (issue #661): only after
//! a fresh member list confirmed them twice are they recorded, for the admin console.

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

/// Fresh member lists that must agree before a user still in the group counts as a leftover.
const LEFTOVER_CONFIRMATIONS: u32 = 2;
const LEFTOVERS_PREFIX: &str = "leftovers/";

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

/// Users a check found in the group but not in the server's member list, waiting for the next
/// check to confirm them (a single mismatch is never reported).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LeftoverSuspects {
  users: Vec<UserId>,
  /// Fresh member lists that agreed so far.
  checks: u32,
}

/// That the server refused this device the group's queue while listing the user as a member.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct QueueRefusal {
  at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Notes who is in the group but not in the server's cached member list, leaving out removals
  /// already on their way (this device's own, or an admin's signed change waiting to be
  /// committed). The cache may be stale (a member who just joined, a missed event, issue #661), so
  /// this only marks them as suspects; [`Client::check_group_leftovers`] asks the server.
  pub(crate) fn note_group_leftovers(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let maybe_members: Option<ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    let (true, Some(members)) = (self.has_group(conversation)?, maybe_members) else {
      return self.clear_leftovers(conversation);
    };
    let candidates = self.unlisted_group_users(conversation, &members)?;
    if candidates.is_empty() {
      return self.clear_leftovers(conversation);
    }
    let mut suspects = self.leftover_suspects(conversation)?;
    for user in candidates {
      if !suspects.users.contains(&user) {
        suspects.users.push(user);
      }
    }
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &suspects_key(conversation),
      &suspects,
    )
  }

  /// Confirms suspected leftovers against a member list fetched now (never the cache). A user the
  /// server lists is dropped at once; one it doesn't list on two checks in a row is a leftover:
  /// meeting guests among them are removed with their own signed leaves, everybody else is
  /// recorded for the admin console and logged. Nobody is removed on the server's word alone.
  pub(crate) async fn check_group_leftovers(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    let suspects = self.leftover_suspects(conversation)?;
    if suspects.users.is_empty() || !self.has_group(conversation)? {
      return Ok(());
    }
    let members = self.api.list_members(conversation).await?;
    records::put_json(&self.store, Table::Members, &conversation.to_string(), &members)?;
    let unlisted = self.unlisted_group_users(conversation, &members)?;
    let still: Vec<UserId> = suspects
      .users
      .into_iter()
      .filter(|user| unlisted.contains(user))
      .collect();
    if still.is_empty() {
      return self.clear_leftovers(conversation);
    }
    // The guests' own leaves: safe on a fresh list, and they should go without waiting.
    self.queue_guest_removals(conversation, &still)?;
    let checks = suspects.checks.saturating_add(1);
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &suspects_key(conversation),
      &LeftoverSuspects {
        users: still.clone(),
        checks,
      },
    )?;
    if checks < LEFTOVER_CONFIRMATIONS {
      return Ok(());
    }
    if group_leftovers(&self.store, conversation)? != still {
      tracing::warn!(conversation_id = %conversation, count = still.len(), "users in the encryption group whom the server no longer lists");
    }
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &leftovers_key(conversation),
      &still,
    )
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

  /// Group users other than this one whom `members` doesn't list and whose removal isn't already
  /// on its way.
  fn unlisted_group_users(
    &mut self,
    conversation: ConversationId,
    members: &ListConversationMembersResponse,
  ) -> Result<Vec<UserId>, CoreError> {
    let listed: BTreeSet<UserId> = members.members.iter().map(|member| member.user.id).collect();
    let being_removed = self.removals_on_their_way(conversation)?;
    let me = self.account.user_id;
    Ok(
      self
        .group_users(conversation)?
        .into_iter()
        .filter(|user| *user != me && !listed.contains(user) && !being_removed.contains(user))
        .collect(),
    )
  }

  fn leftover_suspects(&self, conversation: ConversationId) -> Result<LeftoverSuspects, CoreError> {
    Ok(
      records::get_json(&self.store, Table::AppliedChanges, &suspects_key(conversation))?.unwrap_or_default(),
    )
  }

  fn clear_leftovers(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .store
      .delete(Table::AppliedChanges, &suspects_key(conversation))?;
    self
      .store
      .delete(Table::AppliedChanges, &leftovers_key(conversation))?;
    Ok(())
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

/// Users with a device in the conversation's group whom the server no longer lists, confirmed by
/// fresh member lists. Read by the admin console from the store; never shown to members.
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

/// Confirmed leftovers in every conversation this device is in, for the admin console.
pub fn all_group_leftovers(store: &impl LocalStore) -> Result<Vec<(ConversationId, UserId)>, CoreError> {
  let mut leftovers = Vec::new();
  for (key, users) in
    records::list_json_with_prefix::<Vec<UserId>>(store, Table::AppliedChanges, LEFTOVERS_PREFIX)?
  {
    let Some(Ok(conversation)) = key
      .strip_prefix(LEFTOVERS_PREFIX)
      .map(str::parse::<ConversationId>)
    else {
      continue;
    };
    leftovers.extend(users.into_iter().map(|user| (conversation, user)));
  }
  Ok(leftovers)
}

pub(crate) fn leftovers_key(conversation: ConversationId) -> String {
  format!("{LEFTOVERS_PREFIX}{conversation}")
}

fn suspects_key(conversation: ConversationId) -> String {
  format!("leftover_suspects/{conversation}")
}

pub(crate) fn queue_refused_key(conversation: ConversationId) -> String {
  format!("queue_refused/{conversation}")
}
