//! Presence, custom status and privacy.

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{DeviceActivity, ListPresenceQuery};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::set_presence_handler))
    .routes(routes!(
      handlers::get_my_privacy_handler,
      handlers::put_my_privacy_handler
    ))
    .routes(routes!(
      handlers::set_custom_status_handler,
      handlers::clear_custom_status_handler
    ))
    .routes(routes!(handlers::list_presence_handler))
}

/// Schemas in the document that no documented route names: query parameters and a gateway event's payload.
#[derive(OpenApi)]
#[openapi(components(schemas(DeviceActivity, ListPresenceQuery)))]
struct Schemas;
