//! Devices, their push registrations and device linking (docs/e2ee.md).

use axum::routing::{delete, get, post};
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{
  CreateDeviceLinkRequest, DeviceLinkClosedReason, DeviceLinkPlatform, DeviceLinkRequest,
  ListDeviceLinksResponse,
};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_devices_handler, handlers::register_device_handler))
    .routes(routes!(handlers::revoke_device_handler, handlers::update_device_handler))
    .routes(routes!(handlers::set_device_certificate_handler))
    .routes(routes!(handlers::register_push_handler, handlers::unregister_push_handler))
    .routes(routes!(handlers::send_test_push_handler))
    .routes(routes!(handlers::get_my_push_health_handler))
    .routes(routes!(handlers::get_push_gateway_handler))
    .routes(routes!(handlers::get_web_push_key_handler))
    .routes(routes!(handlers::change_web_push_subscription_handler))
    // Device link requests are annotated but not part of the OpenAPI document.
    .route(
      "/api/v1/device-links",
      get(handlers::list_device_links_handler).post(handlers::create_device_link_handler),
    )
    .route("/api/v1/device-links/{request_id}", delete(handlers::withdraw_device_link_handler))
    .route("/api/v1/device-links/{request_id}/reject", post(handlers::reject_device_link_handler))
    // A WebSocket; the device-linking protocol is in teamcraft-proto (docs/e2ee.md).
    .route("/api/v1/rendezvous", get(handlers::rendezvous_handler))
}

/// Schemas in the document that no documented route names: the device link requests.
#[derive(OpenApi)]
#[openapi(components(schemas(
  CreateDeviceLinkRequest,
  DeviceLinkClosedReason,
  DeviceLinkPlatform,
  DeviceLinkRequest,
  ListDeviceLinksResponse
)))]
struct Schemas;
