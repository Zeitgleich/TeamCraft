use axum::body::Bytes;
use axum::extract::State;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};
use crate::state::AppState;

/// `POST /scim/v2/Groups`: makes a group (a retried create answers with the same resource).
pub async fn scim_create_group_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  body: Bytes,
) -> Result<ScimResponse, ScimError> {
  scim::create_group(&state, &caller, &body).await
}
