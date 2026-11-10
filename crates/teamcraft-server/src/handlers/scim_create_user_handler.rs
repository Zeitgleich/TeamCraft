use axum::body::Bytes;
use axum::extract::State;

use crate::services::scim::{self, ScimCaller, ScimError, ScimResponse};
use crate::state::AppState;

/// `POST /scim/v2/Users`: makes a user (a retried create answers with the same resource).
pub async fn scim_create_user_handler(
  State(state): State<AppState>,
  caller: ScimCaller,
  body: Bytes,
) -> Result<ScimResponse, ScimError> {
  scim::create_user(&state, &caller, &body).await
}
