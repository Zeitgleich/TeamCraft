//! Presence (ARCHITECTURE.md §5.3): device activity aggregated per user, merged with the manual choice.

use std::collections::HashSet;

use teamcraft_db::{PresencePreference, StoredLastSeen, StoredStatus};
use teamcraft_proto::{
  CustomStatus, DeviceId, GatewayEvent, LastSeenVisibility, ListPresenceResponse, ManualPresence,
  PresenceActivity, PresenceState, PrivacyResponse, SetCustomStatusRequest, SetPresenceRequest,
  SetPresenceResponse, UpdatePrivacyRequest, UserId, UserPresence,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::is_guest;
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::gateway::{DeviceAggregate, PresenceSnapshot};
use crate::state::AppState;

const MAX_PRESENCE_QUERY_USERS: usize = 200;

/// What other users see, from the user's manual choice and their devices.
///
/// Appearing offline hides everything. Do-not-disturb wins over activity while it lasts, a manual
/// away while any device is connected. Otherwise the user is online if any device is active, away
/// if devices are connected but idle, else offline.
pub fn derive_state(
  preference: &PresencePreference,
  devices: DeviceAggregate,
  now: OffsetDateTime,
) -> PresenceState {
  let is_dnd_active = preference.manual == ManualPresence::DoNotDisturb
    && preference.maybe_dnd_until.is_none_or(|until| until > now);
  if preference.manual == ManualPresence::Invisible {
    return PresenceState::Offline;
  }
  if is_dnd_active && devices != DeviceAggregate::NoDevice {
    return PresenceState::DoNotDisturb;
  }
  match devices {
    DeviceAggregate::NoDevice => PresenceState::Offline,
    // A manual away stays until the user picks another status, whatever the devices report.
    _ if preference.manual == ManualPresence::Away => PresenceState::Away,
    DeviceAggregate::Active => PresenceState::Online,
    DeviceAggregate::Idle => PresenceState::Away,
  }
}

/// The status others see: the one the user set, else what the server derived (a call, a meeting).
/// Nothing if the workspace switched statuses off, and nothing derived for users who appear offline.
async fn status_of(
  state: &AppState,
  user: UserId,
  presence: &PresenceState,
  now: OffsetDateTime,
) -> Result<Option<CustomStatus>, ApiError> {
  if !state.policy.get().presence.statuses_enabled {
    return Ok(None);
  }
  if let Some(status) = state
    .database
    .get_custom_statuses(&[user], now)
    .await?
    .remove(&user)
  {
    return Ok(Some(status_response(&status)));
  }
  if *presence == PresenceState::Offline {
    return Ok(None);
  }
  derived_status(state, user, now).await
}

/// "In a call" beats "in a meeting"; a meeting known to TeamCraft beats a busy period of the calendar.
async fn derived_status(
  state: &AppState,
  user: UserId,
  now: OffsetDateTime,
) -> Result<Option<CustomStatus>, ApiError> {
  if state
    .maybe_calls
    .as_ref()
    .is_some_and(|calls| calls.is_in_call(user))
  {
    return Ok(Some(derived(PresenceActivity::InCall, None)));
  }
  let maybe_meeting_end = state.database.users_in_meeting(&[user], now).await?.remove(&user);
  let maybe_end = maybe_meeting_end.or_else(|| state.hub.busy_until(user, now));
  Ok(maybe_end.map(|end| derived(PresenceActivity::InMeeting, Some(end))))
}

fn derived(activity: PresenceActivity, maybe_end: Option<OffsetDateTime>) -> CustomStatus {
  CustomStatus {
    maybe_emoji: None,
    maybe_text: None,
    maybe_expires_at: maybe_end.map(unix_millis),
    maybe_activity: Some(activity),
  }
}

/// Turns devices that have shown no sign of activity since `cutoff` into idle ones (the workspace's
/// away timeout) and updates the presence of their users. Returns how many users changed.
pub async fn demote_idle_devices(state: &AppState, cutoff: tokio::time::Instant) -> Result<usize, ApiError> {
  let users = state.hub.demote_idle(cutoff);
  for user in &users {
    refresh_presence(state, *user).await?;
  }
  Ok(users.len())
}

/// Re-evaluates the derived statuses of everybody online; with `polls_calendar` the calendar
/// server is asked first who is busy. Returns how many users were looked at.
pub async fn refresh_derived_statuses(
  state: &AppState,
  now: OffsetDateTime,
  polls_calendar: bool,
) -> Result<usize, ApiError> {
  let users = state.hub.online_users();
  if polls_calendar {
    poll_calendar(state, &users, now).await;
  }
  for user in &users {
    refresh_presence(state, *user).await?;
  }
  Ok(users.len())
}

/// Asks the calendar server when the given users are busy. A failure only means the old answers stay.
async fn poll_calendar(state: &AppState, users: &[UserId], now: OffsetDateTime) {
  let Some(calendar) = state
    .maybe_meetings
    .as_ref()
    .and_then(|meetings| meetings.calendar())
  else {
    return;
  };
  let lookahead = time::Duration::seconds(
    i64::try_from(state.settings.get().presence.calendar_lookahead_secs).unwrap_or(i64::MAX / 4),
  );
  let emails = match state.database.get_user_emails(users).await {
    Ok(emails) => emails,
    Err(error) => {
      warn!(%error, "reading the e-mail addresses for the calendar poll failed");
      return;
    },
  };
  for (user, email) in emails {
    match calendar.busy_periods(&email, now, now + lookahead).await {
      Ok(periods) => state
        .hub
        .set_busy_periods(user, periods.into_iter().map(|p| (p.start, p.end)).collect()),
      Err(error) => warn!(%error, user_id = %user, "asking the calendar for busy periods failed"),
    }
  }
}

/// The user did something on `device` themselves (sent a message): the device counts as in use
/// at once, whatever it last reported, and everybody learns the user is online (unless a manual
/// status says otherwise).
pub async fn note_user_action(state: &AppState, user: UserId, device: DeviceId) -> Result<(), ApiError> {
  match state.hub.note_user_action(user, device) {
    Some(user) => refresh_presence(state, user).await,
    None => Ok(()),
  }
}

/// Recomputes the user's presence and tells everyone who shares a conversation if it changed.
///
/// One refresh per user at a time, each reading the devices afresh: overlapping refreshes (two
/// devices, the periodic refresh, a disconnect) can't announce a state older than the newest.
pub async fn refresh_presence(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let _refreshing = state.hub.presence_refresh_lock(user).lock().await;
  let preference = state.database.get_presence_preference(user).await?;
  let now = OffsetDateTime::now_utc();
  let aggregate = state.hub.device_aggregate(user);
  // Last seen is the moment the user stopped being active (idle or gone). Not recorded while they
  // appear offline or hide it: a time from that period could show once they stop hiding.
  if state.hub.swap_aggregate(user, aggregate) == Some(DeviceAggregate::Active)
    && aggregate != DeviceAggregate::Active
    && preference.manual != ManualPresence::Invisible
    && state.database.get_last_seen_visibility(user).await? == LastSeenVisibility::Everyone
  {
    state.database.set_last_seen(user, now).await?;
  }
  let presence = derive_state(&preference, aggregate, now);
  let maybe_status = status_of(state, user, &presence, now).await?;
  let snapshot = PresenceSnapshot {
    state: presence.clone(),
    maybe_status: maybe_status.clone(),
  };
  if state.hub.record_presence(user, snapshot) {
    let mut audience: HashSet<UserId> = state.database.users_sharing_conversation(user).await?;
    audience.insert(user);
    state.hub.publish(
      audience,
      &GatewayEvent::PresenceUpdate {
        user_id: user,
        state: presence,
        maybe_status,
      },
    );
  }
  Ok(())
}

pub async fn set_presence(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SetPresenceRequest,
) -> Result<SetPresenceResponse, ApiError> {
  if matches!(request.manual, ManualPresence::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown presence choice".into()));
  }
  let maybe_until = match (&request.manual, request.maybe_until) {
    (ManualPresence::DoNotDisturb, Some(millis)) => {
      let until = offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid until".into()))?;
      if until <= OffsetDateTime::now_utc() {
        return Err(ApiError::InvalidRequest("until must be in the future".into()));
      }
      Some(until)
    },
    (ManualPresence::DoNotDisturb, None) => None,
    (_, Some(_)) => {
      return Err(ApiError::InvalidRequest(
        "until only applies to do-not-disturb".into(),
      ));
    },
    (_, None) => None,
  };
  let preference = PresencePreference {
    manual: request.manual.clone(),
    maybe_dnd_until: maybe_until,
  };
  state
    .database
    .set_presence_preference(caller.user.id, &preference)
    .await?;
  refresh_presence(state, caller.user.id).await?;
  Ok(SetPresenceResponse {
    manual: request.manual,
    maybe_until: maybe_until.map(unix_millis),
  })
}

/// Presence of the requested users. Guests only learn about people they share a conversation with.
pub async fn list_presence(
  state: &AppState,
  caller: &AuthenticatedUser,
  user_ids: &str,
) -> Result<ListPresenceResponse, ApiError> {
  let requested = parse_user_ids(user_ids)?;
  let visible: Vec<UserId> = if is_guest(caller) {
    let sharing = state.database.users_sharing_conversation(caller.user.id).await?;
    requested
      .into_iter()
      .filter(|id| *id == caller.user.id || sharing.contains(id))
      .collect()
  } else {
    requested
  };
  // Custom statuses come from the database: users who are offline still show theirs.
  let are_statuses_enabled = state.policy.get().presence.statuses_enabled;
  let mut statuses = if are_statuses_enabled {
    state
      .database
      .get_custom_statuses(&visible, OffsetDateTime::now_utc())
      .await?
  } else {
    std::collections::HashMap::new()
  };
  let is_last_seen_enabled = state.policy.get().presence.is_last_seen_enabled;
  let mut last_seen = if is_last_seen_enabled {
    state.database.get_last_seen(&visible).await?
  } else {
    std::collections::HashMap::new()
  };
  Ok(ListPresenceResponse {
    presences: visible
      .into_iter()
      .map(|user_id| {
        let snapshot = state.hub.last_presence(user_id);
        let maybe_last_seen = last_seen
          .remove(&user_id)
          .and_then(|stored| shown_last_seen(&snapshot.state, &stored))
          .map(unix_millis);
        let maybe_status = statuses
          .remove(&user_id)
          .map(|status| status_response(&status))
          .or_else(|| {
            snapshot
              .maybe_status
              .filter(|status| status.maybe_activity.is_some() && are_statuses_enabled)
          });
        UserPresence {
          user_id,
          state: snapshot.state,
          maybe_status,
          maybe_last_seen,
        }
      })
      .collect(),
  })
}

/// What others may see of a user's last activity: only while the user is offline, never when they
/// hide it or appear offline (the time would give the invisibility away).
fn shown_last_seen(presence: &PresenceState, stored: &StoredLastSeen) -> Option<OffsetDateTime> {
  let is_hidden = stored.is_invisible || stored.visibility != LastSeenVisibility::Everyone;
  if *presence != PresenceState::Offline || is_hidden {
    return None;
  }
  stored.maybe_at
}

/// The caller's privacy choices.
pub async fn get_my_privacy(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<PrivacyResponse, ApiError> {
  Ok(PrivacyResponse {
    last_seen: state.database.get_last_seen_visibility(caller.user.id).await?,
    is_last_seen_available: state.policy.get().presence.is_last_seen_enabled,
  })
}

pub async fn put_my_privacy(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UpdatePrivacyRequest,
) -> Result<PrivacyResponse, ApiError> {
  if matches!(request.last_seen, LastSeenVisibility::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown last_seen choice".into()));
  }
  state
    .database
    .set_last_seen_visibility(caller.user.id, &request.last_seen)
    .await?;
  get_my_privacy(state, caller).await
}

fn parse_user_ids(text: &str) -> Result<Vec<UserId>, ApiError> {
  let ids = text
    .split(',')
    .map(str::trim)
    .filter(|part| !part.is_empty())
    .map(|part| {
      part
        .parse::<UserId>()
        .map_err(|_| ApiError::InvalidRequest(format!("invalid user ID {part:?}")))
    })
    .collect::<Result<Vec<_>, _>>()?;
  if ids.len() > MAX_PRESENCE_QUERY_USERS {
    return Err(ApiError::InvalidRequest(format!(
      "at most {MAX_PRESENCE_QUERY_USERS} users per request"
    )));
  }
  Ok(ids)
}

const MAX_EMOJI_CHARS: usize = 32;

/// Sets the caller's custom status (PLAN.md §4) and tells everybody who sees their presence.
pub async fn set_custom_status(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SetCustomStatusRequest,
) -> Result<CustomStatus, ApiError> {
  let emoji = request
    .maybe_emoji
    .as_deref()
    .map(str::trim)
    .filter(|e| !e.is_empty());
  let text = request
    .maybe_text
    .as_deref()
    .map(str::trim)
    .filter(|t| !t.is_empty());
  let policy = state.policy.get();
  if !policy.presence.statuses_enabled {
    return Err(ApiError::Forbidden("statuses are switched off".into()));
  }
  let maybe_preset = policy.presence.presets.iter().find(|preset| {
    // A preset's empty emoji is no emoji, as in the request.
    preset
      .maybe_emoji
      .as_deref()
      .map(str::trim)
      .filter(|e| !e.is_empty())
      == emoji
      && Some(preset.text.trim()) == text
  });
  if !policy.presence.custom_status_enabled && maybe_preset.is_none() {
    return Err(ApiError::Forbidden(
      "custom statuses are switched off; choose one of the presets".into(),
    ));
  }
  if emoji.is_none() && text.is_none() {
    return Err(ApiError::InvalidRequest(
      "a status needs an emoji or a text".into(),
    ));
  }
  if emoji.is_some_and(|e| e.chars().count() > MAX_EMOJI_CHARS || e.chars().any(char::is_control)) {
    return Err(ApiError::InvalidRequest("the emoji is invalid".into()));
  }
  if let Some(text) = text {
    super::validate_name(text, "status text", state.settings.get().limits.max_name_length)?;
  }
  let now = OffsetDateTime::now_utc();
  let maybe_expires_at = match (request.maybe_expires_in_seconds, request.maybe_expires_at) {
    // A duration is resolved on this clock, whatever the client's clock says.
    (Some(seconds), _) => {
      let at = i64::try_from(seconds)
        .ok()
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| now.checked_add(time::Duration::seconds(seconds)))
        .ok_or_else(|| ApiError::InvalidRequest("invalid expires_in_seconds".into()))?;
      Some(at)
    },
    (None, Some(millis)) => {
      let at =
        offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
      if at <= now {
        return Err(ApiError::InvalidRequest(
          "expires_at must be in the future".into(),
        ));
      }
      Some(at)
    },
    // A preset can end by itself.
    (None, None) => maybe_preset
      .and_then(|preset| preset.maybe_duration_minutes)
      .map(|minutes| now + time::Duration::minutes(i64::from(minutes))),
  };

  let stored = StoredStatus {
    maybe_emoji: emoji.map(str::to_owned),
    maybe_text: text.map(str::to_owned),
    maybe_expires_at,
  };
  state.database.set_custom_status(caller.user.id, &stored).await?;
  refresh_presence(state, caller.user.id).await?;
  Ok(status_response(&stored))
}

pub async fn clear_custom_status(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  state.database.clear_custom_status(caller.user.id).await?;
  refresh_presence(state, caller.user.id).await
}

/// Removes statuses that expired at `now` and tells the audience. The server runs this periodically.
pub async fn expire_statuses(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let users = state.database.delete_expired_statuses(now).await?;
  for user in &users {
    refresh_presence(state, *user).await?;
  }
  Ok(users.len())
}

fn status_response(status: &StoredStatus) -> CustomStatus {
  CustomStatus {
    maybe_emoji: status.maybe_emoji.clone(),
    maybe_text: status.maybe_text.clone(),
    maybe_expires_at: status.maybe_expires_at.map(unix_millis),
    maybe_activity: None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
  }

  fn preference(manual: ManualPresence, until: Option<i64>) -> PresencePreference {
    PresencePreference {
      manual,
      maybe_dnd_until: until.map(|seconds| now() + time::Duration::seconds(seconds)),
    }
  }

  #[test]
  fn follows_device_activity_without_manual_choice() {
    let auto = preference(ManualPresence::Auto, None);
    assert_eq!(
      derive_state(&auto, DeviceAggregate::Active, now()),
      PresenceState::Online
    );
    assert_eq!(
      derive_state(&auto, DeviceAggregate::Idle, now()),
      PresenceState::Away
    );
    assert_eq!(
      derive_state(&auto, DeviceAggregate::NoDevice, now()),
      PresenceState::Offline
    );
  }

  #[test]
  fn a_manual_away_sticks_whatever_the_devices_report() {
    let away = preference(ManualPresence::Away, None);
    assert_eq!(
      derive_state(&away, DeviceAggregate::Active, now()),
      PresenceState::Away
    );
    assert_eq!(
      derive_state(&away, DeviceAggregate::Idle, now()),
      PresenceState::Away
    );
    assert_eq!(
      derive_state(&away, DeviceAggregate::NoDevice, now()),
      PresenceState::Offline
    );
    // Back to automatic: activity decides again.
    let auto = preference(ManualPresence::Auto, None);
    assert_eq!(
      derive_state(&auto, DeviceAggregate::Active, now()),
      PresenceState::Online
    );
  }

  #[test]
  fn appearing_offline_hides_everything() {
    let invisible = preference(ManualPresence::Invisible, None);
    for devices in [
      DeviceAggregate::Active,
      DeviceAggregate::Idle,
      DeviceAggregate::NoDevice,
    ] {
      assert_eq!(derive_state(&invisible, devices, now()), PresenceState::Offline);
    }
  }

  #[test]
  fn do_not_disturb_wins_while_connected_and_until_it_expires() {
    let open_ended = preference(ManualPresence::DoNotDisturb, None);
    assert_eq!(
      derive_state(&open_ended, DeviceAggregate::Active, now()),
      PresenceState::DoNotDisturb
    );
    assert_eq!(
      derive_state(&open_ended, DeviceAggregate::Idle, now()),
      PresenceState::DoNotDisturb
    );
    assert_eq!(
      derive_state(&open_ended, DeviceAggregate::NoDevice, now()),
      PresenceState::Offline
    );

    let timed = preference(ManualPresence::DoNotDisturb, Some(600));
    assert_eq!(
      derive_state(&timed, DeviceAggregate::Active, now()),
      PresenceState::DoNotDisturb
    );
    let expired = preference(ManualPresence::DoNotDisturb, Some(-1));
    assert_eq!(
      derive_state(&expired, DeviceAggregate::Active, now()),
      PresenceState::Online
    );
  }

  mod last_seen {
    use super::*;

    fn stored(visibility: LastSeenVisibility, is_invisible: bool) -> StoredLastSeen {
      StoredLastSeen {
        maybe_at: Some(now()),
        visibility,
        is_invisible,
      }
    }

    #[test]
    fn it_is_shown_for_offline_users_who_allow_it() {
      let shown = stored(LastSeenVisibility::Everyone, false);
      assert_eq!(shown_last_seen(&PresenceState::Offline, &shown), Some(now()));
      for state in [
        PresenceState::Online,
        PresenceState::Away,
        PresenceState::DoNotDisturb,
      ] {
        assert_eq!(shown_last_seen(&state, &shown), None);
      }
    }

    #[test]
    fn it_is_hidden_when_the_user_chose_so_or_appears_offline() {
      let nobody = stored(LastSeenVisibility::Nobody, false);
      assert_eq!(shown_last_seen(&PresenceState::Offline, &nobody), None);
      let invisible = stored(LastSeenVisibility::Everyone, true);
      assert_eq!(shown_last_seen(&PresenceState::Offline, &invisible), None);
      let unknown = stored(LastSeenVisibility::Unknown("friends".into()), false);
      assert_eq!(shown_last_seen(&PresenceState::Offline, &unknown), None);
      let never_active = StoredLastSeen {
        maybe_at: None,
        visibility: LastSeenVisibility::Everyone,
        is_invisible: false,
      };
      assert_eq!(shown_last_seen(&PresenceState::Offline, &never_active), None);
    }
  }

  #[test]
  fn user_id_lists_are_parsed_strictly() {
    assert_eq!(parse_user_ids("1, 2,3").unwrap().len(), 3);
    assert!(parse_user_ids("").unwrap().is_empty());
    assert!(parse_user_ids("1,x").is_err());
    let too_many = (1..=201).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    assert!(parse_user_ids(&too_many).is_err());
  }
}
