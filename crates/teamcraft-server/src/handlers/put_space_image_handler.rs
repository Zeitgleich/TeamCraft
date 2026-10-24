use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use teamcraft_proto::{ErrorBody, SpaceId, SpaceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::space_images;
use crate::state::AppState;

/// Sets the picture of a space (space admins). The body is a PNG, JPEG or WebP file whose
/// `Content-Type` matches it; the server re-encodes it as PNG. SVG is refused.
#[utoipa::path(put, path = "/api/v1/spaces/{space_id}/image", tag = "spaces", summary = "Sets the picture of a space.", params(("space_id" = SpaceId, Path)), request_body(content = Vec<u8>, content_type = "image/*"), responses((status = 200, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_space_image_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<Json<SpaceResponse>, ApiError> {
  let declared_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  let response = space_images::put_space_image(&state, &caller, space_id, declared_type, body).await?;
  Ok(Json(response))
}
