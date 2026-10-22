//! Administration (admin.md §3): metadata of everything, never content.

use serde_json::json;
use teamcraft_db::{
  AdminConversationFilter, AdminConversationRow, AdminDeviceRow, AdminSpaceRow, AdminUserRow, AuditEntry,
  AuditQuery, UserStatusFilter,
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

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

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
  if note.chars().count() > state.settings.limits.max_text_length {
    return Err(ApiError::InvalidRequest("note is too long".into()));
  }
  deactivate_user(
    state,
    Some(caller.user.id),
    user,
    DeactivationReason::Admin,
    &note,
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

/// Deactivates the account everywhere (used by admins and by the OIDC sync): devices are flagged
/// removed, gateway sessions end, and everyone who shared a conversation hears about the removals
/// so their clients commit them to the MLS groups. Returns `false` if already deactivated.
pub async fn deactivate_user(
  state: &AppState,
  maybe_actor: Option<UserId>,
  user: UserId,
  reason: DeactivationReason,
  note: &str,
) -> Result<bool, ApiError> {
  let effects = state.database.deactivate_user(user, reason.as_str()).await?;
  if effects.was_already_deactivated {
    return Ok(false);
  }

  state.hub.revoke_user(user);
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
  audit::record(
    state,
    maybe_actor,
    "user_deactivated",
    Some(AuditTarget::new("user", user)),
    &json!({"reason": reason.as_str(), "note": note, "devices": effects.devices.len(), "conversations": effects.conversations.len()}),
  )
  .await?;
  Ok(true)
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
  let limits = &state.settings.limits;
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
    maybe_deactivation_reason: row
      .maybe_deactivation_reason
      .clone()
      .map(DeactivationReason::from),
    device_count: count(row.device_count),
    space_count: count(row.space_count),
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
