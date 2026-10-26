use std::collections::HashSet;

use serde_json::json;
use teamcraft_db::{
  ConversationChanges, ConversationFilter, ConversationView, NewConversation, StoredRetention,
  participants_key,
};
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, ConvertChannelRequest,
  CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, GatewayEvent, HistoryPolicy,
  ListConversationsQuery, ListConversationsResponse, MoveChannelRequest, Permission, SpaceId,
  SystemEventKind, Target, UpdateConversationRequest, UserId,
};

use super::access::{
  ConversationAccess, is_guest, is_instance_admin, load_conversation, load_space, require, space_target,
};
use super::audit::{self, Target as AuditTarget};
use super::convert::{ConversationExtras, conversation_response};
use super::system_events;
use super::{validate_name, validate_text};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Result of an operation that may create the conversation (DMs are created once per pair).
pub struct Created<T> {
  pub value: T,
  pub is_new: bool,
}

pub async fn list_conversations(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListConversationsQuery,
) -> Result<ListConversationsResponse, ApiError> {
  let filter = if query.joinable {
    // Channel browser: members only, guests don't see it (PLAN.md §1).
    let space = query
      .maybe_space_id
      .ok_or_else(|| ApiError::InvalidRequest("joinable needs space_id".into()))?;
    let access = load_space(state, caller, space).await?;
    if is_guest(caller) || access.view.maybe_role.is_none() {
      return Err(ApiError::Forbidden(
        "the channel browser is for members of the space".into(),
      ));
    }
    ConversationFilter {
      maybe_space: Some(space),
      only_joinable: true,
      ..ConversationFilter::default()
    }
  } else {
    ConversationFilter {
      maybe_space: query.maybe_space_id,
      only_joined: true,
      ..ConversationFilter::default()
    }
  };
  let views = state.database.list_conversations(caller.user.id, &filter).await?;
  Ok(ListConversationsResponse {
    conversations: respond_all(state, &views).await?,
  })
}

pub async fn get_conversation(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  let mut response = respond(state, &access.view).await?;
  if response.kind == ConversationKind::Meeting {
    response.maybe_organizer_user_id = state
      .database
      .get_meeting_by_conversation(id)
      .await?
      .map(|meeting| meeting.organizer);
  }
  Ok(response)
}

const INTEGRATION_STAYS_PRIVATE: &str = "integration channels carry machine output and stay private";

pub async fn create_channel(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  request: CreateChannelRequest,
) -> Result<ConversationResponse, ApiError> {
  let space_access = load_space(state, caller, space).await?;
  require(
    state,
    caller,
    Permission::CreateChannel,
    &space_target(Some(space), space_access.view.maybe_role.clone()),
  )?;
  if space_access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }
  let limits = &state.settings.get().limits;
  let name = validate_name(&request.name, "channel name", limits.max_name_length)?;
  for text in [&request.maybe_topic, &request.maybe_description]
    .into_iter()
    .flatten()
  {
    validate_text(text, "channel field", limits.max_text_length)?;
  }
  if let ConversationVisibility::Unknown(_) = request.visibility {
    return Err(ApiError::InvalidRequest("unknown visibility".into()));
  }
  if request.is_integration && request.visibility == ConversationVisibility::Public {
    return Err(ApiError::Conflict(INTEGRATION_STAYS_PRIVATE.into()));
  }
  if let Some(category) = request.maybe_category_id {
    let found = state.database.get_category(category).await?;
    if found.is_none_or(|c| c.space_id != space) {
      return Err(ApiError::category_not_found());
    }
  }

  let id: ConversationId = state.next_id()?;
  let created = state
    .database
    .create_channel(
      &NewConversation {
        id,
        maybe_space: Some(space),
        kind: ConversationKind::Channel,
        maybe_name: Some(&name),
        maybe_topic: request.maybe_topic.as_deref().filter(|t| !t.is_empty()),
        maybe_description: request.maybe_description.as_deref().filter(|t| !t.is_empty()),
        visibility: request.visibility,
        history_policy: request.maybe_history_policy.unwrap_or(HistoryPolicy::FromJoin),
        maybe_category: request.maybe_category_id,
        maybe_participants_key: None,
        is_integration: request.is_integration,
      },
      caller.user.id,
      request.maybe_idempotency_key.as_ref(),
    )
    .await?;
  // A retry of an earlier request: return that channel, nothing new to set up or announce.
  if created.is_new {
    if request.is_integration {
      apply_integration_defaults(state, id).await?;
    }
    state.hub.publish(
      [caller.user.id],
      &GatewayEvent::ConversationUpdate { conversation_id: id },
    );
  }
  get_conversation(state, caller, created.id).await
}

pub async fn create_dm(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateDmRequest,
) -> Result<Created<ConversationResponse>, ApiError> {
  require(state, caller, Permission::CreateDm, &Target::Instance)?;
  // A DM with oneself is the user's notes: its only participant is the caller, whose own devices
  // make up its encryption group. Opening it again returns the same conversation.
  let others = if request.user_id == caller.user.id {
    Vec::new()
  } else {
    vec![request.user_id]
  };
  open_participants_conversation(state, caller, ConversationKind::Dm, others).await
}

pub async fn create_group_dm(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateGroupDmRequest,
) -> Result<Created<ConversationResponse>, ApiError> {
  require(state, caller, Permission::CreateGroupDm, &Target::Instance)?;
  let others: Vec<UserId> = request
    .user_ids
    .into_iter()
    .filter(|id| *id != caller.user.id)
    .collect::<HashSet<_>>()
    .into_iter()
    .collect();
  if others.len() < 2 {
    return Err(ApiError::InvalidRequest(
      "a group DM needs at least two other people; use a DM for one".into(),
    ));
  }
  if others.len() + 1 > state.settings.get().limits.max_group_dm_participants {
    return Err(ApiError::InvalidRequest(format!(
      "a group DM has at most {} participants",
      state.settings.get().limits.max_group_dm_participants
    )));
  }
  open_participants_conversation(state, caller, ConversationKind::Group, others).await
}

pub async fn update_conversation(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: UpdateConversationRequest,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  if access.view.conversation.kind != ConversationKind::Channel {
    return Err(ApiError::InvalidRequest("only channels have settings".into()));
  }
  let target = access.target();
  require(state, caller, Permission::ManageChannelProperties, &target)?;
  // Making a channel public or private is a conversion with its own rules, audit record and system
  // event, whichever endpoint asks for it.
  if let Some(visibility) = request.maybe_visibility.clone()
    && visibility != access.view.conversation.visibility
  {
    convert_channel_privacy(
      state,
      caller,
      id,
      ConvertChannelRequest {
        visibility,
        maybe_history_policy: None,
      },
    )
    .await?;
  } else if matches!(request.maybe_visibility, Some(ConversationVisibility::Unknown(_))) {
    return Err(ApiError::InvalidRequest("unknown visibility".into()));
  }

  let limits = &state.settings.get().limits;
  let maybe_name = request
    .maybe_name
    .as_deref()
    .map(|n| validate_name(n, "channel name", limits.max_name_length))
    .transpose()?;
  for text in [&request.maybe_topic, &request.maybe_description]
    .into_iter()
    .flatten()
  {
    validate_text(text, "channel field", limits.max_text_length)?;
  }
  if let (Some(category), Some(space)) = (request.maybe_category_id, access.view.conversation.maybe_space_id)
  {
    let found = state.database.get_category(category).await?;
    if found.is_none_or(|c| c.space_id != space) {
      return Err(ApiError::category_not_found());
    }
  }

  if request.maybe_is_integration == Some(true)
    && access.view.conversation.visibility == ConversationVisibility::Public
  {
    return Err(ApiError::Conflict(INTEGRATION_STAYS_PRIVATE.into()));
  }

  let kinds = visible_changes(
    &access.view.conversation,
    maybe_name.as_deref(),
    request.maybe_is_archived,
    request.maybe_moderation.as_ref(),
  );

  state
    .database
    .update_conversation(
      id,
      &ConversationChanges {
        maybe_name: maybe_name.as_deref(),
        maybe_topic: request.maybe_topic.as_deref(),
        maybe_description: request.maybe_description.as_deref(),
        maybe_visibility: None,
        maybe_category: request.maybe_category_id,
        clear_category: request.clear_category,
        maybe_history_policy: request.maybe_history_policy,
        maybe_is_archived: request.maybe_is_archived,
        maybe_moderation: request.maybe_moderation,
        maybe_is_integration: request.maybe_is_integration,
      },
    )
    .await?;
  if request.maybe_is_integration == Some(true) && !access.view.conversation.is_integration {
    apply_integration_defaults(state, id).await?;
  }
  audit::record(
    state,
    Some(caller.user.id),
    "conversation_updated",
    Some(AuditTarget::new("conversation", id)),
    &json!({}),
  )
  .await?;
  for kind in kinds {
    system_events::emit(state, id, kind, Some(caller.user.id), None).await;
  }
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  get_conversation(state, caller, id).await
}

/// Moves a channel to another space. The MLS group, memberships and history are untouched: the
/// space only decides who may browse the channel and which scheme and retention defaults apply.
/// Needs `manage_space_properties` in the space the channel leaves and in the one it enters, and
/// every channel member must already belong to the target space: channel members are always space
/// members, so leaving or expiring from the space (guests) ends the channel membership too, and
/// guest restrictions follow the space role.
pub async fn move_channel(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: MoveChannelRequest,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  let conversation = &access.view.conversation;
  let Some(from) = conversation.maybe_space_id else {
    return Err(ApiError::InvalidRequest("only channels can be moved".into()));
  };
  if conversation.kind != ConversationKind::Channel {
    return Err(ApiError::InvalidRequest("only channels can be moved".into()));
  }
  if from == request.space_id {
    return Err(ApiError::InvalidRequest(
      "the channel is already in this space".into(),
    ));
  }
  if conversation.is_archived {
    return Err(ApiError::Conflict("unarchive the channel first".into()));
  }
  require(
    state,
    caller,
    Permission::ManageSpaceProperties,
    &space_target(Some(from), access.maybe_space_role.clone()),
  )?;
  let destination = load_space(state, caller, request.space_id).await?;
  require(
    state,
    caller,
    Permission::ManageSpaceProperties,
    &destination.target(),
  )?;
  if destination.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }
  if let Some(category) = request.maybe_category_id {
    let found = state.database.get_category(category).await?;
    if found.is_none_or(|found| found.space_id != request.space_id) {
      return Err(ApiError::category_not_found());
    }
  }
  let outside = members_outside_space(state, id, request.space_id).await?;
  if outside > 0 {
    return Err(ApiError::Conflict(format!(
      "{outside} channel members are not members of the target space; add them to the space first"
    )));
  }
  state
    .database
    .move_conversation_to_space(id, request.space_id, request.maybe_category_id)
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "channel_moved",
    Some(AuditTarget::new("conversation", id)),
    &json!({"from_space_id": from.to_string(), "to_space_id": request.space_id.to_string()}),
  )
  .await?;
  system_events::emit(state, id, SystemEventKind::Moved, Some(caller.user.id), None).await;
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  get_conversation(state, caller, id).await
}

/// How many members of the channel have no active role (guests: not expired) in the space.
async fn members_outside_space(
  state: &AppState,
  conversation: ConversationId,
  space: SpaceId,
) -> Result<usize, ApiError> {
  let in_space: HashSet<UserId> = state
    .database
    .list_space_members(space)
    .await?
    .into_iter()
    .map(|member| member.user.id)
    .collect();
  let members = state.database.conversation_member_ids(conversation).await?;
  Ok(members.iter().filter(|user| !in_space.contains(user)).count())
}

/// Makes a channel public or private. Memberships stay as they are; what changes is who may join
/// from now on and, with `history_policy`, what newcomers read (docs/e2ee.md §4.7).
pub async fn convert_channel_privacy(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: ConvertChannelRequest,
) -> Result<ConversationResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  if access.view.conversation.kind != ConversationKind::Channel {
    return Err(ApiError::InvalidRequest("only channels can be converted".into()));
  }
  require(state, caller, Permission::ConvertChannelPrivacy, &access.target())?;
  if matches!(request.visibility, ConversationVisibility::Unknown(_))
    || matches!(request.maybe_history_policy, Some(HistoryPolicy::Unknown(_)))
  {
    return Err(ApiError::InvalidRequest(
      "unknown visibility or history policy".into(),
    ));
  }
  if access.view.conversation.is_archived {
    return Err(ApiError::Conflict("unarchive the channel first".into()));
  }
  if access.view.conversation.is_integration && request.visibility == ConversationVisibility::Public {
    return Err(ApiError::Conflict(INTEGRATION_STAYS_PRIVATE.into()));
  }
  let before = access.view.conversation.visibility.clone();
  state
    .database
    .update_conversation(
      id,
      &ConversationChanges {
        maybe_visibility: Some(request.visibility.clone()),
        maybe_history_policy: request.maybe_history_policy.clone(),
        ..ConversationChanges::default()
      },
    )
    .await?;
  if before != request.visibility {
    audit::record(
      state,
      Some(caller.user.id),
      "conversation_visibility_changed",
      Some(AuditTarget::new("conversation", id)),
      &json!({"from": before.as_str(), "to": request.visibility.as_str()}),
    )
    .await?;
    system_events::emit(
      state,
      id,
      SystemEventKind::PrivacyChanged,
      Some(caller.user.id),
      None,
    )
    .await;
  }
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  get_conversation(state, caller, id).await
}

/// What members should be told about a settings change.
fn visible_changes(
  conversation: &teamcraft_db::Conversation,
  maybe_name: Option<&str>,
  maybe_is_archived: Option<bool>,
  maybe_moderation: Option<&teamcraft_proto::ChannelModeration>,
) -> Vec<SystemEventKind> {
  let mut kinds = Vec::new();
  if maybe_name.is_some_and(|name| conversation.maybe_name.as_deref() != Some(name)) {
    kinds.push(SystemEventKind::Renamed);
  }
  match maybe_is_archived {
    Some(true) if !conversation.is_archived => kinds.push(SystemEventKind::Archived),
    Some(false) if conversation.is_archived => kinds.push(SystemEventKind::Unarchived),
    _ => {},
  }
  if maybe_moderation.is_some_and(|moderation| *moderation != conversation.moderation) {
    kinds.push(SystemEventKind::ModerationChanged);
  }
  kinds
}

/// Retention suited to logs for a channel that just became an integration channel, unless the
/// channel already has its own setting (ARCHITECTURE.md §4.5).
async fn apply_integration_defaults(state: &AppState, id: ConversationId) -> Result<(), ApiError> {
  let days = state.settings.get().integration.default_retention_days;
  let Some(target) = state.database.get_conversation_retention(id).await? else {
    return Ok(());
  };
  if days > 0 && target.own == StoredRetention::default() {
    state
      .database
      .set_conversation_retention(
        id,
        StoredRetention {
          maybe_max_age_days: Some(i32::try_from(days).unwrap_or(i32::MAX)),
          maybe_max_messages: None,
        },
      )
      .await?;
  }
  Ok(())
}

/// Builds the response of a conversation with participants and the guest badge.
pub async fn respond(state: &AppState, view: &ConversationView) -> Result<ConversationResponse, ApiError> {
  let extras = extras_for(state, std::slice::from_ref(view)).await?;
  Ok(conversation_response(state, view, &extras))
}

async fn respond_all(
  state: &AppState,
  views: &[ConversationView],
) -> Result<Vec<ConversationResponse>, ApiError> {
  let extras = extras_for(state, views).await?;
  Ok(
    views
      .iter()
      .map(|view| conversation_response(state, view, &extras))
      .collect(),
  )
}

async fn extras_for(state: &AppState, views: &[ConversationView]) -> Result<ConversationExtras, ApiError> {
  let ids: Vec<ConversationId> = views.iter().map(|view| view.conversation.id).collect();
  Ok(ConversationExtras {
    participants: state.database.participants_of(&ids).await?,
    with_guests: state.database.conversations_with_guests(&ids).await?,
  })
}

async fn open_participants_conversation(
  state: &AppState,
  caller: &AuthenticatedUser,
  kind: ConversationKind,
  others: Vec<UserId>,
) -> Result<Created<ConversationResponse>, ApiError> {
  let users = state.database.get_users(&others).await?;
  if users.len() != others.len() || users.iter().any(|user| user.is_deactivated) {
    return Err(ApiError::user_not_found());
  }
  // A DM involving a guest needs a conversation the guest already shares with every other
  // participant (PLAN.md §12): in a group DM, two members who both know the guest still must not
  // meet a guest who knows only one of them.
  if super::workspace::effective_guest_dm_restriction(state) {
    let guests: Vec<UserId> = users
      .iter()
      .filter(|user| user.kind == teamcraft_proto::UserKind::Guest)
      .map(|user| user.id)
      .chain(is_guest(caller).then_some(caller.user.id))
      .collect();
    for guest in guests {
      let known = state.database.users_sharing_conversation(guest).await?;
      let everybody = others.iter().copied().chain([caller.user.id]);
      if everybody
        .filter(|participant| *participant != guest)
        .any(|participant| !known.contains(&participant))
      {
        return Err(ApiError::user_not_found());
      }
    }
  }

  let mut participants = others;
  participants.push(caller.user.id);
  let (id, is_new) = state
    .database
    .get_or_create_participants_conversation(
      &NewConversation {
        id: state.next_id()?,
        maybe_space: None,
        kind,
        maybe_name: None,
        maybe_topic: None,
        maybe_description: None,
        visibility: ConversationVisibility::Private,
        history_policy: HistoryPolicy::FromJoin,
        maybe_category: None,
        maybe_participants_key: Some(participants_key(&participants)),
        is_integration: false,
      },
      &participants,
    )
    .await?;
  if is_new {
    state.hub.publish(
      participants,
      &GatewayEvent::ConversationUpdate { conversation_id: id },
    );
  }
  let access = load_conversation(state, caller, id).await?;
  Ok(Created {
    value: respond(state, &access.view).await?,
    is_new,
  })
}

/// Checks the conversation is a channel that can still change (not a DM, not archived).
pub fn ensure_open_channel(access: &ConversationAccess) -> Result<(), ApiError> {
  let conversation = &access.view.conversation;
  if conversation.kind != ConversationKind::Channel {
    return Err(ApiError::InvalidRequest(
      "not allowed for DMs and group DMs".into(),
    ));
  }
  if conversation.is_archived {
    return Err(ApiError::Conflict("the channel is archived".into()));
  }
  Ok(())
}

/// Instance admins can look at channel metadata of all spaces; everyone else only at their own.
pub fn can_list_members(access: &ConversationAccess, caller: &AuthenticatedUser) -> bool {
  access.is_member() || is_instance_admin(caller)
}
