//! Conversions between TeamCraft IDs and the 8-byte IDs DAVE puts into MLS.
//!
//! A call participant is one device: its DAVE member id is the `DeviceId`, the DAVE group id is the
//! `CallId`. Snowflakes are non-negative 63-bit values, so they fit into `u64` both ways.

use teamcraft_media::dave::{DaveCallId, DaveParticipantId};
use teamcraft_proto::{CallId, DeviceId, Id, IdError, Snowflake};

pub fn dave_participant_id(device_id: DeviceId) -> DaveParticipantId {
  DaveParticipantId(snowflake_to_u64(device_id.snowflake()))
}

pub fn dave_call_id(call_id: CallId) -> DaveCallId {
  DaveCallId(snowflake_to_u64(call_id.snowflake()))
}

/// Reverse of [`dave_participant_id`]; fails for ids above the 63-bit snowflake range.
pub fn device_id_from_dave(participant_id: DaveParticipantId) -> Result<DeviceId, IdError> {
  let raw = i64::try_from(participant_id.0).map_err(|_| IdError::Malformed(participant_id.0.to_string()))?;
  Ok(DeviceId::from_snowflake(Snowflake::new(raw)?))
}

fn snowflake_to_u64(snowflake: Snowflake) -> u64 {
  // Invariant: snowflakes are non-negative, so the conversion never fails.
  u64::try_from(snowflake.get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn device_id_round_trips_through_dave() {
    let device_id: DeviceId = "9007199254740993".parse().unwrap();
    assert_eq!(
      device_id_from_dave(dave_participant_id(device_id)).unwrap(),
      device_id
    );
  }

  #[test]
  fn dave_id_above_snowflake_range_is_rejected() {
    assert!(device_id_from_dave(DaveParticipantId(u64::MAX)).is_err());
  }

  #[test]
  fn call_id_maps_to_group_id() {
    let call_id: CallId = "77".parse().unwrap();
    assert_eq!(dave_call_id(call_id), DaveCallId(77));
  }
}
