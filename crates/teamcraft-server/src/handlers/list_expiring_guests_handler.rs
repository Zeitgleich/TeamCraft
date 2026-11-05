use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListExpiringGuestsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::guests;
use crate::state::AppState;

/// Guest memberships ending soon that concern the caller: their own, sponsored, or in a space they
/// administer.
#[utoipa::path(get, path = "/api/v1/guests/expiring", tag = "spaces", summary = "Guest memberships ending soon that concern the caller.", responses((status = 200, body = ListExpiringGuestsResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_expiring_guests_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListExpiringGuestsResponse>, ApiError> {
  let response = guests::list_expiring(&state, &caller).await?;
  Ok(Json(response))
}
