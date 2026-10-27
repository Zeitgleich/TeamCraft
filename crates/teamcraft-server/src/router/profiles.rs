//! Profiles, avatars and call backgrounds.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::create_avatar_upload_handler,
      handlers::delete_my_avatar_handler
    ))
    .routes(routes!(handlers::complete_avatar_upload_handler))
    .routes(routes!(handlers::create_background_upload_handler))
    .routes(routes!(handlers::delete_background_handler))
    .routes(routes!(handlers::complete_background_upload_handler))
    .routes(routes!(handlers::download_background_handler))
    .routes(routes!(
      handlers::get_my_profile_handler,
      handlers::put_my_profile_handler
    ))
    .routes(routes!(handlers::download_org_background_handler))
    .routes(routes!(handlers::get_user_avatar_handler))
    .routes(routes!(handlers::get_user_profile_handler))
}
