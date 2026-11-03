use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DependencyHealth, DependencyName, DependencyState, HealthResponse, HealthStatus};
use tracing::warn;

use crate::outage_gate::OutageGate;
use crate::state::AppState;

/// Readiness: the server is not shutting down and the database answers. The database's breaker
/// decides, so one slow ping doesn't take the instance out of the load balancer and a database that
/// is down isn't pinged by every probe (issue #743). The other dependencies are listed; one that is
/// down makes the answer `degraded`, still 200: another instance would meet the same outage.
pub async fn readyz_handler(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
  if state.drain.is_draining() {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      Json(HealthResponse::new(HealthStatus::Unavailable)),
    );
  }
  if let Err(error) = OutageGate::from_state(&state).probe().await {
    warn!(%error, "readiness check failed");
  }
  let dependencies = state.dependencies.health();
  let status = readiness(&dependencies);
  let code = if status == HealthStatus::Unavailable {
    StatusCode::SERVICE_UNAVAILABLE
  } else {
    StatusCode::OK
  };
  (code, Json(HealthResponse { status, dependencies }))
}

/// Ready while the database's breaker is closed; degraded while another dependency is not ok.
fn readiness(dependencies: &[DependencyHealth]) -> HealthStatus {
  let is_database_ok = dependencies
    .iter()
    .filter(|health| health.dependency == DependencyName::Database)
    .all(|health| health.state == DependencyState::Ok);
  if !is_database_ok {
    HealthStatus::Unavailable
  } else if dependencies
    .iter()
    .any(|health| health.state != DependencyState::Ok)
  {
    HealthStatus::Degraded
  } else {
    HealthStatus::Ok
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_database_decides_readiness_and_the_others_degrade_it() {
    let health = |dependency, state| DependencyHealth { dependency, state };
    assert_eq!(
      readiness(&[health(DependencyName::Database, DependencyState::Ok)]),
      HealthStatus::Ok
    );
    assert_eq!(
      readiness(&[
        health(DependencyName::Database, DependencyState::Ok),
        health(DependencyName::ObjectStorage, DependencyState::Down),
      ]),
      HealthStatus::Degraded
    );
    for state in [DependencyState::Down, DependencyState::Recovering] {
      assert_eq!(
        readiness(&[
          health(DependencyName::Database, state),
          health(DependencyName::ObjectStorage, DependencyState::Ok),
        ]),
        HealthStatus::Unavailable
      );
    }
  }
}
