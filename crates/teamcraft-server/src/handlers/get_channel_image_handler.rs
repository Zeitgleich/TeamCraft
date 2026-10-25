use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::channel_images;
use crate::services::space_images::SPACE_IMAGE_CONTENT_TYPE;
use crate::state::AppState;

/// Returns the picture of a channel the caller may see (a PNG). `If-None-Match` with the image's
/// `ETag` answers 304.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/image", tag = "conversations", summary = "Returns the picture of a channel.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, content_type = "image/png", body = Vec<u8>), (status = 304), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_channel_image_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  request_headers: HeaderMap,
) -> Result<Response, ApiError> {
  let image = channel_images::get_channel_image(&state, &caller, conversation_id).await?;
  let etag = HeaderValue::from_str(&format!("\"{}\"", image.version))
    .map_err(|_| ApiError::Internal("the stored image version is not a header value".into()))?;
  let is_fresh = request_headers
    .get(header::IF_NONE_MATCH)
    .is_some_and(|candidate| *candidate == etag);
  let mut response = if is_fresh {
    StatusCode::NOT_MODIFIED.into_response()
  } else {
    Response::new(Body::from(image.bytes))
  };
  let headers = response.headers_mut();
  headers.insert(header::ETAG, etag);
  if !is_fresh {
    headers.insert(
      header::CONTENT_TYPE,
      HeaderValue::from_static(SPACE_IMAGE_CONTENT_TYPE),
    );
  }
  headers.insert(
    header::X_CONTENT_TYPE_OPTIONS,
    HeaderValue::from_static("nosniff"),
  );
  // Only for the people who may see the channel: no shared cache, revalidate by ETag.
  headers.insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("private, no-cache"),
  );
  Ok(response)
}
