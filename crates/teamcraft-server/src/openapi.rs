//! OpenAPI document of the REST API, generated from the handler annotations and proto types. The
//! routes bring their paths and schemas along (`crate::router`); this file only adds what the whole
//! document has.

use std::sync::LazyLock;

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// Built once from the routes: the document never changes while the server runs.
static DOCUMENT: LazyLock<utoipa::openapi::OpenApi> =
  LazyLock::new(|| crate::router::routes().into_openapi());

#[derive(OpenApi)]
#[openapi(
  info(title = "TeamCraft API", description = "REST API of the TeamCraft server. Realtime events: WebSocket /api/v1/gateway, frames in teamcraft-proto."),
  modifiers(&BearerAuth),
)]
struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
  fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
    let scheme = SecurityScheme::Http(
      HttpBuilder::new()
        .scheme(HttpAuthScheme::Bearer)
        .bearer_format("JWT")
        .build(),
    );
    openapi
      .components
      .get_or_insert_with(Default::default)
      .add_security_scheme("bearer", scheme);
  }
}

/// `GET /api/v1/openapi.json`
pub async fn openapi_json_handler() -> Json<utoipa::openapi::OpenApi> {
  Json(openapi())
}

pub fn openapi() -> utoipa::openapi::OpenApi {
  DOCUMENT.clone()
}

/// What the whole document has, before the routes add their paths and schemas.
pub(crate) fn base() -> utoipa::openapi::OpenApi {
  ApiDoc::openapi()
}
