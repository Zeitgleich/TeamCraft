use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeleteSpaceRequest, ErrorBody, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiOptionalJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Deletes a space with all of its channels for everybody.
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}", tag = "spaces", summary = "Deletes a space with all of its channels for everybody.", description = "Archived or not; needs `delete_space`. Every channel goes as `DELETE /conversations/{id}` does (e2ee.md §4.6). `404` once it is gone. The body is optional: a workspace admin's signed deletion (e2ee.md §4.6.5, D7), naming each channel it lists with that group's current epoch (`409 epoch_conflict` otherwise; `GET /spaces/{id}/deletion` tells them), lets the members' devices drop the channels whose anchor places them in the space at once.", params(("space_id" = SpaceId, Path)), request_body(content = Option<DeleteSpaceRequest>), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiOptionalJson(request): ApiOptionalJson<DeleteSpaceRequest>,
) -> Result<StatusCode, ApiError> {
  spaces::delete_space(&state, &caller, space_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
