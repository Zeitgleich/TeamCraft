//! The people directory and organizations.

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::SearchUsersQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_organizations_handler))
    .routes(routes!(handlers::search_users_handler))
    .routes(routes!(handlers::get_user_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(SearchUsersQuery)))]
struct Schemas;
