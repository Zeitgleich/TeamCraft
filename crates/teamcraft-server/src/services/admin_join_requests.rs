//! Admins asking the owners of a private channel to let them in (admin.md §2.4, issue #776).
//!
//! With `admin_self_join = owner_approval` members' clients refuse an admin's own signed self-join,
//! so the admin asks instead. The server only stores the request and tells the channel's owners
//! (members with the channel role `admin`); it decides nothing that matters for the encryption.
//! An owner's client approves by adding the admin with its own MLS commit, which members' clients
//! check like any other addition, and posts the "joined as a workspace admin" message.

use std::collections::{BTreeSet, HashMap};

use serde_json::json;
use teamcraft_db::{AdminJoinRequest, AdminJoinRequestOutcome};
use teamcraft_proto::{
  AddConversationMemberRequest, AdminJoinRequestChange, AdminJoinRequestResponse, AdminSelfJoin, ChannelRole,
  ConversationId, ConversationMemberResponse, ConversationVisibility, GatewayEvent,
  ListAdminJoinRequestsResponse, UserId,
};
use tracing::info;

use super::access::{ConversationAccess, is_guest, is_instance_admin, load_conversation};
use super::audit::{self, Target as AuditTarget};
use super::conversations::ensure_open_channel;
use super::convert::{unix_millis, user_summary};
use super::members;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Most requests one answer lists; open requests are few (admins × private channels they ask for).
const LIST_LIMIT: i64 = 200;
/// Expired requests closed per round of the housekeeping.
const SWEEP_BATCH: i64 = 500;

/// The caller, a workspace admin, asks the owners of the private channel `conversation` to let them
/// in. Asking again while a request is open answers with that request and tells nobody again.
pub async fn request(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<AdminJoinRequestResponse, ApiError> {
  if !is_instance_admin(caller) || is_guest(caller) {
    return Err(ApiError::Forbidden(
      "only workspace admins ask to join private channels".into(),
    ));
  }
  match state.policy.get().admin_self_join {
    AdminSelfJoin::OwnerApproval => {},
    AdminSelfJoin::Visible => {
      return Err(ApiError::Conflict(
        "the workspace policy lets admins join private channels on their own".into(),
      ));
    },
    AdminSelfJoin::Disabled | AdminSelfJoin::Unknown(_) => {
      return Err(ApiError::Forbidden(
        "the workspace policy does not let admins join private channels".into(),
      ));
    },
  }
  let access = load_conversation(state, caller, conversation).await?;
  ensure_open_channel(&access)?;
  ensure_joinable(state, caller, &access).await?;
  let days = state.settings.get().policy.admin_join_request_days;
  let stored = match state
    .database
    .open_admin_join_request(conversation, caller.user.id, days)
    .await?
  {
    AdminJoinRequestOutcome::AlreadyOpen(stored) => stored,
    AdminJoinRequestOutcome::Created(stored) => {
      audit::record(
        state,
        Some(caller.user.id),
        "admin_join_requested",
        Some(AuditTarget::new("conversation", conversation)),
        &json!({}),
      )
      .await?;
      announce(
        state,
        conversation,
        caller.user.id,
        AdminJoinRequestChange::Requested,
      )
      .await?;
      stored
    },
  };
  Ok(response(state, &stored, &caller.user))
}

/// The open requests the caller decides on (channels they own) and their own.
pub async fn list(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListAdminJoinRequestsResponse, ApiError> {
  let stored = state
    .database
    .list_admin_join_requests_for(caller.user.id, LIST_LIMIT)
    .await?;
  let ids: Vec<UserId> = stored
    .iter()
    .map(|request| request.user)
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect();
  let users: HashMap<UserId, teamcraft_db::User> = state
    .database
    .get_users(&ids)
    .await?
    .into_iter()
    .map(|user| (user.id, user))
    .collect();
  let requests = stored
    .iter()
    .filter_map(|request| {
      users
        .get(&request.user)
        .map(|user| response(state, request, user))
    })
    .collect();
  Ok(ListAdminJoinRequestsResponse { requests })
}

/// An owner of the channel lets the admin in: the server membership now (the owner's client
/// commits the MLS add and announces the join next). Answers the membership also when the admin
/// is in already, so a retry after a lost answer goes on to the commit.
pub async fn approve(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  user: UserId,
) -> Result<ConversationMemberResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  ensure_owner(&access)?;
  ensure_open_channel(&access)?;
  if let Some(member) = state
    .database
    .list_conversation_members(conversation)
    .await?
    .into_iter()
    .find(|member| member.user.id == user)
  {
    return Ok(ConversationMemberResponse {
      user: user_summary(state, &member.user),
      role: member.role,
    });
  }
  if state
    .database
    .admin_join_request(conversation, user)
    .await?
    .is_none()
  {
    return Err(ApiError::admin_join_request_not_found());
  }
  // The addition closes the request ([`resolve_on_join`]).
  members::add_member(
    state,
    caller,
    conversation,
    AddConversationMemberRequest {
      user_id: user,
      maybe_role: None,
    },
  )
  .await
}

/// Declines the request (an owner of the channel) or withdraws it (the admin who asked).
/// Idempotent: a request that is gone already (decided on another device, expired) answers `Ok`.
pub async fn close(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  user: UserId,
) -> Result<(), ApiError> {
  let is_withdrawal = user == caller.user.id;
  if !is_withdrawal {
    let access = load_conversation(state, caller, conversation).await?;
    ensure_owner(&access)?;
  }
  if state
    .database
    .take_admin_join_request(conversation, user)
    .await?
    .is_none()
  {
    return Ok(());
  }
  let (action, change) = if is_withdrawal {
    ("admin_join_request_withdrawn", AdminJoinRequestChange::Withdrawn)
  } else {
    ("admin_join_request_declined", AdminJoinRequestChange::Declined)
  };
  audit::record(
    state,
    Some(caller.user.id),
    action,
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"user_id": user.to_string()}),
  )
  .await?;
  announce(state, conversation, user, change).await
}

/// `user` became a member of `conversation` (approved, or added some other way): an open request
/// of theirs is settled.
pub async fn resolve_on_join(
  state: &AppState,
  actor: UserId,
  conversation: ConversationId,
  user: UserId,
) -> Result<(), ApiError> {
  if state
    .database
    .take_admin_join_request(conversation, user)
    .await?
    .is_none()
  {
    return Ok(());
  }
  audit::record(
    state,
    Some(actor),
    "admin_join_request_approved",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"user_id": user.to_string()}),
  )
  .await?;
  announce(state, conversation, user, AdminJoinRequestChange::Approved).await
}

/// Closes the requests that ran out; run by the account housekeeping. Returns how many.
pub async fn sweep_expired(state: &AppState) -> Result<usize, ApiError> {
  let mut closed = 0;
  loop {
    let expired = state
      .database
      .take_expired_admin_join_requests(SWEEP_BATCH)
      .await?;
    for (conversation, user) in &expired {
      announce(state, *conversation, *user, AdminJoinRequestChange::Expired).await?;
    }
    closed += expired.len();
    if i64::try_from(expired.len()).unwrap_or(i64::MAX) < SWEEP_BATCH {
      return Ok(closed);
    }
  }
}

/// A new policy no longer asks for an owner's approval: every open request is moot (admins join
/// by themselves now, or not at all) and is closed.
pub async fn close_all_after_policy_change(state: &AppState, policy: &AdminSelfJoin) -> Result<(), ApiError> {
  if *policy == AdminSelfJoin::OwnerApproval {
    return Ok(());
  }
  let closed = state.database.take_all_admin_join_requests().await?;
  if !closed.is_empty() {
    info!(
      closed = closed.len(),
      "closed admin join requests after a policy change"
    );
  }
  for (conversation, user) in closed {
    announce(state, conversation, user, AdminJoinRequestChange::Expired).await?;
  }
  Ok(())
}

/// Only an open private channel the admin isn't in yet, in a space they belong to (members of a
/// channel must be in its space), and with an owner who can decide.
async fn ensure_joinable(
  state: &AppState,
  caller: &AuthenticatedUser,
  access: &ConversationAccess,
) -> Result<(), ApiError> {
  let channel = &access.view.conversation;
  if channel.visibility != ConversationVisibility::Private {
    return Err(ApiError::Conflict(
      "public channels are joined without asking".into(),
    ));
  }
  if access.is_member() {
    return Err(ApiError::Conflict("already a member of the channel".into()));
  }
  if access.maybe_space_role.is_none() {
    return Err(ApiError::NotSpaceMember);
  }
  let has_owner = state
    .database
    .list_conversation_members(channel.id)
    .await?
    .iter()
    .any(|member| member.role == ChannelRole::Admin && member.user.id != caller.user.id);
  if !has_owner {
    return Err(ApiError::Conflict(
      "the channel has no owner who could approve; assign one first".into(),
    ));
  }
  Ok(())
}

/// Requests are decided by the channel's owners: members with the channel role `admin`.
fn ensure_owner(access: &ConversationAccess) -> Result<(), ApiError> {
  if access.role() == Some(&ChannelRole::Admin) {
    Ok(())
  } else {
    Err(ApiError::Forbidden(
      "only the channel's owners decide on join requests".into(),
    ))
  }
}

/// Tells the channel's owners and the admin; they read the list again.
async fn announce(
  state: &AppState,
  conversation: ConversationId,
  user: UserId,
  change: AdminJoinRequestChange,
) -> Result<(), ApiError> {
  let mut recipients: Vec<UserId> = state
    .database
    .list_conversation_members(conversation)
    .await?
    .into_iter()
    .filter(|member| member.role == ChannelRole::Admin)
    .map(|member| member.user.id)
    .collect();
  recipients.push(user);
  state.hub.publish(
    recipients,
    &GatewayEvent::AdminJoinRequestUpdate {
      conversation_id: conversation,
      user_id: user,
      change,
    },
  );
  Ok(())
}

fn response(
  state: &AppState,
  request: &AdminJoinRequest,
  user: &teamcraft_db::User,
) -> AdminJoinRequestResponse {
  AdminJoinRequestResponse {
    conversation_id: request.conversation,
    user: user_summary(state, user),
    requested_at: unix_millis(request.requested_at),
    expires_at: unix_millis(request.expires_at),
  }
}
