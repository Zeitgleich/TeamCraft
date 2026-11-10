use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};
use crate::state::AppState;

/// `PATCH /scim/v2/Users/{id}`: changes a user by PATCH operations (`If-Match` honoured).
pub async fn scim_patch_user_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<ScimResponse, ScimError> {
  scim::patch_user(&state, &caller, &id, &headers, &body).await
}
