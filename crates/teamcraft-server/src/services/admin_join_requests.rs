//! Admins asking the owners of a private channel to let them in (admin.md §2.4.1, issue #776).
//!
//! With `admin_self_join = owner_approval` members' clients refuse an admin's own signed self-join,
//! so the admin asks instead. The server only stores the request and tells the channel's owners
//! (members with the channel role `admin`, the server's record until channel roles are signed,
//! #380); it decides nothing that matters for the encryption. An owner's client approves by adding
//! the admin with its own MLS commit, which members' clients check like any other addition, and
//! posts the "joined as a workspace admin" message.
//!
//! The decision is the database's: approving is one transaction with the membership, so exactly
//! one of several owners (or devices) approves and a decline or an expiry at the same moment wins
//! completely or not at all. What follows it (telling people, the audit log) is best effort: a
//! failure there is logged and never undoes or strands the decision.

use std::collections::{BTreeSet, HashMap};

use serde_json::json;
use teamcraft_db::{
  AdminJoinApproval, AdminJoinRequest, AdminJoinRequestClosing, AdminJoinRequestOutcome,
  RemovedAdminJoinRequest,
};
use teamcraft_proto::{
  AdminJoinRequestChange, AdminJoinRequestResponse, AdminSelfJoin, ChannelRole, ConversationId,
  ConversationMemberResponse, ConversationVisibility, GatewayEvent, ListAdminJoinRequestsResponse,
  Permission, SystemEventKind, UserId,
};
use tracing::{info, warn};

use super::access::{ConversationAccess, is_guest, is_instance_admin, load_conversation, require};
use super::audit::{self, Target as AuditTarget};
use super::conversations::ensure_open_channel;
use super::convert::{unix_millis, user_summary};
use super::members;
use super::system_events;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Most requests one answer lists; open requests are few (admins × private channels they ask for).
const LIST_LIMIT: i64 = 200;
/// Expired requests closed per round of the housekeeping.
const SWEEP_BATCH: i64 = 500;
/// The bounds of `admin_join_request_days`, also for a value from the configuration file.
const MIN_DAYS: u32 = 1;
const MAX_DAYS: u32 = 90;

/// The caller, a workspace admin, asks the owners of the private channel `conversation` to let them
/// in. Asking again while a request is open answers with that request and tells nobody again;
/// after a decline or a withdrawal the admin waits until that request would have expired.
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
  let days = request_days(state);
  let stored = match state
    .database
    .open_admin_join_request(conversation, caller.user.id, days)
    .await?
  {
    AdminJoinRequestOutcome::AlreadyOpen(stored) => stored,
    AdminJoinRequestOutcome::TooSoon { .. } => return Err(ApiError::AdminJoinRequestTooSoon),
    AdminJoinRequestOutcome::Created(stored) => {
      tell(
        state,
        &[(conversation, caller.user.id)],
        AdminJoinRequestChange::Requested,
      )
      .await;
      record(
        state,
        Some(caller.user.id),
        "admin_join_requested",
        conversation,
        caller.user.id,
      )
      .await;
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

/// An owner of the channel lets the admin in: the request is approved and the admin a member in
/// one step (the owner's client commits the MLS add and announces the join next). Only while the
/// request is open; the same owner's retry after a lost answer is answered again, anybody else
/// gets `admin_join_request_not_found`.
pub async fn approve(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  user: UserId,
) -> Result<ConversationMemberResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  ensure_owner(&access)?;
  ensure_open_channel(&access)?;
  require(state, caller, Permission::ManageMembers, &access.target())?;
  if access.view.conversation.visibility != ConversationVisibility::Private {
    return Err(ApiError::Conflict(
      "only requests to join a private channel are approved".into(),
    ));
  }
  let requester = state
    .database
    .get_user(user)
    .await?
    .filter(|requester| !requester.is_deactivated)
    .ok_or_else(ApiError::user_not_found)?;
  ensure_still_admin(state, user).await?;
  let space = access
    .view
    .conversation
    .maybe_space_id
    .ok_or_else(|| ApiError::InvalidRequest("not allowed for DMs and group DMs".into()))?;
  if state.database.space_role(space, user).await?.is_none() {
    return Err(ApiError::NotSpaceMember);
  }
  match state
    .database
    .approve_admin_join_request(conversation, user, caller.user.id)
    .await?
  {
    AdminJoinApproval::NotOpen => return Err(ApiError::admin_join_request_not_found()),
    // A retry of this owner's approval: done before, nothing to tell again.
    AdminJoinApproval::ApprovedBefore => {},
    AdminJoinApproval::Approved { is_new_member } => {
      if is_new_member {
        if let Err(error) = members::announce_added_user(state, conversation, &requester).await {
          warn!(%error, conversation_id = %conversation, "could not announce an approved admin");
        }
        system_events::emit(
          state,
          conversation,
          SystemEventKind::MemberAdded,
          Some(caller.user.id),
          Some(user),
        )
        .await;
      }
      tell(state, &[(conversation, user)], AdminJoinRequestChange::Approved).await;
      record(
        state,
        Some(caller.user.id),
        "admin_join_request_approved",
        conversation,
        user,
      )
      .await;
    },
  }
  Ok(ConversationMemberResponse {
    user: user_summary(state, &requester),
    role: ChannelRole::Member,
  })
}

/// Declines the request (an owner of the channel) or withdraws it (the admin who asked).
/// Idempotent: a request that is no longer open (decided on another device, expired) answers `Ok`.
pub async fn close(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  user: UserId,
) -> Result<(), ApiError> {
  let closing = if user == caller.user.id {
    AdminJoinRequestClosing::Withdrawn
  } else {
    let access = load_conversation(state, caller, conversation).await?;
    ensure_owner(&access)?;
    AdminJoinRequestClosing::Declined
  };
  if !state
    .database
    .close_admin_join_request(conversation, user, closing, caller.user.id)
    .await?
  {
    return Ok(());
  }
  let (action, change) = match closing {
    AdminJoinRequestClosing::Withdrawn => ("admin_join_request_withdrawn", AdminJoinRequestChange::Withdrawn),
    AdminJoinRequestClosing::Declined => ("admin_join_request_declined", AdminJoinRequestChange::Declined),
  };
  tell(state, &[(conversation, user)], change).await;
  record(state, Some(caller.user.id), action, conversation, user).await;
  Ok(())
}

/// `user` became a member of `conversation` some other way: an open request of theirs is settled.
/// Best effort: the membership is in either way, and a request left open is not listed any more.
pub async fn resolve_on_join(state: &AppState, actor: UserId, conversation: ConversationId, user: UserId) {
  match state
    .database
    .settle_admin_join_request(conversation, user, actor)
    .await
  {
    Ok(false) => {},
    Ok(true) => {
      tell(state, &[(conversation, user)], AdminJoinRequestChange::Approved).await;
      record(
        state,
        Some(actor),
        "admin_join_request_approved",
        conversation,
        user,
      )
      .await;
    },
    Err(error) => warn!(%error, conversation_id = %conversation, "could not settle an admin join request"),
  }
}

/// Deletes the requests that ran out; the open ones are told and recorded. Run by the account
/// housekeeping. Returns how many open requests expired.
pub async fn sweep_expired(state: &AppState) -> Result<usize, ApiError> {
  let mut expired = 0;
  loop {
    let removed = state
      .database
      .take_expired_admin_join_requests(SWEEP_BATCH)
      .await?;
    expired += close_removed(state, &removed, "expired").await;
    if i64::try_from(removed.len()).unwrap_or(i64::MAX) < SWEEP_BATCH {
      return Ok(expired);
    }
  }
}

/// A new policy no longer asks for an owner's approval: every request is moot (admins join by
/// themselves now, or not at all) and goes. Called after the policy is stored and announced; the
/// caller logs a failure.
pub async fn close_all_after_policy_change(state: &AppState, policy: &AdminSelfJoin) -> Result<(), ApiError> {
  if *policy == AdminSelfJoin::OwnerApproval {
    return Ok(());
  }
  let removed = state.database.take_all_admin_join_requests().await?;
  let closed = close_removed(state, &removed, "policy_changed").await;
  if closed > 0 {
    info!(closed, "closed admin join requests after a policy change");
  }
  Ok(())
}

/// Tells the owners and admins of the removed requests that were still open, and records them.
/// Returns how many were open.
async fn close_removed(state: &AppState, removed: &[RemovedAdminJoinRequest], reason: &str) -> usize {
  let open: Vec<(ConversationId, UserId)> = removed
    .iter()
    .filter(|request| request.was_open)
    .map(|request| (request.conversation, request.user))
    .collect();
  tell(state, &open, AdminJoinRequestChange::Expired).await;
  for (conversation, user) in &open {
    if let Err(error) = audit::record(
      state,
      None,
      "admin_join_request_expired",
      Some(AuditTarget::new("conversation", *conversation)),
      &json!({"user_id": user.to_string(), "reason": reason}),
    )
    .await
    {
      warn!(%error, conversation_id = %conversation, "could not record an expired admin join request");
    }
  }
  open.len()
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
  let owners = state
    .database
    .channel_owners(&BTreeSet::from([channel.id]))
    .await?;
  if !owners.iter().any(|(_, owner)| *owner != caller.user.id) {
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

/// The person who asked is still a workspace admin as far as the server knows (the role their
/// last sign-in showed). The owner's client checks the signed roster on top.
async fn ensure_still_admin(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let is_admin = state
    .database
    .get_user_oidc_role(user)
    .await?
    .flatten()
    .is_some_and(|role| role == "admin" || role == "owner");
  if is_admin {
    Ok(())
  } else {
    Err(ApiError::Forbidden(
      "the person who asked is no workspace admin any more".into(),
    ))
  }
}

/// `admin_join_request_days`, kept within its bounds whatever the configuration file says.
fn request_days(state: &AppState) -> u32 {
  state
    .settings
    .get()
    .policy
    .admin_join_request_days
    .clamp(MIN_DAYS, MAX_DAYS)
}

/// Tells the channels' owners and the admins (they read the list again). Best effort: logged when
/// the owners can't be read. One query for all channels.
async fn tell(state: &AppState, requests: &[(ConversationId, UserId)], change: AdminJoinRequestChange) {
  if requests.is_empty() {
    return;
  }
  let conversations: BTreeSet<ConversationId> =
    requests.iter().map(|(conversation, _)| *conversation).collect();
  let owners = match state.database.channel_owners(&conversations).await {
    Ok(owners) => owners,
    Err(error) => {
      warn!(%error, "could not read the owners to tell about admin join requests");
      Vec::new()
    },
  };
  for (conversation, user) in requests {
    let recipients = owners
      .iter()
      .filter(|(owned, _)| owned == conversation)
      .map(|(_, owner)| *owner)
      .chain([*user]);
    state.hub.publish(
      recipients,
      &GatewayEvent::AdminJoinRequestUpdate {
        conversation_id: *conversation,
        user_id: *user,
        change: change.clone(),
      },
    );
  }
}

/// An audit entry for a request; best effort after the decision is stored.
async fn record(
  state: &AppState,
  actor: Option<UserId>,
  action: &str,
  conversation: ConversationId,
  user: UserId,
) {
  if let Err(error) = audit::record(
    state,
    actor,
    action,
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"user_id": user.to_string()}),
  )
  .await
  {
    warn!(%error, conversation_id = %conversation, action, "could not record an admin join request");
  }
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
