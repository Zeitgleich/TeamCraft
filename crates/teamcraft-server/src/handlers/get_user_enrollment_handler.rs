use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, UserEnrollmentResponse, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::identity;
use crate::state::AppState;

/// Returns the identity key and device list a user has published; neither before their first
/// device is enrolled (200, not 404).
#[utoipa::path(get, path = "/api/v1/users/{user_id}/enrollment", tag = "e2ee", summary = "Returns what a user has published for end-to-end encryption; empty before their first device is enrolled.", params(("user_id" = UserId, Path)), responses((status = 200, body = UserEnrollmentResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_enrollment_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<UserEnrollmentResponse>, ApiError> {
  let response = identity::get_user_enrollment(&state, &caller, user_id).await?;
  Ok(Json(response))
}
