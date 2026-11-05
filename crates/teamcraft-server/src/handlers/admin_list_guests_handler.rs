use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminGuestsQuery, ListAdminGuestsResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::guests;
use crate::state::AppState;

/// All guest memberships, soonest expiry first, for the admin console.
#[utoipa::path(get, path = "/api/v1/admin/guests", tag = "admin", summary = "All guest memberships, soonest expiry first.", params(ListAdminGuestsQuery), responses((status = 200, body = ListAdminGuestsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_guests_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiQuery(query): ApiQuery<ListAdminGuestsQuery>,
) -> Result<Json<ListAdminGuestsResponse>, ApiError> {
  let response = guests::admin_list(&state, &caller, query).await?;
  Ok(Json(response))
}
