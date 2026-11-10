use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;

use crate::services::scim::{self, ScimCaller, ScimError};
use crate::state::AppState;

/// `GET /scim/v2/Users/{id}`: one user of the client; 304 when `If-None-Match` names its version.
pub async fn scim_get_user_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
) -> Result<Response, ScimError> {
  scim::get_user(&state, &caller, &id, &headers).await
}
