use std::collections::HashSet;

use serde_json::json;
use teamcraft_db::{
  ConversationChanges, ConversationFilter, ConversationView, NewConversation, participants_key,
};
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, CreateChannelRequest,
  CreateDmRequest, CreateGroupDmRequest, GatewayEvent, HistoryPolicy, ListConversationsQuery,
  ListConversationsResponse, Permission, SpaceId, Target, UpdateConversationRequest, UserId,
};

use super::access::{
  ConversationAccess, is_guest, is_instance_admin, load_conversation, load_space, require, space_target,
};
use super::audit::{self, Target as AuditTarget};
use super::convert::{ConversationExtras, conversation_response};
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
  respond(state, &access.view).await
}

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
    &space_target(space_access.view.maybe_role.clone()),
  )?;
  if space_access.view.space.is_archived {
    return Err(ApiError::Conflict("the space is archived".into()));
  }
  let limits = &state.settings.limits;
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
  if let Some(category) = request.maybe_category_id {
    let found = state.database.get_category(category).await?;
    if found.is_none_or(|c| c.space_id != space) {
      return Err(ApiError::category_not_found());
    }
  }

  let id: ConversationId = state.next_id()?;
  state
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
      },
      caller.user.id,
    )
    .await?;
  state.hub.publish(
    [caller.user.id],
    &GatewayEvent::ConversationUpdate { conversation_id: id },
  );
  get_conversation(state, caller, id).await
}

pub async fn create_dm(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateDmRequest,
) -> Result<Created<ConversationResponse>, ApiError> {
  require(state, caller, Permission::CreateDm, &Target::Instance)?;
  if request.user_id == caller.user.id {
    return Err(ApiError::InvalidRequest("can't open a DM with yourself".into()));
  }
  open_participants_conversation(state, caller, ConversationKind::Dm, vec![request.user_id]).await
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
  if others.len() + 1 > state.settings.limits.max_group_dm_participants {
    return Err(ApiError::InvalidRequest(format!(
      "a group DM has at most {} participants",
      state.settings.limits.max_group_dm_participants
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
  if let Some(visibility) = &request.maybe_visibility {
    if matches!(visibility, ConversationVisibility::Unknown(_)) {
      return Err(ApiError::InvalidRequest("unknown visibility".into()));
    }
    if *visibility != access.view.conversation.visibility {
      require(state, caller, Permission::ConvertChannelPrivacy, &target)?;
    }
  }

  let limits = &state.settings.limits;
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

  state
    .database
    .update_conversation(
      id,
      &ConversationChanges {
        maybe_name: maybe_name.as_deref(),
        maybe_topic: request.maybe_topic.as_deref(),
        maybe_description: request.maybe_description.as_deref(),
        maybe_visibility: request.maybe_visibility,
        maybe_category: request.maybe_category_id,
        clear_category: request.clear_category,
        maybe_history_policy: request.maybe_history_policy,
        maybe_is_archived: request.maybe_is_archived,
        maybe_moderation: request.maybe_moderation,
      },
    )
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "conversation_updated",
    Some(AuditTarget::new("conversation", id)),
    &json!({}),
  )
  .await?;
  let members = state.database.conversation_member_ids(id).await?;
  state
    .hub
    .publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
  get_conversation(state, caller, id).await
}

/// Builds the response of a conversation with participants and the guest badge.
pub async fn respond(state: &AppState, view: &ConversationView) -> Result<ConversationResponse, ApiError> {
  let extras = extras_for(state, std::slice::from_ref(view)).await?;
  Ok(conversation_response(view, &extras))
}

async fn respond_all(
  state: &AppState,
  views: &[ConversationView],
) -> Result<Vec<ConversationResponse>, ApiError> {
  let extras = extras_for(state, views).await?;
  Ok(
    views
      .iter()
      .map(|view| conversation_response(view, &extras))
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
  let sharing = if state.settings.policy.guests_dm_only_with_shared_conversations {
    Some(state.database.users_sharing_conversation(caller.user.id).await?)
  } else {
    None
  };
  let users = state.database.get_users(&others).await?;
  if users.len() != others.len() || users.iter().any(|user| user.is_deactivated) {
    return Err(ApiError::user_not_found());
  }
  for user in &users {
    // A DM involving a guest needs a conversation the two already share (PLAN.md §12).
    let involves_guest = is_guest(caller) || user.kind == teamcraft_proto::UserKind::Guest;
    if involves_guest && sharing.as_ref().is_some_and(|shared| !shared.contains(&user.id)) {
      return Err(ApiError::user_not_found());
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
