//! Profiles and avatars.

use teamcraft_proto::UserId;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredProfile {
  pub maybe_title: Option<String>,
  pub maybe_department: Option<String>,
  pub maybe_phone: Option<String>,
  pub maybe_time_zone: Option<String>,
  pub maybe_working_hours: Option<StoredWorkingHours>,
  /// The person's own picture.
  pub maybe_avatar: Option<StoredAvatar>,
  /// The identity provider's picture (issue #2071), a PNG the server made.
  pub maybe_provider_avatar: Option<StoredAvatar>,
}

/// Working hours: weekdays as a bit mask (bit 0 = Monday) and minutes since local midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredWorkingHours {
  pub weekday_mask: u8,
  pub start_minute: u16,
  pub end_minute: u16,
}

/// A stored avatar object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAvatar {
  pub key: String,
  pub content_type: String,
  pub size_bytes: u64,
}

/// The type of every picture the server made itself.
const PROVIDER_AVATAR_CONTENT_TYPE: &str = "image/png";

impl Database {
  pub async fn get_profile(&self, user: UserId) -> Result<StoredProfile, DbError> {
    let row = sqlx::query!(
      r#"SELECT title, department, phone, time_zone, working_weekdays, working_start_minute,
                working_end_minute, avatar_key, avatar_content_type, avatar_size_bytes,
                provider_avatar_key, provider_avatar_size_bytes
         FROM user_profiles WHERE user_id = $1"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    let Some(row) = row else {
      return Ok(StoredProfile::default());
    };
    Ok(StoredProfile {
      maybe_title: row.title,
      maybe_department: row.department,
      maybe_phone: row.phone,
      maybe_time_zone: row.time_zone,
      maybe_working_hours: working_hours(
        row.working_weekdays,
        row.working_start_minute,
        row.working_end_minute,
      ),
      maybe_avatar: avatar(row.avatar_key, row.avatar_content_type, row.avatar_size_bytes)?,
      maybe_provider_avatar: avatar(
        row.provider_avatar_key,
        Some(PROVIDER_AVATAR_CONTENT_TYPE.to_owned()),
        row.provider_avatar_size_bytes,
      )?,
    })
  }

  /// Replaces the text fields of the profile (the avatar stays).
  pub async fn put_profile(&self, user: UserId, profile: &StoredProfile) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO user_profiles (user_id, title, department, phone, time_zone, working_weekdays,
                                    working_start_minute, working_end_minute)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (user_id) DO UPDATE
         SET title = $2, department = $3, phone = $4, time_zone = $5, working_weekdays = $6,
             working_start_minute = $7, working_end_minute = $8, updated_at = now()"#,
      to_db(user),
      profile.maybe_title,
      profile.maybe_department,
      profile.maybe_phone,
      profile.maybe_time_zone,
      profile
        .maybe_working_hours
        .map(|hours| i16::from(hours.weekday_mask)),
      profile
        .maybe_working_hours
        .map(|hours| i16::try_from(hours.start_minute).unwrap_or(0)),
      profile
        .maybe_working_hours
        .map(|hours| i16::try_from(hours.end_minute).unwrap_or(0)),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Makes `avatar` the person's own picture. Returns the replaced object's key.
  pub async fn replace_avatar(&self, user: UserId, avatar: &StoredAvatar) -> Result<Option<String>, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO user_profiles AS x (user_id, avatar_key, avatar_content_type, avatar_size_bytes)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE
         SET avatar_key = $2, avatar_content_type = $3, avatar_size_bytes = $4, updated_at = now()
         RETURNING (SELECT avatar_key FROM user_profiles WHERE user_id = $1) AS previous"#,
      to_db(user),
      avatar.key,
      avatar.content_type,
      i64::try_from(avatar.size_bytes).unwrap_or(i64::MAX),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(row.previous.filter(|previous| *previous != avatar.key))
  }

  /// What the identity provider's picture of the person was made from, if there is one.
  pub async fn provider_picture_source(&self, user: UserId) -> Result<Option<String>, DbError> {
    let row = sqlx::query!(
      "SELECT provider_picture_source FROM user_profiles WHERE user_id = $1",
      to_db(user)
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.and_then(|row| row.provider_picture_source))
  }

  /// Sets the identity provider's picture made from `source`, or removes it (`None`). Returns the
  /// replaced object's key.
  pub async fn set_provider_avatar(
    &self,
    user: UserId,
    maybe_picture: Option<(&StoredAvatar, &str)>,
  ) -> Result<Option<String>, DbError> {
    let (maybe_key, maybe_size, maybe_source) = match maybe_picture {
      Some((avatar, source)) => (
        Some(avatar.key.as_str()),
        Some(i64::try_from(avatar.size_bytes).unwrap_or(i64::MAX)),
        Some(source),
      ),
      None => (None, None, None),
    };
    let row = sqlx::query!(
      r#"INSERT INTO user_profiles AS x (user_id, provider_avatar_key, provider_avatar_size_bytes,
                                         provider_picture_source)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE
         SET provider_avatar_key = $2, provider_avatar_size_bytes = $3, provider_picture_source = $4,
             updated_at = now()
         RETURNING (SELECT provider_avatar_key FROM user_profiles WHERE user_id = $1) AS previous"#,
      to_db(user),
      maybe_key,
      maybe_size,
      maybe_source,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(
      row
        .previous
        .filter(|previous| Some(previous.as_str()) != maybe_key),
    )
  }

  /// Removes the current avatar. Returns its key.
  pub async fn clear_avatar(&self, user: UserId) -> Result<Option<String>, DbError> {
    let row = sqlx::query!(
      r#"UPDATE user_profiles x SET avatar_key = NULL, avatar_content_type = NULL,
                avatar_size_bytes = NULL, updated_at = now()
         FROM user_profiles old
         WHERE x.user_id = $1 AND old.user_id = x.user_id
         RETURNING old.avatar_key"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.and_then(|row| row.avatar_key))
  }
}

fn working_hours(
  maybe_weekdays: Option<i16>,
  maybe_start: Option<i16>,
  maybe_end: Option<i16>,
) -> Option<StoredWorkingHours> {
  Some(StoredWorkingHours {
    weekday_mask: u8::try_from(maybe_weekdays?).ok()?,
    start_minute: u16::try_from(maybe_start?).ok()?,
    end_minute: u16::try_from(maybe_end?).ok()?,
  })
}

fn avatar(
  maybe_key: Option<String>,
  maybe_content_type: Option<String>,
  maybe_size: Option<i64>,
) -> Result<Option<StoredAvatar>, DbError> {
  match (maybe_key, maybe_content_type, maybe_size) {
    (Some(key), Some(content_type), Some(size)) => Ok(Some(StoredAvatar {
      key,
      content_type,
      size_bytes: u64::try_from(size).map_err(|_| DbError::InvalidStoredValue("avatar size".into()))?,
    })),
    _ => Ok(None),
  }
}
