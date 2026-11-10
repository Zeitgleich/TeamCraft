use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;

use crate::services::scim::{self, ScimCaller, ScimError};
use crate::state::AppState;

/// `PATCH /scim/v2/Groups/{id}`: changes a group by PATCH operations (`If-Match` honoured).
pub async fn scim_patch_group_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<Response, ScimError> {
  scim::patch_group(&state, &caller, &id, &headers, &body).await
}
