//! The HTTP routes. Each feature registers its routes in its own module below (`router/meetings.rs`,
//! …), so parallel work doesn't meet in one long list (#614). A handler with `#[utoipa::path]` is
//! registered with `routes!`, which adds it to the OpenAPI document together with the schemas it
//! names; a plain `.route(…)` stays out of the document.

mod admin;
mod admin_brands;
mod admin_settings;
mod admin_users;
mod announcements;
mod attachments;
mod auth;
mod brand;
mod calls;
mod conversations;
mod devices;
mod directory;
mod e2ee;
mod health;
mod import;
mod meetings;
mod messages;
mod notifications;
mod org_recovery;
mod presence;
mod profiles;
mod retention;
mod shares;
mod spaces;
mod sync;
mod threads;
mod user_groups;
mod workspace;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use tower_http::trace::TraceLayer;
use utoipa_axum::router::OpenApiRouter;

use crate::brand as brand_resolver;
use crate::openapi;
use crate::security;
use crate::state::AppState;
use crate::web;

pub fn router(state: AppState) -> Router {
  Router::from(routes())
    .fallback_service(web::router(&state.settings.get().web))
    .layer(DefaultBodyLimit::max(
      state.settings.get().security.max_request_body_bytes,
    ))
    .layer(middleware::from_fn_with_state(
      state.clone(),
      brand_resolver::resolve_brand,
    ))
    .layer(middleware::from_fn_with_state(state.clone(), security::guard))
    .layer(
      TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
        tracing::debug_span!(
          "request",
          method = %request.method(),
          path = %logged_path(request.uri().path()),
        )
      }),
    )
    .with_state(state)
}

/// The request path as logs show it: meeting link tokens (`/meeting/<token>`, the knock route)
/// are left out, and so is the query (#987 review).
fn logged_path(path: &str) -> String {
  for prefix in ["/meeting/", "/api/v1/meetings/link/"] {
    if let Some(rest) = path.strip_prefix(prefix) {
      let tail = rest.split_once('/').map_or("", |(_, tail)| tail);
      return if tail.is_empty() {
        format!("{prefix}[redacted]")
      } else {
        format!("{prefix}[redacted]/{tail}")
      };
    }
  }
  path.to_owned()
}

/// Every route, with the OpenAPI document of the documented ones ([`openapi::openapi`]).
pub(crate) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(openapi::base())
    .merge(admin::routes())
    .merge(admin_brands::routes())
    .merge(admin_settings::routes())
    .merge(admin_users::routes())
    .merge(announcements::routes())
    .merge(attachments::routes())
    .merge(auth::routes())
    .merge(brand::routes())
    .merge(calls::routes())
    .merge(conversations::routes())
    .merge(devices::routes())
    .merge(directory::routes())
    .merge(e2ee::routes())
    .merge(health::routes())
    .merge(import::routes())
    .merge(meetings::routes())
    .merge(messages::routes())
    .merge(notifications::routes())
    .merge(org_recovery::routes())
    .merge(presence::routes())
    .merge(profiles::routes())
    .merge(retention::routes())
    .merge(shares::routes())
    .merge(spaces::routes())
    .merge(sync::routes())
    .merge(threads::routes())
    .merge(user_groups::routes())
    .merge(workspace::routes())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn logged_paths_leave_meeting_link_tokens_out() {
    let token = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";
    assert_eq!(logged_path(&format!("/meeting/{token}")), "/meeting/[redacted]");
    assert_eq!(
      logged_path(&format!("/api/v1/meetings/link/{token}/knock")),
      "/api/v1/meetings/link/[redacted]/knock"
    );
    assert_eq!(
      logged_path("/api/v1/meetings/7/lobby"),
      "/api/v1/meetings/7/lobby"
    );
  }
}
