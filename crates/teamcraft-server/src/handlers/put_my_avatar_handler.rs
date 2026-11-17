use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use teamcraft_proto::{ErrorBody, ProfileResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::profiles;
use crate::state::AppState;

/// Sets the caller's own picture. The body is a PNG, JPEG or WebP file whose `Content-Type`
/// matches it; the server keeps the middle square, re-encoded as a PNG of at most
/// `limits.avatar_side_px` pixels per side. Refused with `avatar_from_identity_provider` while the
/// workspace shows the identity provider's picture and allows no override.
#[utoipa::path(put, path = "/api/v1/me/avatar", tag = "profiles", summary = "Sets the caller's own picture.", request_body(content = Vec<u8>, content_type = "image/*"), responses((status = 200, body = ProfileResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_my_avatar_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  headers: HeaderMap,
  body: Bytes,
) -> Result<Json<ProfileResponse>, ApiError> {
  let declared_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  let profile = profiles::put_my_avatar(&state, &caller, declared_type, body).await?;
  Ok(Json(profile))
}
