//! IDs used by the SFU. Participants are devices (`teamcraft_proto::DeviceId`); `CallId` stays local
//! until the calls module in `teamcraft-proto` exists.

use serde::{Deserialize, Serialize};
use teamcraft_media::dave::DaveParticipantId;
pub use teamcraft_proto::DeviceId;
use teamcraft_proto::{Id, IdError, Snowflake};

/// A call (one DAVE group, one SFU room).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub u64);

/// DAVE identifies members by an 8-byte id in their MLS credential; TeamCraft uses the device id.
pub fn dave_participant_id(device_id: DeviceId) -> DaveParticipantId {
  // Invariant: snowflakes are non-negative, so the conversion to u64 never fails.
  DaveParticipantId(u64::try_from(device_id.snowflake().get()).unwrap_or_default())
}

/// Reverse of [`dave_participant_id`]; fails for ids above the 63-bit snowflake range.
pub fn device_id_from_dave(participant_id: DaveParticipantId) -> Result<DeviceId, IdError> {
  let raw = i64::try_from(participant_id.0).map_err(|_| IdError::Malformed(participant_id.0.to_string()))?;
  Ok(DeviceId::from_snowflake(Snowflake::new(raw)?))
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
}
