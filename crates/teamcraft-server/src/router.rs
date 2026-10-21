use axum::Router;
use axum::routing::get;
use tower_http::trace::TraceLayer;

use crate::handlers::{get_me_handler, healthz_handler, readyz_handler};
use crate::state::AppState;

pub fn router(state: AppState) -> Router {
  let api_v1 = Router::new().route("/me", get(get_me_handler));

  Router::new()
    .route("/healthz", get(healthz_handler))
    .route("/readyz", get(readyz_handler))
    .nest("/api/v1", api_v1)
    .layer(TraceLayer::new_for_http())
    .with_state(state)
}
