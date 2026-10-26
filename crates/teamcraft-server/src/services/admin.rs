//! Administration (admin.md §3): metadata of everything, never content.

use serde_json::json;
use teamcraft_db::{
  ActiveAdminCounts, AdminConversationFilter, AdminConversationRow, AdminDeviceRow, AdminSpaceRow,
  AdminUserRow, AuditEntry, AuditQuery, ChatRemoval, ChatRemovalEffects, LOCAL_ISSUER, UserDeletion,
  UserStatusFilter,
};
use teamcraft_proto::{
  AdminConversationSummary, AdminDeviceResponse, AdminInstanceResponse, AdminSpaceSummary, AdminUserStatus,
  AdminUserSummary, AuditEntryId, AuditEntryResponse, ConversationId, ConversationKind,
  DeactivateUserRequest, DeactivationReason, DeviceId, GatewayEvent, Id, ListAdminConversationsQuery,
  ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersQuery,
  ListAdminUsersResponse, ListAuditQuery, ListAuditResponse, ListConversationMembersResponse,
  ListSpaceMembersResponse, Permission, PushProvider, PushProviderState, PushProviderStatus, SfuStatus,
  SpaceId, StorageStatus, Target, UnixMillis, UserId,
};
use time::OffsetDateTime;

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Users removed from all chats per sweep run; the rest follow in the next run.
const REMOVAL_SWEEP_BATCH: i64 = 50;
/// The identity provider sync deactivating this many users within an hour looks like an outage or
/// a misconfiguration, not people leaving: the automatic removal leaves them to an admin.
const MASS_OIDC_DEACTIVATION: i64 = 10;
/// How long a direct message deleted with a deleted account is remembered for the remaining
/// participants, so a device offline that long still drops it with its local history on the
/// next sync; after that it only disappears from the list.
pub const DELETED_CONVERSATION_MEMORY: time::Duration = time::Duration::days(180);

pub async fn list_spaces(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListAdminSpacesResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let rows = state.database.admin_list_spaces().await?;
  Ok(ListAdminSpacesResponse {
    spaces: rows.iter().map(space_summary).collect(),
  })
}

pub async fn list_space_members(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<ListSpaceMembersResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if state.database.get_space(space, caller.user.id).await?.is_none() {
    return Err(ApiError::space_not_found());
  }
  let members = state.database.list_space_members(space).await?;
  Ok(ListSpaceMembersResponse {
    members: members
      .iter()
      .map(|m| teamcraft_proto::SpaceMemberResponse {
        user: super::convert::user_summary(state, &m.user),
        role: m.role.clone(),
        maybe_expires_at: m.maybe_expires_at.map(super::convert::unix_millis),
        maybe_sponsor_user_id: m.maybe_sponsor,
      })
      .collect(),
  })
}

pub async fn list_conversations(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListAdminConversationsQuery,
) -> Result<ListAdminConversationsResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let limit = page_limit(state, query.maybe_limit);
  let mut rows = state
    .database
    .admin_list_conversations(&AdminConversationFilter {
      maybe_space: query.maybe_space_id,
      maybe_visibility: query.maybe_visibility,
      maybe_has_guests: query.maybe_has_guests,
      maybe_inactive_days: query.maybe_inactive_days,
      maybe_min_members: query.maybe_min_members,
      maybe_max_members: query.maybe_max_members,
      maybe_is_orphaned: query.maybe_is_orphaned,
      maybe_after: query.maybe_after,
      maybe_member: query.maybe_user_id,
      limit: i64::from(limit),
    })
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  Ok(ListAdminConversationsResponse {
    conversations: rows.iter().map(conversation_summary).collect(),
    has_more,
  })
}

/// Members of a channel (never of a DM): admins manage rooms without reading them.
pub async fn list_conversation_members(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ListConversationMembersResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let view = state
    .database
    .get_conversation(conversation, caller.user.id)
    .await?;
  if view.is_none_or(|v| v.conversation.kind != ConversationKind::Channel) {
    return Err(ApiError::conversation_not_found());
  }
  let members = state.database.list_conversation_members(conversation).await?;
  Ok(ListConversationMembersResponse {
    members: members
      .iter()
      .map(|m| teamcraft_proto::ConversationMemberResponse {
        user: super::convert::user_summary(state, &m.user),
        role: m.role.clone(),
      })
      .collect(),
  })
}

pub async fn list_users(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListAdminUsersQuery,
) -> Result<ListAdminUsersResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let limit = page_limit(state, query.maybe_limit);
  let status = match query.maybe_status.unwrap_or(AdminUserStatus::All) {
    AdminUserStatus::Active => UserStatusFilter::Active,
    AdminUserStatus::Deactivated => UserStatusFilter::Deactivated,
    _ => UserStatusFilter::All,
  };
  let mut rows = state
    .database
    .admin_list_users(query.query.trim(), status, query.maybe_after, i64::from(limit))
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  Ok(ListAdminUsersResponse {
    users: rows.iter().map(user_summary).collect(),
    has_more,
  })
}

pub async fn list_user_devices(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<ListAdminDevicesResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if state.database.get_user(user).await?.is_none() {
    return Err(ApiError::user_not_found());
  }
  let rows = state.database.admin_list_devices(user).await?;
  Ok(ListAdminDevicesResponse {
    devices: rows.iter().map(device_response).collect(),
  })
}

pub async fn deactivate_user_as_admin(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
  request: DeactivateUserRequest,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if user == caller.user.id {
    return Err(ApiError::InvalidRequest(
      "admins can't deactivate themselves".into(),
    ));
  }
  ensure_may_act_on(state, caller, user).await?;
  let note = request.maybe_note.unwrap_or_default();
  if note.chars().count() > state.settings.get().limits.max_text_length {
    return Err(ApiError::InvalidRequest("note is too long".into()));
  }
  deactivate_user(
    state,
    Some(caller.user.id),
    user,
    DeactivationReason::Admin,
    &note,
    AfterDeactivation::KeepChats,
  )
  .await?;
  Ok(())
}

/// Admins manage members, guests and bots; owners also manage admins and other owners. Without
/// this an admin could lock every owner (the only signers of roster and policy) out of the
/// workspace.
async fn ensure_may_act_on(
  state: &AppState,
  caller: &AuthenticatedUser,
  target: UserId,
) -> Result<(), ApiError> {
  let target_role = state
    .database
    .get_user_oidc_role(target)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let rank = |role: Option<&str>| match role {
    Some("owner") => 3,
    Some("admin") => 2,
    _ => 1,
  };
  let caller_rank = rank(Some(caller.role.as_str()));
  let target_rank = rank(target_role.as_deref());
  if caller_rank > target_rank || (caller_rank == 3 && target_rank == 3) {
    Ok(())
  } else {
    Err(ApiError::Forbidden(
      "only owners can manage admins and owners".into(),
    ))
  }
}

/// Deactivates (suspends) the account (used by admins, the OIDC sync, bots and meeting guests):
/// every session ends at once and signing in is refused, but the user stays in their chats with
/// their devices, so a reactivation restores everything. With [`AfterDeactivation::RemoveFromChats`]
/// the user is also removed from all chats at once. Returns `false` if already deactivated.
pub async fn deactivate_user(
  state: &AppState,
  maybe_actor: Option<UserId>,
  user: UserId,
  reason: DeactivationReason,
  note: &str,
  after: AfterDeactivation,
) -> Result<bool, ApiError> {
  let deactivation = state.database.deactivate_user(user, reason.as_str()).await?;
  let is_new = !deactivation.was_already_deactivated;
  // Idempotent, so run also when it already was: a crash after the commit must not leave a
  // session or a call running on a retry.
  end_sessions(state, user).await?;
  for conversation in &deactivation.conversations {
    super::calls::evict_from_call(state, *conversation, user).await;
  }
  if after == AfterDeactivation::RemoveFromChats {
    remove_from_all_chats(state, maybe_actor, user).await?;
  }
  if is_new {
    // Clients fetch the profile again and show the user as deactivated.
    state.hub.publish(
      deactivation.co_members.iter().copied(),
      &GatewayEvent::ProfileUpdate { user_id: user },
    );
    audit::record(
      state,
      maybe_actor,
      "user_deactivated",
      Some(AuditTarget::new("user", user)),
      &json!({"reason": reason.as_str(), "note": note, "conversations": deactivation.conversations.len()}),
    )
    .await?;
  }
  Ok(is_new)
}

/// What follows a deactivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterDeactivation {
  /// The user stays in their chats (suspended); an admin can remove them later.
  KeepChats,
  /// The user is removed from all chats at once (bots, meeting guests: they don't come back).
  RemoveFromChats,
}

/// Ends every session of the user: gateway connections close and can't resume, local sessions end.
/// Access tokens of the identity provider stay valid until they expire, but every request checks
/// the account and refuses a deactivated one.
async fn end_sessions(state: &AppState, user: UserId) -> Result<(), ApiError> {
  state.hub.revoke_user(user);
  state
    .database
    .end_sessions_of(LOCAL_ISSUER, &user.to_string())
    .await?;
  Ok(())
}

/// The admin action "Remove from all chats" for a deactivated user (admin.md §3.4): their devices
/// are removed and they leave every conversation and space; the members' clients commit the
/// removals with a key rotation (e2ee.md §4.6). Not reversible by a reactivation.
pub async fn remove_from_chats_as_admin(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  ensure_may_act_on(state, caller, user).await?;
  remove_from_all_chats(state, Some(caller.user.id), user).await
}

/// Removes a deactivated user from all chats and tells everyone affected, so their clients commit
/// the device removals to the MLS groups. The server's removal is only a hint to the clients
/// (security review A8): each client checks the removal before it commits.
pub async fn remove_from_all_chats(
  state: &AppState,
  maybe_actor: Option<UserId>,
  user: UserId,
) -> Result<(), ApiError> {
  let effects = match state.database.remove_deactivated_user_from_chats(user).await? {
    ChatRemoval::Removed(effects) => effects,
    ChatRemoval::NotDeactivated => {
      return Err(ApiError::Conflict(
        "only deactivated users can be removed from all chats".into(),
      ));
    },
    ChatRemoval::UnknownUser => return Err(ApiError::user_not_found()),
  };
  if !announce_chat_removal(state, user, &effects).await {
    return Ok(());
  }
  audit::record(
    state,
    maybe_actor,
    "user_removed_from_chats",
    Some(AuditTarget::new("user", user)),
    &json!({"devices": effects.devices.len(), "conversations": effects.conversations.len(), "spaces": effects.spaces.len()}),
  )
  .await?;
  Ok(())
}

/// Tells everyone affected that the user left conversations and spaces and lost their devices, so
/// their clients commit the device removals to the MLS groups (only a hint, security review A8),
/// and ends the user's part in running calls. `false` if there was nothing to tell.
async fn announce_chat_removal(state: &AppState, user: UserId, effects: &ChatRemovalEffects) -> bool {
  let is_nothing_left =
    effects.devices.is_empty() && effects.conversations.is_empty() && effects.spaces.is_empty();
  if is_nothing_left {
    return false;
  }
  for (conversation, _) in &effects.conversations {
    super::calls::evict_from_call(state, *conversation, user).await;
  }
  for (conversation, remaining) in &effects.conversations {
    state.hub.publish(
      remaining.iter().copied(),
      &GatewayEvent::MembershipRemove {
        conversation_id: *conversation,
        user_id: user,
      },
    );
  }
  for (space, remaining) in &effects.spaces {
    state.hub.publish(
      remaining.iter().copied(),
      &GatewayEvent::SpaceMemberRemove {
        space_id: *space,
        user_id: user,
      },
    );
  }
  for device in &effects.devices {
    state.hub.publish(
      effects.co_members.iter().copied(),
      &GatewayEvent::DeviceRemove {
        user_id: user,
        device_id: *device,
      },
    );
  }
  true
}

/// "Delete account" (admin.md §3.2.1, issue #630). The admin's client signed the user's removal
/// from every channel before (the server never fakes a membership change); this does the rest:
/// the user's 1:1 direct messages (and group DMs that would keep one other person) are deleted
/// with their content, devices are revoked, key material, sessions and memberships go, the
/// identity provider's identity is released, and the account stays as a tombstone that names old
/// messages. Idempotent: a retry after a lost answer finishes what is left and succeeds.
pub async fn delete_user_as_admin(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if user == caller.user.id {
    return Err(ApiError::CannotDeleteSelf);
  }
  let target = state
    .database
    .get_user(user)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  if !target.is_deleted {
    ensure_may_act_on(state, caller, user).await?;
    ensure_admins_remain(state, user, target.is_deactivated).await?;
  }
  let direct_conversations = delete_direct_conversations(state, user).await?;
  for key in state.database.user_object_keys(user).await? {
    super::retention::delete_object(state, &key).await?;
  }
  let (was_already_deleted, effects) = match state.database.delete_user(user).await? {
    UserDeletion::Deleted {
      was_already_deleted,
      effects,
    } => (was_already_deleted, effects),
    UserDeletion::UnknownUser => return Err(ApiError::user_not_found()),
  };
  // Also on a retry: a crash after the commit must not leave a connection or a call running.
  state.hub.revoke_user(user);
  announce_chat_removal(state, user, &effects).await;
  if was_already_deleted {
    return Ok(());
  }
  // Clients fetch the profile again and show the tombstone.
  state.hub.publish(
    effects.co_members.iter().copied(),
    &GatewayEvent::ProfileUpdate { user_id: user },
  );
  audit::record(
    state,
    Some(caller.user.id),
    "user_deleted",
    Some(AuditTarget::new("user", user)),
    &json!({
      "devices": effects.devices.len(),
      "conversations": effects.conversations.len(),
      "spaces": effects.spaces.len(),
      "direct_conversations_deleted": direct_conversations,
    }),
  )
  .await?;
  Ok(())
}

/// Deleting an active owner or admin must leave the workspace with another active one.
async fn ensure_admins_remain(state: &AppState, user: UserId, is_deactivated: bool) -> Result<(), ApiError> {
  if is_deactivated {
    // Not counted as an active admin anyway.
    return Ok(());
  }
  let role = state.database.get_user_oidc_role(user).await?.flatten();
  let counts = state.database.active_admin_counts().await?;
  admins_remain_without(role.as_deref(), counts)
}

/// Whether the workspace keeps an active owner and admin once an active account with `role` goes.
fn admins_remain_without(role: Option<&str>, counts: ActiveAdminCounts) -> Result<(), ApiError> {
  match role {
    Some("owner") if counts.owners <= 1 => Err(ApiError::LastOwner),
    Some("owner" | "admin") if counts.admins <= 1 => Err(ApiError::LastAdmin),
    _ => Ok(()),
  }
}

/// Deletes the user's 1:1 direct messages, and group DMs that would keep at most one other person,
/// with their content in storage (PO decision on #630: a conversation with nobody left to talk to
/// goes). The remaining participants' devices drop it with its local history: live by the gateway,
/// later by the next sync (`deleted_conversation_ids`). Returns how many went.
async fn delete_direct_conversations(state: &AppState, user: UserId) -> Result<usize, ApiError> {
  let conversations = state.database.direct_conversations_deleted_with(user).await?;
  for direct in &conversations {
    let conversation_id = direct.conversation;
    super::calls::evict_from_call(state, conversation_id, user).await;
    // Storage first: a failure leaves the rows, so a retry finds the objects again.
    for key in state.database.conversation_object_keys(conversation_id).await? {
      super::retention::delete_object(state, &key).await?;
    }
    state
      .database
      .delete_direct_conversation(conversation_id, &direct.remaining)
      .await?;
    state.hub.publish(
      direct.remaining.iter().copied(),
      &GatewayEvent::ConversationDelete { conversation_id },
    );
    // Clients that don't know `conversation_delete` yet drop a conversation they were removed from.
    for other in &direct.remaining {
      state.hub.publish(
        [*other],
        &GatewayEvent::MembershipRemove {
          conversation_id,
          user_id: *other,
        },
      );
    }
  }
  Ok(conversations.len())
}

/// Removes users from all chats whose deactivation is older than the instance setting
/// `remove_deactivated_after_days` (off by default). Run periodically; returns how many it removed.
pub async fn sweep_deactivated_users(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let Some(days) = state.settings.get().policy.maybe_remove_deactivated_after_days else {
    return Ok(0);
  };
  let cutoff = now - time::Duration::days(i64::from(days));
  let due = state
    .database
    .deactivated_users_due_for_removal(cutoff, MASS_OIDC_DEACTIVATION, REMOVAL_SWEEP_BATCH)
    .await?;
  let mut removed = 0;
  for user in due {
    match remove_from_all_chats(state, None, user).await {
      Ok(()) => removed += 1,
      // Reactivated since the query: nothing to do.
      Err(ApiError::Conflict(_)) => {},
      Err(error) => return Err(error),
    }
  }
  Ok(removed)
}

pub async fn reactivate_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  ensure_may_act_on(state, caller, user).await?;
  if !state.database.reactivate_user(user).await? {
    return Err(ApiError::user_not_found());
  }
  announce_reactivation(state, user).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "user_reactivated",
    Some(AuditTarget::new("user", user)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// Tells everyone who shares a conversation with the user that they are back.
pub async fn announce_reactivation(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let audience = state.database.users_sharing_conversation(user).await?;
  state
    .hub
    .publish(audience, &GatewayEvent::ProfileUpdate { user_id: user });
  Ok(())
}

/// Removes one device of a user (a lost phone), announced like any device removal.
pub async fn revoke_user_device(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
  device: DeviceId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  ensure_may_act_on(state, caller, user).await?;
  super::devices::revoke_and_announce(state, caller.user.id, user, device, "admin").await
}

pub async fn list_audit(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListAuditQuery,
) -> Result<ListAuditResponse, ApiError> {
  require(state, caller, Permission::ReadAuditLog, &Target::Instance)?;
  query_audit(state, query).await
}

/// What happened to the caller's own account: devices removed, deactivation, recoveries.
pub async fn security_log(
  state: &AppState,
  caller: &AuthenticatedUser,
  mut query: ListAuditQuery,
) -> Result<ListAuditResponse, ApiError> {
  query.maybe_actor = None;
  query.maybe_target_kind = Some("user".into());
  query.maybe_target_id = Some(caller.user.id.to_string());
  query_audit(state, query).await
}

async fn query_audit(state: &AppState, query: ListAuditQuery) -> Result<ListAuditResponse, ApiError> {
  let limit = page_limit(state, query.maybe_limit);
  let target_id = query
    .maybe_target_id
    .as_deref()
    .map(|raw| {
      raw
        .parse::<i64>()
        .map_err(|_| ApiError::InvalidRequest("invalid target_id".into()))
    })
    .transpose()?;
  let time = |value: Option<UnixMillis>| {
    value
      .map(|millis| offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid time".into())))
      .transpose()
  };
  let mut rows = state
    .database
    .list_audit(&AuditQuery {
      maybe_actor: query.maybe_actor,
      maybe_action: query.maybe_action.as_deref(),
      maybe_target_kind: query.maybe_target_kind.as_deref(),
      maybe_target_id: target_id,
      maybe_since: time(query.maybe_since)?,
      maybe_until: time(query.maybe_until)?,
      maybe_before: query.maybe_before.map(|id| id.snowflake()),
      limit: i64::from(limit),
    })
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  Ok(ListAuditResponse {
    entries: rows.iter().map(audit_response).collect(),
    has_more,
  })
}

fn page_limit(state: &AppState, maybe_limit: Option<u32>) -> u32 {
  let limits = &state.settings.get().limits;
  maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size)
}

fn count(value: i64) -> u32 {
  u32::try_from(value).unwrap_or(u32::MAX)
}

fn space_summary(row: &AdminSpaceRow) -> AdminSpaceSummary {
  AdminSpaceSummary {
    id: row.id,
    name: row.name.clone(),
    join_policy: row.join_policy.clone(),
    is_archived: row.is_archived,
    member_count: count(row.member_count),
    guest_count: count(row.guest_count),
    channel_count: count(row.channel_count),
    created_at: unix_millis(row.created_at),
  }
}

fn conversation_summary(row: &AdminConversationRow) -> AdminConversationSummary {
  AdminConversationSummary {
    id: row.id,
    maybe_space_id: row.maybe_space,
    maybe_name: row.maybe_name.clone(),
    visibility: row.visibility.clone(),
    is_archived: row.is_archived,
    member_count: count(row.member_count),
    guest_count: count(row.guest_count),
    message_count: u64::try_from(row.message_count).unwrap_or(0),
    storage_bytes: u64::try_from(row.storage_bytes).unwrap_or(0),
    created_at: unix_millis(row.created_at),
    maybe_last_activity_at: row.maybe_last_message_millis.map(UnixMillis),
  }
}

fn user_summary(row: &AdminUserRow) -> AdminUserSummary {
  AdminUserSummary {
    id: row.id,
    maybe_username: row.maybe_username.clone(),
    display_name: row.display_name.clone(),
    kind: row.kind.clone(),
    maybe_oidc_role: row.maybe_oidc_role.clone(),
    is_deactivated: row.is_deactivated,
    is_deleted: row.is_deleted,
    maybe_deactivation_reason: row
      .maybe_deactivation_reason
      .clone()
      .map(DeactivationReason::from),
    device_count: count(row.device_count),
    space_count: count(row.space_count),
    conversation_count: count(row.conversation_count),
    maybe_last_seen_at: row.maybe_last_seen.map(unix_millis),
    created_at: unix_millis(row.created_at),
  }
}

fn device_response(row: &AdminDeviceRow) -> AdminDeviceResponse {
  AdminDeviceResponse {
    id: row.id,
    name: row.name.clone(),
    is_revoked: row.is_revoked,
    maybe_push_provider: row.maybe_push_provider.clone().map(|p: PushProvider| p),
    maybe_last_seen_at: row.maybe_last_seen.map(unix_millis),
    created_at: unix_millis(row.created_at),
  }
}

fn audit_response(entry: &AuditEntry) -> AuditEntryResponse {
  AuditEntryResponse {
    id: AuditEntryId::from_snowflake(entry.id),
    maybe_actor_user_id: entry.maybe_actor,
    action: entry.action.clone(),
    maybe_target_kind: entry.maybe_target_kind.clone(),
    maybe_target_id: entry.maybe_target_id.map(|id| id.to_string()),
    detail: entry.detail.clone(),
    created_at: unix_millis(entry.created_at),
  }
}

/// Makes an existing member the admin of a channel, typically one whose admins left or were
/// deactivated. The new owner must already be a member (membership of end-to-end encrypted
/// channels is changed by the members' clients, not by this call).
pub async fn assign_channel_owner(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: teamcraft_proto::AssignChannelOwnerRequest,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let view = state
    .database
    .get_conversation(conversation, caller.user.id)
    .await?;
  if view.is_none_or(|v| v.conversation.kind != ConversationKind::Channel) {
    return Err(ApiError::conversation_not_found());
  }
  let members = state.database.list_conversation_members(conversation).await?;
  let Some(member) = members
    .iter()
    .find(|member| member.user.id == request.user_id && !member.user.is_deactivated)
  else {
    return Err(ApiError::Conflict(
      "the new owner must be an active member of the channel".into(),
    ));
  };
  if member.user.kind != teamcraft_proto::UserKind::Member {
    return Err(ApiError::InvalidRequest("only members can own channels".into()));
  }
  state
    .database
    .update_membership(
      conversation,
      request.user_id,
      &teamcraft_db::MembershipChanges {
        maybe_role: Some(teamcraft_proto::ChannelRole::Admin),
        ..Default::default()
      },
    )
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "channel_owner_assigned",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"user_id": request.user_id.to_string()}),
  )
  .await?;
  super::system_events::emit(
    state,
    conversation,
    teamcraft_proto::SystemEventKind::OwnerAssigned,
    Some(caller.user.id),
    Some(request.user_id),
  )
  .await;
  let ids = state.database.conversation_member_ids(conversation).await?;
  state.hub.publish(
    ids,
    &teamcraft_proto::GatewayEvent::ConversationUpdate {
      conversation_id: conversation,
    },
  );
  Ok(())
}

/// What the instance runs on: media server, push providers and storage (admin.md §3.4).
pub async fn instance_status(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<AdminInstanceResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let sfu = match &state.maybe_calls {
    Some(calls) => SfuStatus {
      is_configured: true,
      maybe_url: Some(calls.sfu_url().to_owned()),
      is_reporting_enabled: calls.is_reporting_enabled(),
      has_host_controls: calls.has_controls(),
      maybe_last_contact_at: calls.last_sfu_contact(),
      active_calls: u32::try_from(calls.active_call_count()).unwrap_or(u32::MAX),
    },
    None => SfuStatus {
      is_configured: false,
      maybe_url: None,
      is_reporting_enabled: false,
      has_host_controls: false,
      maybe_last_contact_at: None,
      active_calls: 0,
    },
  };
  let counts = state.database.admin_push_device_counts().await?;
  let push = [
    PushProvider::WebPush,
    PushProvider::Fcm,
    PushProvider::UnifiedPush,
    PushProvider::Apns,
  ]
  .into_iter()
  .map(|provider| {
    let health = state.push.health(&provider);
    let state_of = if !state.push.is_configured(&provider) {
      PushProviderState::NotConfigured
    } else if health.is_failing() {
      PushProviderState::Failing
    } else {
      PushProviderState::Active
    };
    let wire_name = provider.as_str();
    PushProviderStatus {
      device_count: counts
        .iter()
        .find(|count| count.provider == wire_name)
        .map_or(0, |count| u32::try_from(count.device_count).unwrap_or(u32::MAX)),
      provider,
      state: state_of,
      success_count: health.success_count,
      failure_count: health.failure_count,
      endpoint_failure_count: health.endpoint_failure_count,
      maybe_last_success_at: health.maybe_last_success_at,
      maybe_last_failure_at: health.maybe_last_failure_at,
      maybe_last_error: health.maybe_last_error,
    }
  })
  .collect();
  let totals = state.database.admin_storage_totals().await?;
  Ok(AdminInstanceResponse {
    sfu,
    push,
    storage: StorageStatus {
      is_configured: state.maybe_blob_store.is_some(),
      attachment_count: totals.attachment_count,
      attachment_bytes: totals.attachment_bytes,
      index_segment_bytes: totals.index_segment_bytes,
    },
    is_telephony_available: false,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_last_active_owner_and_admin_stay() {
    let counts = |owners, admins| ActiveAdminCounts { owners, admins };
    assert!(matches!(
      admins_remain_without(Some("owner"), counts(1, 3)),
      Err(ApiError::LastOwner)
    ));
    assert!(matches!(
      admins_remain_without(Some("admin"), counts(1, 1)),
      Err(ApiError::LastAdmin)
    ));
    assert!(matches!(
      admins_remain_without(Some("owner"), counts(2, 1)),
      Err(ApiError::LastAdmin)
    ));
    assert!(admins_remain_without(Some("owner"), counts(2, 2)).is_ok());
    assert!(admins_remain_without(Some("admin"), counts(1, 2)).is_ok());
    assert!(admins_remain_without(Some("member"), counts(0, 0)).is_ok());
    assert!(admins_remain_without(None, counts(0, 0)).is_ok());
  }
}
