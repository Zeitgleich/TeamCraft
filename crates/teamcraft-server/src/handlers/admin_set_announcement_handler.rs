use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, GetAnnouncementResponse, SetAnnouncementRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::announcements;
use crate::state::AppState;

/// Replaces the announcement banner.
#[utoipa::path(put, path = "/api/v1/admin/announcement", tag = "admin", summary = "Replaces the announcement banner.", request_body = SetAnnouncementRequest, responses((status = 200, body = GetAnnouncementResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_set_announcement_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<SetAnnouncementRequest>,
) -> Result<Json<GetAnnouncementResponse>, ApiError> {
  let response = announcements::set_announcement(&state, &caller, request).await?;
  Ok(Json(response))
}
