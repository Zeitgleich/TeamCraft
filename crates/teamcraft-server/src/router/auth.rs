//! Who is signed in, local accounts, and what the identity provider calls: back-channel logout and
//! its events.

use axum::extract::DefaultBodyLimit;
use axum::routing::post;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::local_activate_handler))
    .routes(routes!(handlers::local_login_handler))
    .routes(routes!(handlers::local_logout_handler))
    .routes(routes!(handlers::change_local_password_handler))
    .routes(routes!(handlers::get_me_handler))
    // Called by the identity provider (OpenID Connect back-channel logout), not by clients.
    .route("/api/v1/auth/backchannel-logout", post(handlers::backchannel_logout_handler))
    // Called by the identity provider's event listener (docs/provisioning.md §4), not by clients.
    .route(
      "/api/v1/identity-provider/events",
      post(handlers::provider_event_handler)
        .layer(DefaultBodyLimit::max(teamcraft_proto::MAX_PROVIDER_EVENT_BYTES)),
    )
}
