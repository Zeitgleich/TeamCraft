//! Profiles (title, department, phone, time zone, working hours) and avatars (PLAN.md §4, §11).
//!
//! Avatars are plain images (everybody who sees the person sees them). Uploads come to the server,
//! which decodes them and stores a square PNG it encoded itself (no metadata, a size it chose);
//! downloads go through presigned URLs like attachments. A person can also have a picture from
//! the identity provider ([`super::provider_avatars`]); the workspace policy decides which one
//! shows (issue #2071).

use std::collections::HashSet;
use std::time::Duration;

use axum::body::Bytes;
use teamcraft_db::{StoredAvatar, StoredProfile, StoredWorkingHours};
use teamcraft_proto::{
  AvatarContentType, AvatarResponse, AvatarSource, DirectoryPolicy, GatewayEvent, Id, ProfileResponse,
  UpdateProfileRequest, UserId, WorkingHours,
};
use time::OffsetDateTime;
use tracing::warn;

use super::convert::unix_millis;
use super::directory::visible_user;
use super::space_images::{WhenBusy, run_limited, sanitize_avatar};
use super::storage_cleanup::delete_or_queue;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage::BlobStore;
use crate::time_zone;
use teamcraft_proto::Weekday;

const MAX_PHONE_CHARS: usize = 40;
const MAX_TIME_ZONE_CHARS: usize = 64;
const MINUTES_PER_DAY: u16 = 1440;

pub async fn get_my_profile(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ProfileResponse, ApiError> {
  profile_of(state, caller.user.id).await
}

/// The profile of somebody the caller may know about.
pub async fn get_profile(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<ProfileResponse, ApiError> {
  let visible = visible_user(state, caller, user).await?;
  profile_of(state, visible.id).await
}

/// Replaces the caller's profile; fields left out are cleared.
pub async fn put_my_profile(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UpdateProfileRequest,
) -> Result<ProfileResponse, ApiError> {
  let limits = state.settings.get().limits.clone();
  let limit = limits.max_profile_text_chars;
  let current = state.database.get_profile(caller.user.id).await?;
  let maybe_name = clean_display_name(request.maybe_display_name, limits.max_display_name_chars)?;
  let maybe_current_name = state.database.get_display_name_override(caller.user.id).await?;
  let profile = StoredProfile {
    maybe_title: clean_text(request.maybe_title, limit, "title")?,
    maybe_department: clean_text(request.maybe_department, limit, "department")?,
    maybe_phone: clean_phone(request.maybe_phone)?,
    maybe_time_zone: clean_time_zone(request.maybe_time_zone)?,
    maybe_working_hours: request.maybe_working_hours.map(stored_hours).transpose()?,
    maybe_avatar: current.maybe_avatar,
    maybe_provider_avatar: current.maybe_provider_avatar,
  };
  // Clearing is always allowed, and so is sending back the override already stored: only a new
  // name is subject to the policy.
  if maybe_name.is_some()
    && maybe_name != maybe_current_name
    && !state.policy.get().directory.allow_display_name_override
  {
    return Err(ApiError::DisplayNameOverrideNotAllowed);
  }
  state.database.put_profile(caller.user.id, &profile).await?;
  if maybe_name != maybe_current_name {
    state
      .database
      .set_display_name_override(caller.user.id, maybe_name.as_deref())
      .await?;
  }
  publish_update(state, caller.user.id).await?;
  profile_of(state, caller.user.id).await
}

/// Makes the uploaded picture the caller's own: checked and re-encoded as a square PNG here
/// ([`sanitize_avatar`]), whatever the client sent.
pub async fn put_my_avatar(
  state: &AppState,
  caller: &AuthenticatedUser,
  declared_type: &str,
  bytes: Bytes,
) -> Result<ProfileResponse, ApiError> {
  let profile = state.database.get_profile(caller.user.id).await?;
  if !can_change_avatar(&state.policy.get().directory, &profile) {
    return Err(ApiError::AvatarFromIdentityProvider);
  }
  // While the storage is down, say "later" before decoding a picture that can't be kept.
  blob_store(state)?.check_available()?;
  let png = sanitized_avatar(state, declared_type.to_owned(), bytes, WhenBusy::Refuse).await?;
  store_avatar(state, caller.user.id, png).await?;
  profile_of(state, caller.user.id).await
}

/// Decodes and re-encodes a picture within the configured limits, on the picture decoding pool.
pub(super) async fn sanitized_avatar(
  state: &AppState,
  declared_type: String,
  bytes: Bytes,
  when_busy: WhenBusy,
) -> Result<Vec<u8>, ApiError> {
  let (max_bytes, side, timeout) = {
    let limits = &state.settings.get().limits;
    (
      limits.max_avatar_bytes,
      limits.avatar_side_px,
      Duration::from_secs(limits.image_decode_timeout_secs),
    )
  };
  if bytes.is_empty() {
    return Err(ApiError::InvalidRequest("the picture is empty".into()));
  }
  if bytes.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "avatars are limited to {max_bytes} bytes"
    )));
  }
  run_limited(&state.image_decodes, when_busy, timeout, move || {
    sanitize_avatar(&bytes, &declared_type, side)
  })
  .await?
  .map_err(|error| ApiError::InvalidRequest(error.to_string()))
}

pub async fn delete_my_avatar(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  let profile = state.database.get_profile(caller.user.id).await?;
  if !can_change_avatar(&state.policy.get().directory, &profile) {
    return Err(ApiError::AvatarFromIdentityProvider);
  }
  remove_avatar(state, caller.user.id).await
}

/// Removes `user`'s own avatar, if there is one.
pub(super) async fn remove_avatar(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  if let Some(key) = state.database.clear_avatar(user).await? {
    delete_or_queue(state, store, &key).await;
    publish_update(state, user).await?;
  }
  Ok(())
}

/// Makes an already checked PNG `user`'s own avatar.
pub(super) async fn store_avatar(state: &AppState, user: UserId, png: Vec<u8>) -> Result<(), ApiError> {
  let store = blob_store(state)?;
  let avatar = put_png(state, store, user, png).await?;
  match state.database.replace_avatar(user, &avatar).await {
    Ok(Some(replaced)) => delete_or_queue(state, store, &replaced).await,
    Ok(None) => {},
    Err(error) => {
      delete_or_queue(state, store, &avatar.key).await;
      return Err(error.into());
    },
  }
  publish_update(state, user).await
}

/// Stores a PNG under a fresh key (a reader never gets a half-replaced object).
pub(super) async fn put_png(
  state: &AppState,
  store: &BlobStore,
  user: UserId,
  png: Vec<u8>,
) -> Result<StoredAvatar, ApiError> {
  let object: teamcraft_proto::AttachmentId = state.next_id()?;
  let avatar = StoredAvatar {
    key: format!("avatars/{}/{}", user.snowflake(), object.snowflake()),
    content_type: AvatarContentType::Png.as_str().to_owned(),
    size_bytes: png.len() as u64,
  };
  store
    .put_object(&avatar.key, png, AvatarContentType::Png.as_str())
    .await
    .map_err(|error| {
      warn!(%error, "storing a picture failed");
      ApiError::storage_failed("avatar storage", error)
    })?;
  Ok(avatar)
}

/// A download URL for somebody's avatar.
pub async fn get_avatar(
  state: &AppState,
  caller: &AuthenticatedUser,
  user: UserId,
) -> Result<AvatarResponse, ApiError> {
  let store = blob_store(state)?;
  // While the storage is down, say "later" here instead of handing out a URL that fails.
  store.check_available()?;
  let visible = visible_user(state, caller, user).await?;
  let profile = state.database.get_profile(visible.id).await?;
  let avatar =
    shown_avatar(&state.policy.get().directory, &profile).ok_or_else(ApiError::avatar_not_found)?;
  Ok(AvatarResponse {
    download_url: store.presign_download(&avatar.key).to_string(),
    content_type: AvatarContentType::from(avatar.content_type.clone()),
    size_bytes: avatar.size_bytes,
    expires_at: unix_millis(OffsetDateTime::now_utc() + store.expiry()),
  })
}

async fn profile_of(state: &AppState, user: UserId) -> Result<ProfileResponse, ApiError> {
  let profile = state.database.get_profile(user).await?;
  let policy = state.policy.get();
  let directory = &policy.directory;
  let maybe_avatar_version = shown_avatar(directory, &profile).map(|avatar| avatar_version(&avatar.key));
  let can_change_avatar = can_change_avatar(directory, &profile);
  Ok(ProfileResponse {
    user_id: user,
    maybe_title: profile.maybe_title,
    maybe_department: profile.maybe_department,
    maybe_phone: profile.maybe_phone,
    maybe_time_zone: profile.maybe_time_zone,
    maybe_working_hours: profile.maybe_working_hours.map(wire_hours),
    maybe_display_name_override: state.database.get_display_name_override(user).await?,
    can_override_display_name: directory.allow_display_name_override,
    has_avatar: maybe_avatar_version.is_some(),
    maybe_avatar_version,
    can_change_avatar,
  })
}

/// The picture everybody sees of a person (issue #2071): their own, unless the workspace takes
/// pictures from the identity provider; then the provider's, unless people may override it and
/// set their own. Without the preferred picture, the other one shows.
pub(super) fn shown_avatar<'a>(
  policy: &DirectoryPolicy,
  profile: &'a StoredProfile,
) -> Option<&'a StoredAvatar> {
  let own = profile.maybe_avatar.as_ref();
  let provider = profile.maybe_provider_avatar.as_ref();
  match policy.avatar_source {
    AvatarSource::IdentityProvider if policy.allow_avatar_override => own.or(provider),
    AvatarSource::IdentityProvider => provider.or(own),
    // An unknown source from a newer admin client: the safe reading is people's own pictures.
    AvatarSource::User | AvatarSource::Unknown(_) => own,
  }
}

/// Whether the person may set or remove their own picture: always, except while the provider's
/// picture shows and the policy doesn't let people override it.
pub(super) fn can_change_avatar(policy: &DirectoryPolicy, profile: &StoredProfile) -> bool {
  match policy.avatar_source {
    AvatarSource::IdentityProvider => policy.allow_avatar_override || profile.maybe_provider_avatar.is_none(),
    AvatarSource::User | AvatarSource::Unknown(_) => true,
  }
}

/// The version clients cache an avatar by: the last part of its object key, which is new with
/// every upload (`avatars/{user}/{object}`) and says nothing about storage.
fn avatar_version(key: &str) -> String {
  key.rsplit('/').next().unwrap_or(key).to_owned()
}

pub(super) async fn publish_update(state: &AppState, user: UserId) -> Result<(), ApiError> {
  let mut audience: HashSet<UserId> = state.database.users_sharing_conversation(user).await?;
  audience.insert(user);
  state
    .hub
    .publish(audience, &GatewayEvent::ProfileUpdate { user_id: user });
  Ok(())
}

pub(super) fn blob_store(state: &AppState) -> Result<&std::sync::Arc<BlobStore>, ApiError> {
  state
    .maybe_blob_store
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("storage is not configured".into()))
}

/// Trims; empty means "not set"; control characters are refused.
fn clean_text(maybe_text: Option<String>, max_chars: usize, field: &str) -> Result<Option<String>, ApiError> {
  let Some(text) = maybe_text else {
    return Ok(None);
  };
  let text = text.trim();
  if text.is_empty() {
    return Ok(None);
  }
  if text.chars().count() > max_chars || text.chars().any(char::is_control) {
    return Err(ApiError::InvalidRequest(format!("invalid {field}")));
  }
  Ok(Some(text.to_owned()))
}

/// Trims; empty means "use the name from the identity provider"; control characters are refused.
fn clean_display_name(maybe_name: Option<String>, max_chars: usize) -> Result<Option<String>, ApiError> {
  clean_text(maybe_name, max_chars, "display name")
}

fn clean_phone(maybe_phone: Option<String>) -> Result<Option<String>, ApiError> {
  let Some(phone) = maybe_phone else {
    return Ok(None);
  };
  let phone = phone.trim();
  if phone.is_empty() {
    return Ok(None);
  }
  let is_plausible = phone.chars().count() <= MAX_PHONE_CHARS
    && phone.chars().any(|c| c.is_ascii_digit())
    && phone
      .chars()
      .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | '.' | '/' | ' '));
  if is_plausible {
    Ok(Some(phone.to_owned()))
  } else {
    Err(ApiError::InvalidRequest("invalid phone number".into()))
  }
}

/// Accepts only names from the IANA time zone database (compiled in), e.g. `Europe/Berlin`.
fn clean_time_zone(maybe_zone: Option<String>) -> Result<Option<String>, ApiError> {
  let Some(zone) = maybe_zone else {
    return Ok(None);
  };
  let zone = zone.trim();
  if zone.is_empty() {
    return Ok(None);
  }
  if zone.len() <= MAX_TIME_ZONE_CHARS && time_zone::is_known(zone) {
    Ok(Some(zone.to_owned()))
  } else {
    Err(ApiError::InvalidRequest("invalid time_zone".into()))
  }
}

fn stored_hours(hours: WorkingHours) -> Result<StoredWorkingHours, ApiError> {
  let mut mask = 0u8;
  for day in &hours.weekdays {
    mask |= 1 << weekday_bit(day)?;
  }
  if mask == 0 {
    return Err(ApiError::InvalidRequest(
      "working_hours needs at least one weekday".into(),
    ));
  }
  if hours.start_minute >= hours.end_minute || hours.end_minute > MINUTES_PER_DAY {
    return Err(ApiError::InvalidRequest(
      "working_hours must start before it ends, within one day".into(),
    ));
  }
  Ok(StoredWorkingHours {
    weekday_mask: mask,
    start_minute: hours.start_minute,
    end_minute: hours.end_minute,
  })
}

fn wire_hours(hours: StoredWorkingHours) -> WorkingHours {
  const DAYS: [Weekday; 7] = [
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
    Weekday::Saturday,
    Weekday::Sunday,
  ];
  WorkingHours {
    weekdays: DAYS
      .into_iter()
      .enumerate()
      .filter(|(bit, _)| hours.weekday_mask & (1 << bit) != 0)
      .map(|(_, day)| day)
      .collect(),
    start_minute: hours.start_minute,
    end_minute: hours.end_minute,
  }
}

fn weekday_bit(day: &Weekday) -> Result<u8, ApiError> {
  Ok(match day {
    Weekday::Monday => 0,
    Weekday::Tuesday => 1,
    Weekday::Wednesday => 2,
    Weekday::Thursday => 3,
    Weekday::Friday => 4,
    Weekday::Saturday => 5,
    Weekday::Sunday => 6,
    Weekday::Unknown(_) => return Err(ApiError::InvalidRequest("unknown weekday".into())),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_avatars_version_is_its_object_name_only() {
    assert_eq!(avatar_version("avatars/7/123456789"), "123456789");
    assert_ne!(avatar_version("avatars/7/1"), avatar_version("avatars/7/2"));
    assert_eq!(avatar_version("flat"), "flat");
  }

  mod text {
    use super::*;

    #[test]
    fn text_is_trimmed_and_bounded() {
      assert_eq!(
        clean_text(Some("  Lead  ".into()), 10, "title")
          .unwrap()
          .as_deref(),
        Some("Lead")
      );
      assert_eq!(clean_text(Some("   ".into()), 10, "title").unwrap(), None);
      assert_eq!(clean_text(None, 10, "title").unwrap(), None);
      assert!(clean_text(Some("x".repeat(11)), 10, "title").is_err());
      assert!(clean_text(Some("a\nb".into()), 10, "title").is_err());
    }

    #[test]
    fn phone_numbers_allow_the_usual_punctuation_only() {
      for good in ["+49 30 123456", "(030) 12-34", "0049/30.123"] {
        assert_eq!(clean_phone(Some(good.into())).unwrap().as_deref(), Some(good));
      }
      for bad in ["call me", "+++", "1".repeat(41).as_str(), "12;34", "<script>"] {
        assert!(clean_phone(Some(bad.into())).is_err(), "{bad}");
      }
    }

    #[test]
    fn time_zones_have_the_shape_of_iana_names() {
      for good in [
        "Europe/Berlin",
        "UTC",
        "America/Argentina/Buenos_Aires",
        "Etc/GMT+5",
        "Asia/Kolkata",
        // Links of the IANA database stay valid names.
        "US/Eastern",
        "Europe/Kiev",
        "GMT",
      ] {
        assert!(clean_time_zone(Some(good.into())).unwrap().is_some(), "{good}");
        assert_eq!(
          clean_time_zone(Some(format!(" {good} "))).unwrap().as_deref(),
          Some(good)
        );
      }
      for bad in [
        "Europe//Berlin",
        "/Berlin",
        "Europe/",
        "../etc",
        "Europe/Berlin; drop",
        "9/9",
        "Mars/Base",
        "Europe/Berlinn",
        "Europe/Berlin/Mitte",
        // Names are stored as given: only the database's own spelling is one.
        "europe/berlin",
        "EUROPE/BERLIN",
        "utc",
      ] {
        assert!(clean_time_zone(Some(bad.into())).is_err(), "{bad}");
      }
    }
  }

  mod hours {
    use super::*;

    #[test]
    fn hours_round_trip_through_the_stored_mask() {
      let hours = WorkingHours {
        weekdays: vec![Weekday::Friday, Weekday::Monday],
        start_minute: 540,
        end_minute: 1020,
      };
      let stored = stored_hours(hours).unwrap();
      assert_eq!(stored.weekday_mask, 0b0001_0001);
      assert_eq!(
        wire_hours(stored).weekdays,
        vec![Weekday::Monday, Weekday::Friday]
      );
    }

    #[test]
    fn invalid_hours_are_refused() {
      let hours = |weekdays: Vec<Weekday>, start_minute, end_minute| WorkingHours {
        weekdays,
        start_minute,
        end_minute,
      };
      assert!(stored_hours(hours(vec![], 0, 60)).is_err());
      assert!(stored_hours(hours(vec![Weekday::Monday], 600, 600)).is_err());
      assert!(stored_hours(hours(vec![Weekday::Monday], 700, 600)).is_err());
      assert!(stored_hours(hours(vec![Weekday::Monday], 0, 1441)).is_err());
      assert!(stored_hours(hours(vec![Weekday::Unknown("x".into())], 0, 60)).is_err());
      assert!(stored_hours(hours(vec![Weekday::Monday], 0, 1440)).is_ok());
    }
  }
}
