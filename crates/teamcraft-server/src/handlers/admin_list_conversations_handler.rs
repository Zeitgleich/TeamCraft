use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminConversationsQuery, ListAdminConversationsResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::admin;
use crate::state::AppState;

/// Lists all channels, private ones included (metadata only).
#[utoipa::path(get, path = "/api/v1/admin/conversations", tag = "admin", summary = "Lists all channels, private ones included (metadata only).", params(ListAdminConversationsQuery), responses((status = 200, body = ListAdminConversationsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_conversations_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiQuery(query): ApiQuery<ListAdminConversationsQuery>,
) -> Result<Json<ListAdminConversationsResponse>, ApiError> {
  let response = admin::list_conversations(&state, &caller, query).await?;
  Ok(Json(response))
}
