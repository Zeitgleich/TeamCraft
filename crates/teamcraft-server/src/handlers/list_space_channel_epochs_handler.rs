use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListSpaceChannelEpochsResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Lists the group epochs of a space's channels, for signing its deletion.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/channel-epochs", tag = "spaces", summary = "Lists the group epochs of a space's channels, for signing its deletion.", description = "Needs `delete_space`. Only the channels the caller sees.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = ListSpaceChannelEpochsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_space_channel_epochs_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<ListSpaceChannelEpochsResponse>, ApiError> {
  let response = spaces::channel_epochs(&state, &caller, space_id).await?;
  Ok(Json(response))
}
