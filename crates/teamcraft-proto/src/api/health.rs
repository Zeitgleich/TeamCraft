use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Role, UserId};

wire_enum! {
  /// Result of a health probe.
  pub enum HealthStatus {
    Ok => "ok",
    Unavailable => "unavailable",
  }
}

/// `GET /healthz` and `GET /readyz`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
  pub status: HealthStatus,
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
    let json = serde_json::to_string(&HealthResponse {
      status: HealthStatus::Ok,
    })
    .unwrap();
    assert_eq!(json, r#"{"status":"ok"}"#);
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
