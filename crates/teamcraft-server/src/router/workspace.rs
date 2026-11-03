//! The signed workspace roster and policy (docs/admin.md).

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::ListWorkspaceLogQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(
      handlers::get_policy_handler,
      handlers::append_policy_handler
    ))
    .routes(routes!(handlers::list_policy_log_handler))
    .routes(routes!(
      handlers::list_roster_handler,
      handlers::append_roster_handler
    ))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListWorkspaceLogQuery)))]
struct Schemas;
