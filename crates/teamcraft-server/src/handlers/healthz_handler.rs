use axum::Json;
use teamcraft_proto::{HealthResponse, HealthStatus};

/// Liveness: the process is up and serving. Does not touch dependencies.
pub async fn healthz_handler() -> Json<HealthResponse> {
  Json(HealthResponse::new(HealthStatus::Ok))
}
