use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

use crate::services::scim::{self, ScimCaller, ScimError};
use crate::state::AppState;

/// `DELETE /scim/v2/Users/{id}`: removes the resource; deactivates the account, or deletes it if it never signed in.
pub async fn scim_delete_user_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  Path(id): Path<String>,
  headers: HeaderMap,
) -> Result<StatusCode, ScimError> {
  scim::delete_user(&state, &caller, &id, &headers).await?;
  Ok(StatusCode::NO_CONTENT)
}
