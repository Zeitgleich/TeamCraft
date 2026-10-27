//! Health, what clients need before signing in, and the API document itself.

use axum::routing::get;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{DependencyHealth, DependencyName, DependencyState, HealthResponse, HealthStatus};

use crate::handlers;
use crate::openapi::openapi_json_handler;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::get_client_compatibility_handler))
    .routes(routes!(handlers::get_server_info_handler))
    // Not in the OpenAPI document: the probes are outside the API, the document doesn't list itself.
    .route("/healthz", get(handlers::healthz_handler))
    .route("/readyz", get(handlers::readyz_handler))
    .route("/api/v1/openapi.json", get(openapi_json_handler))
}

/// Schemas in the document that no documented route names: the health probes.
#[derive(OpenApi)]
#[openapi(components(schemas(
  DependencyHealth,
  DependencyName,
  DependencyState,
  HealthResponse,
  HealthStatus
)))]
struct Schemas;
