//! Organization key recovery: shares, requests and approvals (docs/e2ee.md).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::list_recovery_requests_handler,
      handlers::create_recovery_request_handler
    ))
    .routes(routes!(
      handlers::get_recovery_request_handler,
      handlers::cancel_recovery_request_handler
    ))
    .routes(routes!(handlers::approve_recovery_request_handler))
    .routes(routes!(handlers::deny_recovery_request_handler))
    .routes(routes!(handlers::get_recovery_material_handler))
    .routes(routes!(
      handlers::get_recovery_result_handler,
      handlers::put_recovery_result_handler
    ))
    .routes(routes!(handlers::put_recovery_share_handler))
    .routes(routes!(handlers::put_org_recovery_shares_handler))
    .routes(routes!(handlers::get_my_org_recovery_shares_handler))
}
