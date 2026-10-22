//! Notification preferences: per user (DND schedule, push mode, receipts, thread follow) and per
//! conversation (level, mute time). The push routing reads them (`push::routing`).

use teamcraft_db::{Conversation, StoredSpaceNotifications, StoredUserSettings};
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationNotificationSettings, DndSchedule, GatewayEvent,
  MINUTES_PER_DAY, MessageId, NotificationLevel, NotificationSettings, PushMode, ReadReceipt,
  ReadReceiptsResponse, SpaceId, SpaceNotificationSettings, UserId,
};
use time::OffsetDateTime;

use super::access::{load_conversation, load_space};
use super::convert::{offset_date_time, space_notification_settings, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_DND_WINDOWS: usize = 14;
const MAX_DND_ALLOWED_USERS: usize = 50;
const MAX_UTC_OFFSET_MINUTES: i16 = 14 * 60;

pub async fn get_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<NotificationSettings, ApiError> {
  let stored = state
    .database
    .get_user_notification_settings(caller.user.id)
    .await?;
  settings_response(state, &stored)
}

/// Replaces all of the caller's preferences.
pub async fn put_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: NotificationSettings,
) -> Result<NotificationSettings, ApiError> {
  if matches!(request.push_mode, PushMode::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown push_mode".into()));
  }
  if let Some(schedule) = &request.maybe_dnd_schedule {
    validate_schedule(schedule)?;
  }
  let maybe_dnd_schedule = request
    .maybe_dnd_schedule
    .as_ref()
    .map(serde_json::to_string)
    .transpose()
    .map_err(|error| ApiError::Internal(format!("serializing the DND schedule failed: {error}")))?;
  let stored = StoredUserSettings {
    maybe_auto_follow_mentioned_threads: Some(request.auto_follow_mentioned_threads),
    push_mode: request.push_mode.as_str().to_owned(),
    maybe_send_read_receipts: Some(request.send_read_receipts),
    maybe_dnd_schedule,
  };
  state
    .database
    .set_user_notification_settings(caller.user.id, &stored)
    .await?;
  settings_response(state, &stored)
}

pub async fn get_conversation_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ConversationNotificationSettings, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  let membership = access
    .view
    .maybe_membership
    .ok_or_else(|| ApiError::Forbidden("not a member of this conversation".into()))?;
  Ok(ConversationNotificationSettings {
    level: membership.notification_level,
    maybe_muted_until: membership.maybe_muted_until.map(unix_millis),
    is_inherited: membership.is_notification_inherited,
  })
}

/// Sets the caller's notification level and mute time for one conversation.
pub async fn put_conversation_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: ConversationNotificationSettings,
) -> Result<ConversationNotificationSettings, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("not a member of this conversation".into()));
  }
  if request.is_inherited {
    state
      .database
      .clear_conversation_notifications(conversation, caller.user.id)
      .await?;
    return get_conversation_settings(state, caller, conversation).await;
  }
  if matches!(request.level, NotificationLevel::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown notification level".into()));
  }
  let maybe_muted_until = request
    .maybe_muted_until
    .map(|millis| {
      offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("muted_until is out of range".into()))
    })
    .transpose()?;
  state
    .database
    .set_conversation_notifications(conversation, caller.user.id, &request.level, maybe_muted_until)
    .await?;
  Ok(request)
}

/// The caller's notification setting for a whole space.
pub async fn get_space_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<SpaceNotificationSettings, ApiError> {
  let access = load_space(state, caller, space).await?;
  if access.view.maybe_role.is_none() {
    return Err(ApiError::Forbidden("not a member of this space".into()));
  }
  Ok(space_notification_settings(&access.view.notifications))
}

/// Sets the caller's notification level and mute time for a whole space: the channels in it follow
/// unless the caller configured them individually.
pub async fn put_space_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  request: SpaceNotificationSettings,
) -> Result<SpaceNotificationSettings, ApiError> {
  get_space_settings(state, caller, space).await?;
  if matches!(request.maybe_level, Some(NotificationLevel::Unknown(_))) {
    return Err(ApiError::InvalidRequest("unknown notification level".into()));
  }
  let maybe_muted_until = request
    .maybe_muted_until
    .map(|millis| {
      offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("muted_until is out of range".into()))
    })
    .transpose()?;
  let stored = StoredSpaceNotifications {
    maybe_level: request.maybe_level,
    maybe_muted_until,
  };
  state
    .database
    .set_space_notifications(space, caller.user.id, &stored)
    .await?;
  Ok(space_notification_settings(&stored))
}

/// The read positions of the members who send receipts, for a caller who sends them too.
pub async fn get_read_receipts(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ReadReceiptsResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("not a member of this conversation".into()));
  }
  let mut receipts = Vec::new();
  if has_receipts(&access.view.conversation) {
    let members = state
      .database
      .list_receipt_members(conversation, state.settings.notifications.read_receipts_default)
      .await?;
    if members.iter().any(|member| member.user == caller.user.id) {
      receipts = members
        .into_iter()
        .filter(|member| member.user != caller.user.id)
        .filter_map(|member| {
          member.maybe_last_read.map(|message_id| ReadReceipt {
            user_id: member.user,
            message_id,
          })
        })
        .collect();
    }
  }
  Ok(ReadReceiptsResponse {
    conversation_id: conversation,
    receipts,
  })
}

/// Tells the other receipt-sending members of a DM or group DM that `reader` read up to `message`.
pub async fn publish_read_receipt(
  state: &AppState,
  reader: UserId,
  conversation: &Conversation,
  message: MessageId,
) -> Result<(), ApiError> {
  if !has_receipts(conversation) {
    return Ok(());
  }
  let members = state
    .database
    .list_receipt_members(
      conversation.id,
      state.settings.notifications.read_receipts_default,
    )
    .await?;
  if !members.iter().any(|member| member.user == reader) {
    return Ok(());
  }
  state.hub.publish(
    members
      .into_iter()
      .map(|member| member.user)
      .filter(|user| *user != reader),
    &GatewayEvent::ReadReceipt {
      conversation_id: conversation.id,
      user_id: reader,
      message_id: message,
    },
  );
  Ok(())
}

/// Receipts exist only in DMs and group DMs.
fn has_receipts(conversation: &Conversation) -> bool {
  matches!(conversation.kind, ConversationKind::Dm | ConversationKind::Group)
}

/// Whether the user's receipts are on, with the instance default for users who never chose.
pub fn receipts_enabled(state: &AppState, stored: &StoredUserSettings) -> bool {
  stored
    .maybe_send_read_receipts
    .unwrap_or(state.settings.notifications.read_receipts_default)
}

/// Whether a conversation is muted by time at `now`.
pub fn is_muted(maybe_muted_until: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
  maybe_muted_until.is_some_and(|until| until > now)
}

fn settings_response(
  state: &AppState,
  stored: &StoredUserSettings,
) -> Result<NotificationSettings, ApiError> {
  let maybe_dnd_schedule = stored
    .maybe_dnd_schedule
    .as_deref()
    .map(serde_json::from_str::<DndSchedule>)
    .transpose()
    .map_err(|error| ApiError::Internal(format!("stored DND schedule is invalid: {error}")))?;
  Ok(NotificationSettings {
    auto_follow_mentioned_threads: stored
      .maybe_auto_follow_mentioned_threads
      .unwrap_or(state.policy.get().threads.auto_follows_mentioned_threads),
    push_mode: PushMode::from(stored.push_mode.clone()),
    send_read_receipts: receipts_enabled(state, stored),
    maybe_dnd_schedule,
  })
}

fn validate_schedule(schedule: &DndSchedule) -> Result<(), ApiError> {
  if schedule.utc_offset_minutes.abs() > MAX_UTC_OFFSET_MINUTES {
    return Err(ApiError::InvalidRequest(
      "utc_offset_minutes is out of range".into(),
    ));
  }
  if schedule.windows.len() > MAX_DND_WINDOWS {
    return Err(ApiError::InvalidRequest("too many DND windows".into()));
  }
  if schedule.allow_user_ids.len() > MAX_DND_ALLOWED_USERS {
    return Err(ApiError::InvalidRequest("too many allowed users".into()));
  }
  for window in &schedule.windows {
    if window.weekdays.is_empty()
      || window
        .weekdays
        .iter()
        .any(|day| matches!(day, teamcraft_proto::Weekday::Unknown(_)))
    {
      return Err(ApiError::InvalidRequest(
        "a DND window needs valid weekdays".into(),
      ));
    }
    if window.start_minute >= MINUTES_PER_DAY
      || window.end_minute >= MINUTES_PER_DAY
      || window.start_minute == window.end_minute
    {
      return Err(ApiError::InvalidRequest(
        "DND window minutes must be below 1440 and start and end must differ".into(),
      ));
    }
  }
  Ok(())
}
