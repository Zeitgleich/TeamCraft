use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Base64Bytes, DeviceId, UnixMillis, UserId};

wire_enum! {
  /// Employee or external person (ARCHITECTURE.md §4.1).
  pub enum UserKind {
    Member => "member",
    Guest => "guest",
  }
}

/// What other users see of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserSummary {
  pub id: UserId,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
}

/// `GET /api/v1/users?query=` – the directory. Guests only find people they share a conversation with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SearchUsersResponse {
  pub users: Vec<UserSummary>,
}

/// `POST /api/v1/devices`. Keys are opaque to the server until the crypto milestone verifies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegisterDeviceRequest {
  pub name: String,
  pub public_key: Base64Bytes,
  pub cross_signature: Base64Bytes,
  #[serde(rename = "push_token", default, skip_serializing_if = "Option::is_none")]
  pub maybe_push_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceResponse {
  pub id: DeviceId,
  pub name: String,
  pub public_key: Base64Bytes,
  pub created_at: UnixMillis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListDevicesResponse {
  pub devices: Vec<DeviceResponse>,
}

/// Query of `GET /api/v1/users`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchUsersQuery {
  /// Matched against display name and username; empty lists the first people alphabetically.
  #[serde(default)]
  pub query: String,
}
