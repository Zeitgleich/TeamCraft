use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateLocalGuestRequest, ErrorBody, LocalGuestResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::local_accounts;
use crate::state::AppState;

/// Creates a local guest account and e-mails the activation link.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/local-guests", tag = "guests", summary = "Creates a local guest account and e-mails the activation link.", request_body = CreateLocalGuestRequest, params(("space_id" = SpaceId, Path)), responses((status = 201, body = LocalGuestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_local_guest_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<CreateLocalGuestRequest>,
) -> Result<(StatusCode, Json<LocalGuestResponse>), ApiError> {
  let response = local_accounts::create_local_guest(&state, &caller, space_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
