//! Presence (ARCHITECTURE.md §5.3): device activity aggregated per user, merged with the manual choice.

use std::collections::HashSet;

use teamcraft_db::PresencePreference;
use teamcraft_proto::{
  GatewayEvent, ListPresenceResponse, ManualPresence, PresenceState, SetPresenceRequest, SetPresenceResponse,
  UserId, UserPresence,
};
use time::OffsetDateTime;

use super::access::is_guest;
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::gateway::DeviceAggregate;
use crate::state::AppState;

const MAX_PRESENCE_QUERY_USERS: usize = 200;

/// What other users see, from the user's manual choice and their devices.
///
/// Appearing offline hides everything. Do-not-disturb wins over activity while it lasts. Otherwise
/// the user is online if any device is active, away if devices are connected but idle, else offline.
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
    DeviceAggregate::Active => PresenceState::Online,
    DeviceAggregate::Idle => PresenceState::Away,
    DeviceAggregate::NoDevice => PresenceState::Offline,
  }
}

/// Recomputes the user's presence and tells everyone who shares a conversation if it changed.
pub async fn refresh_presence(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let preference = state.database.get_presence_preference(user).await?;
  let presence = derive_state(
    &preference,
    state.hub.device_aggregate(user),
    OffsetDateTime::now_utc(),
  );
  if state.hub.record_presence(user, presence.clone()) {
    let mut audience: HashSet<UserId> = state.database.users_sharing_conversation(user).await?;
    audience.insert(user);
    state.hub.publish(
      audience,
      &GatewayEvent::PresenceUpdate {
        user_id: user,
        state: presence,
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
  Ok(ListPresenceResponse {
    presences: visible
      .into_iter()
      .map(|user_id| UserPresence {
        user_id,
        state: state.hub.last_presence(user_id),
      })
      .collect(),
  })
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

  #[test]
  fn user_id_lists_are_parsed_strictly() {
    assert_eq!(parse_user_ids("1, 2,3").unwrap().len(), 3);
    assert!(parse_user_ids("").unwrap().is_empty());
    assert!(parse_user_ids("1,x").is_err());
    let too_many = (1..=201).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    assert!(parse_user_ids(&too_many).is_err());
  }
}
