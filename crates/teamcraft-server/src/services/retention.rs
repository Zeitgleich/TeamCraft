//! Retention (PLAN.md §1, admin.md §3.1): keep the last N days and/or the last N messages.
//!
//! The signed workspace policy holds the default and the limit; a space or channel can set its own
//! value within the limit. A background job deletes what is past retention: ciphertext, attachments,
//! search index segments, and emptied month partitions. Pinned messages survive if the policy says so.

use serde_json::json;
use teamcraft_db::{RetentionTarget, StoredRetention};
use teamcraft_proto::{
  ConversationId, Id, MessageId, Permission, RetentionSetting, RetentionStatusResponse, SNOWFLAKE_EPOCH_MS,
  SetRetentionRequest, Snowflake, SpaceId,
};
use time::OffsetDateTime;
use tracing::{info, warn};

use super::access::{load_conversation, load_space, require};
use super::audit::{self, Target as AuditTarget};
use super::workspace::current_policy;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// What one run of the job removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
  pub messages_deleted: u64,
  pub attachments_deleted: u64,
  pub index_segments_deleted: u64,
  pub partitions_dropped: u64,
}

pub async fn get_space_retention(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<RetentionStatusResponse, ApiError> {
  load_space(state, caller, space).await?;
  let own = state.database.get_space_retention(space).await?;
  status(state, own, StoredRetention::default()).await
}

pub async fn set_space_retention(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  request: SetRetentionRequest,
) -> Result<RetentionStatusResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  let own = to_stored(request.retention)?;
  check_within_limit(state, own, StoredRetention::default()).await?;
  state.database.set_space_retention(space, own).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "retention_changed",
    Some(AuditTarget::new("space", space)),
    &json!({"max_age_days": own.maybe_max_age_days, "max_messages": own.maybe_max_messages}),
  )
  .await?;
  status(state, own, StoredRetention::default()).await
}

pub async fn get_conversation_retention(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<RetentionStatusResponse, ApiError> {
  load_conversation(state, caller, conversation).await?;
  let target = target(state, conversation).await?;
  status(state, target.own, target.space).await
}

pub async fn set_conversation_retention(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: SetRetentionRequest,
) -> Result<RetentionStatusResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  require(
    state,
    caller,
    Permission::ManageChannelProperties,
    &access.target(),
  )?;
  let current = target(state, conversation).await?;
  let own = to_stored(request.retention)?;
  check_within_limit(state, own, current.space).await?;
  state
    .database
    .set_conversation_retention(conversation, own)
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "retention_changed",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"max_age_days": own.maybe_max_age_days, "max_messages": own.maybe_max_messages}),
  )
  .await?;
  super::system_events::emit(
    state,
    conversation,
    teamcraft_proto::SystemEventKind::RetentionChanged,
    Some(caller.user.id),
    None,
  )
  .await;
  status(state, own, current.space).await
}

/// Pins or unpins a message: pinned messages survive retention if the policy keeps them.
pub async fn pin_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
  is_pinned: bool,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  require(state, caller, Permission::ManageBookmarks, &access.target())?;
  if state
    .database
    .set_message_pinned(conversation, message, is_pinned)
    .await?
  {
    Ok(())
  } else {
    Err(ApiError::InvalidRequest("unknown message".into()))
  }
}

/// Runs the retention job once with `now` as the current time (injectable for tests).
pub async fn run_once(state: &AppState, now: OffsetDateTime) -> Result<RetentionReport, ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  let retention = policy.retention;
  let mut report = RetentionReport::default();

  for target in state.database.list_retention_targets().await? {
    // The limit also holds for settings made before it was lowered.
    let effective = limited(
      effective(target.own, target.space, retention.default),
      retention.limit,
    );
    if effective.maybe_max_age_days.is_none() && effective.maybe_max_messages.is_none() {
      continue;
    }
    if let Err(error) = enforce(state, &target, effective, retention.keep_pinned, now, &mut report).await {
      // One broken conversation must not stop the others; the next run tries again.
      warn!(conversation_id = %target.conversation, %error, "retention failed for a conversation");
    }
  }

  let dropped = state.database.drop_empty_message_partitions(now).await?;
  report.partitions_dropped = dropped.len() as u64;
  state
    .database
    .ensure_partitions_at(now, state.settings.retention.partitions_ahead_months)
    .await?;

  if report != RetentionReport::default() {
    info!(?report, "retention run finished");
    audit::record(
      state,
      None,
      "retention_run",
      None::<AuditTarget<'_>>,
      &json!({
        "messages": report.messages_deleted,
        "attachments": report.attachments_deleted,
        "index_segments": report.index_segments_deleted,
        "partitions": report.partitions_dropped,
      }),
    )
    .await?;
  }
  Ok(report)
}

async fn enforce(
  state: &AppState,
  target: &RetentionTarget,
  effective: StoredRetention,
  keep_pinned: bool,
  now: OffsetDateTime,
  report: &mut RetentionReport,
) -> Result<(), ApiError> {
  let conversation = target.conversation;
  let age_cutoff = effective
    .maybe_max_age_days
    .map(|days| snowflake_floor(now - time::Duration::days(i64::from(days))));
  let count_cutoff = match effective.maybe_max_messages {
    Some(keep) => state
      .database
      .nth_newest_message_id(conversation, keep)
      .await?
      .map(|id| id.snowflake()),
    None => None,
  };
  // The stricter rule wins: everything older than the later cutoff goes.
  let Some(cutoff) = [age_cutoff, count_cutoff].into_iter().flatten().max() else {
    return Ok(());
  };
  let before = MessageId::from_snowflake(cutoff);

  for key in state
    .database
    .list_offloaded_ciphertext_before(conversation, before, keep_pinned)
    .await?
  {
    delete_object(state, &key).await?;
  }
  report.messages_deleted += state
    .database
    .delete_messages_before(conversation, before, keep_pinned)
    .await?;

  // Attachments aren't linked to messages on the server, so a conversation with pinned messages
  // keeps all its attachments (a pinned message may still need one).
  if !(keep_pinned && state.database.conversation_has_pinned(conversation).await?) {
    for object in state
      .database
      .list_attachments_before(conversation, before)
      .await?
    {
      delete_object(state, &object.s3_key).await?;
      state.database.delete_attachment_row(object.id).await?;
      report.attachments_deleted += 1;
    }
  }
  let cutoff_time =
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(cutoff.unix_millis()) * 1_000_000).unwrap_or(now);
  for object in state
    .database
    .list_index_segments_before(conversation, cutoff_time)
    .await?
  {
    delete_object(state, &object.s3_key).await?;
    state.database.delete_index_segment_row(object.id).await?;
    report.index_segments_deleted += 1;
  }
  Ok(())
}

async fn delete_object(state: &AppState, key: &str) -> Result<(), ApiError> {
  if let Some(store) = &state.maybe_blob_store {
    store
      .delete_object(key)
      .await
      .map_err(|e| ApiError::Unavailable(format!("storage: {e}")))?;
  }
  Ok(())
}

/// The smallest Snowflake ID created at or after `time` (same math as `tc_snowflake_floor`).
pub fn snowflake_floor(time: OffsetDateTime) -> Snowflake {
  let millis = i64::try_from(time.unix_timestamp_nanos() / 1_000_000).unwrap_or(0) - SNOWFLAKE_EPOCH_MS;
  // Clamped to a non-negative value below 2^63, so the conversion always succeeds.
  Snowflake::new(millis.clamp(0, (1 << 41) - 1) << 22).unwrap_or(Snowflake::ZERO)
}

/// The setting cut down to the workspace limit: a value above it counts as the limit, an unlimited
/// one as well. Settings stay as stored, so raising the limit again gives them back.
pub fn limited(setting: StoredRetention, limit: RetentionSetting) -> StoredRetention {
  StoredRetention {
    maybe_max_age_days: at_most(
      setting.maybe_max_age_days,
      limit.maybe_max_age_days.and_then(|days| i32::try_from(days).ok()),
    ),
    maybe_max_messages: at_most(
      setting.maybe_max_messages,
      limit
        .maybe_max_messages
        .and_then(|count| i64::try_from(count).ok()),
    ),
  }
}

fn at_most<T: Ord>(maybe_value: Option<T>, maybe_limit: Option<T>) -> Option<T> {
  match (maybe_value, maybe_limit) {
    (Some(value), Some(limit)) => Some(value.min(limit)),
    (None, Some(limit)) => Some(limit),
    (maybe_value, None) => maybe_value,
  }
}

/// Per field: the object's own value, else its space's, else the default.
pub fn effective(own: StoredRetention, space: StoredRetention, default: RetentionSetting) -> StoredRetention {
  StoredRetention {
    maybe_max_age_days: own.maybe_max_age_days.or(space.maybe_max_age_days).or(
      default
        .maybe_max_age_days
        .and_then(|days| i32::try_from(days).ok()),
    ),
    maybe_max_messages: own.maybe_max_messages.or(space.maybe_max_messages).or(
      default
        .maybe_max_messages
        .and_then(|count| i64::try_from(count).ok()),
    ),
  }
}

async fn target(state: &AppState, conversation: ConversationId) -> Result<RetentionTarget, ApiError> {
  state
    .database
    .get_conversation_retention(conversation)
    .await?
    .ok_or_else(ApiError::conversation_not_found)
}

async fn status(
  state: &AppState,
  own: StoredRetention,
  space: StoredRetention,
) -> Result<RetentionStatusResponse, ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  let effective = limited(
    effective(own, space, policy.retention.default),
    policy.retention.limit,
  );
  Ok(RetentionStatusResponse {
    own: to_setting(own),
    effective: to_setting(effective),
    limit: policy.retention.limit,
    keep_pinned: policy.retention.keep_pinned,
  })
}

/// The resulting effective retention must respect the workspace limit.
async fn check_within_limit(
  state: &AppState,
  own: StoredRetention,
  inherited: StoredRetention,
) -> Result<(), ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  let effective = effective(own, inherited, policy.retention.default);
  within_limit(effective, policy.retention.limit)
}

fn within_limit(effective: StoredRetention, limit: RetentionSetting) -> Result<(), ApiError> {
  if let Some(max_days) = limit.maybe_max_age_days {
    match effective.maybe_max_age_days {
      Some(days) if u32::try_from(days).is_ok_and(|days| days <= max_days) => {},
      _ => {
        return Err(ApiError::InvalidRequest(format!(
          "messages may be kept at most {max_days} days"
        )));
      },
    }
  }
  if let Some(max_messages) = limit.maybe_max_messages {
    match effective.maybe_max_messages {
      Some(count) if u64::try_from(count).is_ok_and(|count| count <= max_messages) => {},
      _ => {
        return Err(ApiError::InvalidRequest(format!(
          "at most {max_messages} messages may be kept"
        )));
      },
    }
  }
  Ok(())
}

fn to_stored(setting: RetentionSetting) -> Result<StoredRetention, ApiError> {
  if setting.maybe_max_age_days == Some(0) || setting.maybe_max_messages == Some(0) {
    return Err(ApiError::InvalidRequest(
      "retention values must be positive".into(),
    ));
  }
  Ok(StoredRetention {
    maybe_max_age_days: setting
      .maybe_max_age_days
      .map(|days| {
        i32::try_from(days).map_err(|_| ApiError::InvalidRequest("max_age_days is too large".into()))
      })
      .transpose()?,
    maybe_max_messages: setting
      .maybe_max_messages
      .map(|count| {
        i64::try_from(count).map_err(|_| ApiError::InvalidRequest("max_messages is too large".into()))
      })
      .transpose()?,
  })
}

fn to_setting(stored: StoredRetention) -> RetentionSetting {
  RetentionSetting {
    maybe_max_age_days: stored
      .maybe_max_age_days
      .and_then(|days| u32::try_from(days).ok()),
    maybe_max_messages: stored
      .maybe_max_messages
      .and_then(|count| u64::try_from(count).ok()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn stored(days: Option<i32>, messages: Option<i64>) -> StoredRetention {
    StoredRetention {
      maybe_max_age_days: days,
      maybe_max_messages: messages,
    }
  }

  #[test]
  fn the_most_specific_value_wins_per_field() {
    let default = RetentionSetting {
      maybe_max_age_days: Some(90),
      maybe_max_messages: Some(1000),
    };
    let space = stored(Some(60), None);
    let own = stored(None, Some(50));
    let result = effective(own, space, default);
    assert_eq!(
      (result.maybe_max_age_days, result.maybe_max_messages),
      (Some(60), Some(50))
    );
    let none = effective(
      stored(None, None),
      stored(None, None),
      RetentionSetting::default(),
    );
    assert_eq!(none, stored(None, None));
  }

  #[test]
  fn settings_beyond_a_lowered_limit_count_as_the_limit() {
    let limit = RetentionSetting {
      maybe_max_age_days: Some(30),
      maybe_max_messages: None,
    };
    assert_eq!(
      limited(stored(Some(90), Some(5)), limit),
      stored(Some(30), Some(5))
    );
    assert_eq!(limited(stored(None, None), limit), stored(Some(30), None));
    assert_eq!(limited(stored(Some(7), None), limit), stored(Some(7), None));
    assert_eq!(
      limited(stored(Some(90), None), RetentionSetting::default()),
      stored(Some(90), None)
    );
  }

  #[test]
  fn limits_require_a_value_within_them() {
    let limit = RetentionSetting {
      maybe_max_age_days: Some(30),
      maybe_max_messages: None,
    };
    assert!(within_limit(stored(Some(30), None), limit).is_ok());
    assert!(within_limit(stored(Some(31), None), limit).is_err());
    assert!(
      within_limit(stored(None, Some(5)), limit).is_err(),
      "unlimited age exceeds the limit"
    );
    assert!(within_limit(stored(None, None), RetentionSetting::default()).is_ok());
  }

  #[test]
  fn zero_values_are_rejected() {
    assert!(
      to_stored(RetentionSetting {
        maybe_max_age_days: Some(0),
        maybe_max_messages: None
      })
      .is_err()
    );
    assert!(
      to_stored(RetentionSetting {
        maybe_max_age_days: None,
        maybe_max_messages: Some(0)
      })
      .is_err()
    );
  }

  #[test]
  fn the_floor_matches_the_snowflake_layout() {
    let time = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let floor = snowflake_floor(time);
    assert_eq!(floor.unix_millis(), 1_800_000_000_000);
    assert_eq!((floor.worker(), floor.sequence()), (0, 0));
    assert_eq!(
      snowflake_floor(OffsetDateTime::UNIX_EPOCH).get(),
      0,
      "before the epoch clamps to zero"
    );
  }
}
