use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SetPresenceRequest, SetPresenceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::presence;
use crate::state::AppState;

/// Sets do-not-disturb or appear-offline (or back to automatic).
#[utoipa::path(put, path = "/api/v1/me/presence", tag = "presence", summary = "Sets do-not-disturb or appear-offline (or back to automatic).", request_body = SetPresenceRequest, responses((status = 200, body = SetPresenceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_presence_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<SetPresenceRequest>,
) -> Result<Json<SetPresenceResponse>, ApiError> {
  let response = presence::set_presence(&state, &caller, request).await?;
  Ok(Json(response))
}
