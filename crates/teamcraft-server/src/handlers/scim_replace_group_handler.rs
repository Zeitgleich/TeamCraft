use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};
use crate::state::AppState;

/// `PUT /scim/v2/Groups/{id}`: replaces a group's attributes (`If-Match` honoured).
pub async fn scim_replace_group_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<ScimResponse, ScimError> {
  scim::replace_group(&state, &caller, &id, &headers, &body).await
}
