use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{HealthResponse, HealthStatus};
use tracing::warn;

use crate::state::AppState;

/// Readiness: the database answers.
pub async fn readyz_handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
  match state.database.ping().await {
    Ok(()) => (
      StatusCode::OK,
      Json(HealthResponse {
        status: HealthStatus::Ok,
      }),
    ),
    Err(error) => {
      warn!(%error, "readiness check failed");
      (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(HealthResponse {
          status: HealthStatus::Unavailable,
        }),
      )
    },
  }
}
