use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Role, UserId};

wire_enum! {
  /// Result of a health probe.
  pub enum HealthStatus {
    Ok => "ok",
    /// Ready, but a dependency the server can do without for a while (object storage, identity
    /// provider, push gateway, SFU) is failing: the features that need it answer 503 with
    /// `Retry-After` until it is back. `/readyz` still answers 200, so the instance stays in the
    /// load balancer (another instance would meet the same outage).
    Degraded => "degraded",
    Unavailable => "unavailable",
  }
}

wire_enum! {
  /// A backend the server depends on (issue #743).
  pub enum DependencyName {
    Database => "database",
    ObjectStorage => "object_storage",
    IdentityProvider => "identity_provider",
    PushGateway => "push_gateway",
    Sfu => "sfu",
  }
}

wire_enum! {
  /// How a dependency is doing, by its circuit breaker.
  pub enum DependencyState {
    /// Answering (or not used yet).
    Ok => "ok",
    /// Failed several times in a row: calls fail fast until the next probe.
    Down => "down",
    /// Down until moments ago; the next call probes it.
    Recovering => "recovering",
  }
}

/// `GET /healthz` and `GET /readyz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
  pub status: HealthStatus,
  /// `/readyz` only: each configured dependency's state (names and states, no addresses or errors).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub dependencies: Vec<DependencyHealth>,
}

/// One dependency in [`HealthResponse::dependencies`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DependencyHealth {
  pub dependency: DependencyName,
  pub state: DependencyState,
}

impl HealthResponse {
  /// A status without dependency details.
  pub fn new(status: HealthStatus) -> Self {
    Self {
      status,
      dependencies: Vec::new(),
    }
  }
}

/// `GET /api/v1/client-compatibility` (no sign-in): which clients this server still works with
/// (docs/updates.md §4.4). Versions are semantic versions (`1.2.3`, `1.2.3-rc.1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClientCompatibilityResponse {
  pub server_version: String,
  /// Older clients ask their user to update.
  pub minimum_client_version: String,
}

/// `GET /api/v1/me`: the authenticated user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GetMeResponse {
  pub id: UserId,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub display_name: String,
  /// Highest instance role granted by the identity provider.
  pub role: Role,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn health_wire_format_is_stable() {
    let json = serde_json::to_string(&HealthResponse::new(HealthStatus::Ok)).unwrap();
    assert_eq!(json, r#"{"status":"ok"}"#);
  }

  #[test]
  fn readiness_lists_dependencies_and_tolerates_new_ones() {
    let ready = HealthResponse {
      status: HealthStatus::Degraded,
      dependencies: vec![DependencyHealth {
        dependency: DependencyName::ObjectStorage,
        state: DependencyState::Down,
      }],
    };
    assert_eq!(
      serde_json::to_string(&ready).unwrap(),
      r#"{"status":"degraded","dependencies":[{"dependency":"object_storage","state":"down"}]}"#
    );
    let newer: HealthResponse =
      serde_json::from_str(r#"{"status":"ok","dependencies":[{"dependency":"search","state":"draining"}]}"#)
        .unwrap();
    assert_eq!(
      newer.dependencies[0].dependency,
      DependencyName::Unknown("search".into())
    );
    assert_eq!(
      newer.dependencies[0].state,
      DependencyState::Unknown("draining".into())
    );
  }

  #[test]
  fn client_compatibility_wire_format_is_stable() {
    let json = serde_json::to_string(&ClientCompatibilityResponse {
      server_version: "1.0.1".into(),
      minimum_client_version: "1.0.0".into(),
    })
    .unwrap();
    assert_eq!(
      json,
      r#"{"server_version":"1.0.1","minimum_client_version":"1.0.0"}"#
    );
  }

  #[test]
  fn me_response_wire_format_is_stable() {
    let me = GetMeResponse {
      id: UserId::from_snowflake(Snowflake::new(42).unwrap()),
      maybe_username: None,
      display_name: "Alice".into(),
      role: Role::Member,
    };
    assert_eq!(
      serde_json::to_string(&me).unwrap(),
      r#"{"id":"42","display_name":"Alice","role":"member"}"#
    );
    let parsed: GetMeResponse =
      serde_json::from_str(r#"{"id":"42","username":"alice","display_name":"Alice","role":"auditor"}"#)
        .unwrap();
    assert_eq!(parsed.maybe_username.as_deref(), Some("alice"));
    assert_eq!(parsed.role, Role::Unknown("auditor".into()));
  }
}
