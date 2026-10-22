use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CustomStatus, ErrorBody, SetCustomStatusRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::presence;
use crate::state::AppState;

/// Sets the caller's custom status (emoji, text, expiry).
#[utoipa::path(put, path = "/api/v1/me/status", tag = "presence", summary = "Sets the caller's custom status (emoji, text, expiry).", request_body = SetCustomStatusRequest, responses((status = 200, body = CustomStatus), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_custom_status_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<SetCustomStatusRequest>,
) -> Result<Json<CustomStatus>, ApiError> {
  let response = presence::set_custom_status(&state, &caller, request).await?;
  Ok(Json(response))
}
