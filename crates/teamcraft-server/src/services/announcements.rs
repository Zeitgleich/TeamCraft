//! The instance-wide announcement banner (PLAN.md §13).

use serde_json::json;
use teamcraft_db::{NewAnnouncement, StoredAnnouncement};
use teamcraft_proto::{
  AnnouncementId, AnnouncementLevel, AnnouncementResponse, GatewayEvent, GetAnnouncementResponse, Permission,
  SetAnnouncementRequest, Target,
};
use time::OffsetDateTime;

use super::access::require;
use super::audit::{self, Target as AuditTarget};
use super::convert::{offset_date_time, unix_millis};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_TEXT_CHARS: usize = 1000;

/// The banner in force, with the caller's dismissal.
pub async fn get_announcement(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<GetAnnouncementResponse, ApiError> {
  let Some(announcement) = state
    .database
    .current_announcement(OffsetDateTime::now_utc())
    .await?
  else {
    return Ok(GetAnnouncementResponse {
      maybe_announcement: None,
    });
  };
  let is_dismissed = state
    .database
    .is_announcement_dismissed(announcement.id, caller.user.id)
    .await?;
  Ok(GetAnnouncementResponse {
    maybe_announcement: Some(response(&announcement, is_dismissed)),
  })
}

/// Hides the banner for the caller, if it may be hidden.
pub async fn dismiss_announcement(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: AnnouncementId,
) -> Result<(), ApiError> {
  let current = state
    .database
    .current_announcement(OffsetDateTime::now_utc())
    .await?
    .filter(|announcement| announcement.id == id)
    .ok_or_else(ApiError::announcement_not_found)?;
  if !current.is_dismissible {
    return Err(ApiError::Forbidden(
      "this announcement cannot be dismissed".into(),
    ));
  }
  state.database.dismiss_announcement(id, caller.user.id).await?;
  Ok(())
}

/// Replaces the banner. Instance admins only.
pub async fn set_announcement(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SetAnnouncementRequest,
) -> Result<GetAnnouncementResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let text = request.text.trim();
  if text.is_empty()
    || text.chars().count() > MAX_TEXT_CHARS
    || text.chars().any(|c| c.is_control() && c != '\n')
  {
    return Err(ApiError::InvalidRequest("invalid announcement text".into()));
  }
  if matches!(request.level, AnnouncementLevel::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown announcement level".into()));
  }
  let maybe_expires_at = match request.maybe_expires_at {
    Some(millis) => {
      let at =
        offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
      if at <= OffsetDateTime::now_utc() {
        return Err(ApiError::InvalidRequest(
          "expires_at must be in the future".into(),
        ));
      }
      Some(at)
    },
    None => None,
  };
  let id: AnnouncementId = state.next_id()?;
  state
    .database
    .replace_announcement(&NewAnnouncement {
      id,
      text,
      level: request.level.as_str(),
      is_dismissible: request.is_dismissible,
      created_by: caller.user.id,
      maybe_expires_at,
    })
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "announcement_set",
    Some(AuditTarget::new("announcement", id)),
    &json!({"level": request.level.as_str()}),
  )
  .await?;
  state.hub.publish_all(&GatewayEvent::AnnouncementUpdate);
  get_announcement(state, caller).await
}

/// Ends the banner.
pub async fn clear_announcement(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  if state.database.end_announcement().await? {
    audit::record(
      state,
      Some(caller.user.id),
      "announcement_cleared",
      None,
      &json!({}),
    )
    .await?;
    state.hub.publish_all(&GatewayEvent::AnnouncementUpdate);
  }
  Ok(())
}

fn response(announcement: &StoredAnnouncement, is_dismissed: bool) -> AnnouncementResponse {
  AnnouncementResponse {
    id: announcement.id,
    text: announcement.text.clone(),
    level: AnnouncementLevel::from(announcement.level.clone()),
    is_dismissible: announcement.is_dismissible,
    created_at: unix_millis(announcement.created_at),
    maybe_expires_at: announcement.maybe_expires_at.map(unix_millis),
    is_dismissed,
  }
}
