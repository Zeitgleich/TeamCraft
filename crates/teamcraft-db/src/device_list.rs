use teamcraft_proto::UserId;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// A user's signed device list; opaque to the database.
#[derive(Debug, Clone)]
pub struct DeviceList {
  pub sequence: u64,
  pub signed_list: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceListOutcome {
  Stored,
  /// The stored sequence differs from the expected one (0 if there is none).
  VersionConflict {
    current_sequence: u64,
  },
}

impl Database {
  pub async fn get_device_list(&self, user: UserId) -> Result<Option<DeviceList>, DbError> {
    let row = sqlx::query!(
      "SELECT sequence, signed_list FROM device_lists WHERE user_id = $1",
      to_db(user)
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| DeviceList {
      sequence: u64::try_from(row.sequence).unwrap_or(0),
      signed_list: row.signed_list,
    }))
  }

  /// Compare-and-swap on the sequence number: creates the list (`expected_sequence` 0) or replaces it
  /// if it is still at `expected_sequence`, in one statement so concurrent writers can't both win.
  /// The caller makes sure `sequence` is above `expected_sequence`.
  pub async fn put_device_list(
    &self,
    user: UserId,
    expected_sequence: u64,
    sequence: u64,
    signed_list: &[u8],
  ) -> Result<DeviceListOutcome, DbError> {
    let expected =
      i64::try_from(expected_sequence).map_err(|_| DbError::InvalidStoredValue("sequence".into()))?;
    let new = i64::try_from(sequence).map_err(|_| DbError::InvalidStoredValue("sequence".into()))?;
    let stored = if expected == 0 {
      sqlx::query_scalar!(
        r#"INSERT INTO device_lists (user_id, sequence, signed_list) VALUES ($1, $2, $3)
           ON CONFLICT DO NOTHING RETURNING sequence"#,
        to_db(user),
        new,
        signed_list,
      )
      .fetch_optional(self.pool())
      .await?
    } else {
      sqlx::query_scalar!(
        r#"UPDATE device_lists SET sequence = $3, signed_list = $4, updated_at = now()
           WHERE user_id = $1 AND sequence = $2 RETURNING sequence"#,
        to_db(user),
        expected,
        new,
        signed_list,
      )
      .fetch_optional(self.pool())
      .await?
    };
    if stored.is_some() {
      return Ok(DeviceListOutcome::Stored);
    }
    let current = sqlx::query_scalar!(
      "SELECT sequence FROM device_lists WHERE user_id = $1",
      to_db(user)
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(DeviceListOutcome::VersionConflict {
      current_sequence: current.map_or(0, |v| u64::try_from(v).unwrap_or(0)),
    })
  }
}
