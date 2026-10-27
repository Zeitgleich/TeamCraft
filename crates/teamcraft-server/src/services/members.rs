use serde_json::json;
use teamcraft_db::{ConversationMember, MembershipChanges};
use teamcraft_proto::{
  AddConversationMemberRequest, ChannelRole, ConversationId, ConversationMemberResponse,
  ConversationVisibility, GatewayEvent, ListConversationMembersResponse, Permission, SetReadPositionRequest,
  SpaceRole, SystemEventKind, UpdateConversationMemberRequest, UserId,
};
use tracing::warn;

use super::access::{ConversationAccess, is_guest, load_conversation, require};
use super::admin_join_requests;
use super::audit::{self, Target as AuditTarget};
use super::conversations::{can_list_members, ensure_open_channel};
use super::convert::user_summary;
use super::notifications;
use super::system_events;
use super::threads;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn list_members(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<ListConversationMembersResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  if !can_list_members(&access, caller) {
    return Err(ApiError::Forbidden(
      "only members can see who is in the channel".into(),
    ));
  }
  let members = state.database.list_conversation_members(id).await?;
  Ok(members_response(state, &members))
}

/// The answer of `GET …/members` for these members (also what `GET /sync/heads` digests).
pub fn members_response(state: &AppState, members: &[ConversationMember]) -> ListConversationMembersResponse {
  ListConversationMembersResponse {
    members: members
      .iter()
      .map(|member| ConversationMemberResponse {
        user: user_summary(state, &member.user),
        role: member.role.clone(),
      })
      .collect(),
  }
}

/// Joins a public channel of one of the caller's spaces. Guests are added by someone with `manage_members`.
pub async fn join_conversation(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_open_channel(&access)?;
  if access.is_member() {
    return Ok(());
  }
  let is_public = access.view.conversation.visibility == ConversationVisibility::Public;
  if !is_public || is_guest(caller) || access.maybe_space_role.is_none() {
    return Err(ApiError::Forbidden(
      "this channel can only be joined by invitation".into(),
    ));
  }
  if state
    .database
    .add_conversation_member(id, caller.user.id, ChannelRole::Member)
    .await?
  {
    announce_added(state, id, caller).await?;
  }
  Ok(())
}

pub async fn add_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: AddConversationMemberRequest,
) -> Result<ConversationMemberResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_open_channel(&access)?;
  require(state, caller, Permission::ManageMembers, &access.target())?;
  // Admins manage private channels without being members, but the signed policy decides whether
  // they may add themselves (admin.md §2.4). Members' clients enforce it for the encryption; the
  // server does not hand out the membership rows (member list, read positions, call roster) either.
  if request.user_id == caller.user.id
    && !access.is_member()
    && access.view.conversation.visibility == ConversationVisibility::Private
    && state.policy.get().admin_self_join != teamcraft_proto::AdminSelfJoin::Visible
  {
    return Err(ApiError::Forbidden(
      "the workspace policy does not let admins join private channels on their own".into(),
    ));
  }
  let role = request.maybe_role.unwrap_or(ChannelRole::Member);
  if matches!(role, ChannelRole::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown channel role".into()));
  }
  if is_privileged(&role) {
    require(state, caller, Permission::ManageChannelRoles, &access.target())?;
  }

  let user = state
    .database
    .get_user(request.user_id)
    .await?
    .filter(|u| !u.is_deactivated);
  let Some(user) = user else {
    return Err(ApiError::user_not_found());
  };
  // Channel members must belong to the channel's space; guests were invited to the space first.
  let Some(space) = access.view.conversation.maybe_space_id else {
    return Err(ApiError::InvalidRequest(
      "not allowed for DMs and group DMs".into(),
    ));
  };
  let maybe_space_role = state.database.space_role(space, user.id).await?;
  let Some(space_role) = maybe_space_role else {
    return Err(ApiError::NotSpaceMember);
  };
  if space_role == SpaceRole::Guest && is_privileged(&role) {
    return Err(ApiError::InvalidRequest(
      "guests can't be channel admins or moderators".into(),
    ));
  }

  state
    .database
    .add_conversation_member(id, user.id, role.clone())
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "conversation_member_added",
    Some(AuditTarget::new("conversation", id)),
    &json!({"user_id": user.id.to_string()}),
  )
  .await?;
  announce_added_user(state, id, &user).await?;
  system_events::emit(
    state,
    id,
    SystemEventKind::MemberAdded,
    Some(caller.user.id),
    Some(user.id),
  )
  .await;
  // Best effort: the membership is in; a request left open is listed no more and expires.
  admin_join_requests::resolve_on_join(state, caller.user.id, id, user.id).await;
  Ok(ConversationMemberResponse {
    user: user_summary(state, &user),
    role,
  })
}

/// Checks that the caller may remove `user_id` (or leave), without changing anything. Clients ask
/// before they commit the MLS removal, so the group never drops somebody the server then refuses
/// to remove (e2ee.md §4.6, removal order).
pub async fn check_removal(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  user_id: UserId,
) -> Result<(), ApiError> {
  authorize_removal(state, caller, id, user_id).await.map(|_| ())
}

/// Removes a member, or leaves when `user_id` is the caller. Idempotent: removing somebody who is
/// no longer a member succeeds, so a retry after a lost answer or a second admin doing the same
/// finishes instead of failing.
pub async fn remove_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  user_id: UserId,
) -> Result<(), ApiError> {
  let Some(members) = authorize_removal(state, caller, id, user_id).await? else {
    return Ok(());
  };
  state.database.remove_conversation_member(id, user_id).await?;
  if user_id != caller.user.id {
    audit::record(
      state,
      Some(caller.user.id),
      "conversation_member_removed",
      Some(AuditTarget::new("conversation", id)),
      &json!({"user_id": user_id.to_string()}),
    )
    .await?;
  }
  state.hub.publish(
    members.iter().map(|m| m.user.id),
    &GatewayEvent::MembershipRemove {
      conversation_id: id,
      user_id,
    },
  );
  let kind = if user_id == caller.user.id {
    SystemEventKind::MemberLeft
  } else {
    SystemEventKind::MemberRemoved
  };
  system_events::emit(state, id, kind, Some(caller.user.id), Some(user_id)).await;
  super::calls::evict_from_call(state, id, user_id).await;
  Ok(())
}

/// Changes a member's role (needs `manage_channel_roles`) or the caller's own preferences.
pub async fn update_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  user_id: UserId,
  request: UpdateConversationMemberRequest,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, id).await?;
  let request_role = request.maybe_role.clone();
  let is_self = user_id == caller.user.id;
  let has_preferences = request.maybe_notification_level.is_some() || request.maybe_is_favorite.is_some();
  if has_preferences && !is_self {
    return Err(ApiError::Forbidden(
      "preferences can only be changed for yourself".into(),
    ));
  }
  if !access.is_member() && is_self {
    return Err(ApiError::Forbidden("not a member of this conversation".into()));
  }

  if let Some(role) = &request.maybe_role {
    ensure_open_channel(&access)?;
    require(state, caller, Permission::ManageChannelRoles, &access.target())?;
    if matches!(role, ChannelRole::Unknown(_)) {
      return Err(ApiError::InvalidRequest("unknown channel role".into()));
    }
    let members = state.database.list_conversation_members(id).await?;
    let Some(target) = members.iter().find(|m| m.user.id == user_id) else {
      return Err(ApiError::user_not_found());
    };
    if is_privileged(role) {
      // Guests of the instance or of the channel's space (as in `add_member`).
      let maybe_space_role = match access.view.conversation.maybe_space_id {
        Some(space) => state.database.space_role(space, user_id).await?,
        None => None,
      };
      if target.user.kind == teamcraft_proto::UserKind::Guest
        || matches!(maybe_space_role, None | Some(SpaceRole::Guest))
      {
        return Err(ApiError::InvalidRequest(
          "guests can't be channel admins or moderators".into(),
        ));
      }
    }
    if target.role == ChannelRole::Admin
      && *role != ChannelRole::Admin
      && state.database.count_channel_admins(id).await? <= 1
    {
      return Err(ApiError::Conflict("a channel needs at least one admin".into()));
    }
  }
  if let Some(level) = &request.maybe_notification_level
    && matches!(level, teamcraft_proto::NotificationLevel::Unknown(_))
  {
    return Err(ApiError::InvalidRequest("unknown notification level".into()));
  }

  state
    .database
    .update_membership(
      id,
      user_id,
      &MembershipChanges {
        maybe_role: request.maybe_role,
        maybe_notification_level: request.maybe_notification_level,
        maybe_is_favorite: request.maybe_is_favorite,
      },
    )
    .await?;
  if let Some(role) = &request_role {
    audit::record(
      state,
      Some(caller.user.id),
      "conversation_member_role_changed",
      Some(AuditTarget::new("conversation", id)),
      &json!({"user_id": user_id.to_string(), "role": role.as_str()}),
    )
    .await?;
    system_events::emit(
      state,
      id,
      SystemEventKind::RoleChanged,
      Some(caller.user.id),
      Some(user_id),
    )
    .await;
  }
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  Ok(())
}

pub async fn set_read_position(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: SetReadPositionRequest,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("not a member of this conversation".into()));
  }
  if state
    .database
    .get_message(id, request.message_id)
    .await?
    .is_none()
  {
    return Err(ApiError::InvalidRequest("unknown message".into()));
  }
  state
    .database
    .set_read_position(id, caller.user.id, request.message_id)
    .await?;
  // Other devices of the same user update their unread markers.
  state.hub.publish(
    [caller.user.id],
    &GatewayEvent::ConversationUpdate { conversation_id: id },
  );
  // The position is stored; a failed count must not fail the request (nor skip the push dismissal
  // and the read receipt below). The devices' next resync repairs the badge.
  if let Err(error) = threads::publish_unread(state, caller.user.id, id).await {
    warn!(conversation_id = %id, %error, "could not send the new unread counts");
  }
  // Phones that show a notification for what was just read remove it.
  tokio::spawn(crate::push::routing::dismiss_read(
    state.clone(),
    caller.user.id,
    id,
    None,
    request.message_id,
  ));
  notifications::publish_read_receipt(
    state,
    caller.user.id,
    &access.view.conversation,
    request.message_id,
  )
  .await?;
  Ok(())
}

/// The permission checks of a removal. `None` if `user_id` isn't a member (nothing left to do),
/// otherwise the current members.
async fn authorize_removal(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  user_id: UserId,
) -> Result<Option<Vec<teamcraft_db::ConversationMember>>, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_open_channel_for_leaving(&access)?;
  if user_id != caller.user.id {
    require(state, caller, Permission::ManageMembers, &access.target())?;
  }
  let members = state.database.list_conversation_members(id).await?;
  let Some(target) = members.iter().find(|m| m.user.id == user_id) else {
    return Ok(None);
  };
  // Moderators remove members; removing a moderator or an admin takes the right to manage roles.
  if user_id != caller.user.id && is_privileged(&target.role) {
    require(state, caller, Permission::ManageChannelRoles, &access.target())?;
  }
  if target.role == ChannelRole::Admin
    && members.len() > 1
    && state.database.count_channel_admins(id).await? <= 1
  {
    return Err(ApiError::Conflict(
      "make another member channel admin first".into(),
    ));
  }
  Ok(Some(members))
}

fn ensure_open_channel_for_leaving(access: &ConversationAccess) -> Result<(), ApiError> {
  if access.view.conversation.kind != teamcraft_proto::ConversationKind::Channel {
    return Err(ApiError::InvalidRequest(
      "not allowed for DMs and group DMs".into(),
    ));
  }
  Ok(())
}

async fn announce_added(
  state: &AppState,
  id: ConversationId,
  caller: &AuthenticatedUser,
) -> Result<(), ApiError> {
  announce_added_user(state, id, &caller.user).await
}

/// Tells the conversation's members (and the new one) that `user` is in.
pub(crate) async fn announce_added_user(
  state: &AppState,
  id: ConversationId,
  user: &teamcraft_db::User,
) -> Result<(), ApiError> {
  let members = state.database.conversation_member_ids(id).await?;
  state.hub.publish(
    members,
    &GatewayEvent::MembershipAdd {
      conversation_id: id,
      user: user_summary(state, user),
    },
  );
  Ok(())
}

/// Roles that give powers over other members: only somebody who manages roles hands them out or
/// removes their holders.
fn is_privileged(role: &ChannelRole) -> bool {
  matches!(role, ChannelRole::Admin | ChannelRole::Moderator)
}
