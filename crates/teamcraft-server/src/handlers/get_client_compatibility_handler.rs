use axum::Json;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::ClientCompatibilityResponse;

/// The oldest client version this server works with. Raised only by a release that breaks older
/// clients; within 1.x the API stays compatible (docs/release.md), so it stays at 1.0.0.
pub const MINIMUM_CLIENT_VERSION: &str = "1.0.0";

/// Tells clients which versions this server supports, so outdated ones ask their user to update
/// (docs/updates.md §4.4). Public: a client checks it before sign-in.
#[utoipa::path(get, path = "/api/v1/client-compatibility", tag = "health", summary = "Returns the server version and the oldest supported client version.", responses((status = 200, body = ClientCompatibilityResponse)))]
pub async fn get_client_compatibility_handler() -> Response {
  let body = ClientCompatibilityResponse {
    server_version: env!("CARGO_PKG_VERSION").to_owned(),
    minimum_client_version: MINIMUM_CLIENT_VERSION.to_owned(),
  };
  let mut response = Json(body).into_response();
  response.headers_mut().insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("public, max-age=3600"),
  );
  response
}
