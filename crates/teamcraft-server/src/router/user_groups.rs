//! User groups.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::list_user_groups_handler,
      handlers::create_user_group_handler
    ))
    .routes(routes!(
      handlers::get_user_group_handler,
      handlers::update_user_group_handler,
      handlers::delete_user_group_handler
    ))
    .routes(routes!(
      handlers::add_user_group_member_handler,
      handlers::remove_user_group_member_handler
    ))
}
