//! Spaces: settings, members, categories, invites and local guests.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::list_spaces_handler,
      handlers::create_space_handler
    ))
    .routes(routes!(
      handlers::get_space_handler,
      handlers::update_space_handler
    ))
    .routes(routes!(
      handlers::list_categories_handler,
      handlers::create_category_handler
    ))
    .routes(routes!(
      handlers::update_category_handler,
      handlers::delete_category_handler
    ))
    .routes(routes!(
      handlers::get_space_image_handler,
      handlers::put_space_image_handler,
      handlers::delete_space_image_handler
    ))
    .routes(routes!(
      handlers::list_invites_handler,
      handlers::create_invite_handler
    ))
    .routes(routes!(handlers::delete_invite_handler))
    .routes(routes!(handlers::join_space_handler))
    .routes(routes!(handlers::create_local_guest_handler))
    .routes(routes!(
      handlers::list_space_members_handler,
      handlers::add_space_member_handler
    ))
    .routes(routes!(
      handlers::update_space_member_handler,
      handlers::remove_space_member_handler
    ))
}
