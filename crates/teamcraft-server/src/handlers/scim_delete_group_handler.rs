use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

use crate::services::scim::{self, ScimCaller, ScimError};
use crate::state::AppState;

/// `DELETE /scim/v2/Groups/{id}`: removes the resource; deletes the group; nobody leaves a channel.
pub async fn scim_delete_group_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
) -> Result<StatusCode, ScimError> {
  scim::delete_group(&state, &caller, &id, &headers).await?;
  Ok(StatusCode::NO_CONTENT)
}
