use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, NotetakerCredentialsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::notetaker;
use crate::state::AppState;

/// Returns the notetaker's API key to the bot account the signed policy names (e2ee.md §8.2).
#[utoipa::path(get, path = "/api/v1/notetaker/credentials", tag = "calls", summary = "Returns the notetaker's OpenRouter key to its bot account.", responses((status = 200, body = NotetakerCredentialsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_notetaker_credentials_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<NotetakerCredentialsResponse>, ApiError> {
  let response = notetaker::credentials(&state, &caller).await?;
  Ok(Json(response))
}
