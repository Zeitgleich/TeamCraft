//! SCIM clients in the admin console (docs/provisioning.md §7): each identity provider application
//! that provisions through `/scim/v2` gets a bearer token of its own, shown once, stored hashed,
//! rotated or revoked here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ScimClientId, UnixMillis, WebhookSecretText};

/// The most SCIM clients an instance keeps.
pub const MAX_SCIM_CLIENTS: usize = 10;
/// The longest name of a SCIM client, in characters.
pub const MAX_SCIM_CLIENT_NAME_CHARS: usize = 100;

wire_enum! {
  /// Which SCIM attribute of a user is the subject (`sub`) of their sign-in at the identity
  /// provider, so the account SCIM makes is the one the first sign-in finds.
  pub enum ScimIdentityAttribute {
    /// `externalId` (Okta, and providers that send their own user ID there).
    ExternalId => "external_id",
    /// `userName`.
    UserName => "user_name",
  }
}

/// One SCIM client as the console shows it; never the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScimClientResponse {
  pub id: ScimClientId,
  pub name: String,
  pub identity_attribute: ScimIdentityAttribute,
  pub created_at: UnixMillis,
  pub token_rotated_at: UnixMillis,
  /// When a request with its token last arrived (to the minute).
  #[serde(rename = "last_used_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_used_at: Option<UnixMillis>,
  /// Accounts and groups it provisioned that still exist.
  pub user_count: u32,
  pub group_count: u32,
}

/// `GET /api/v1/admin/scim/clients`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScimClientsResponse {
  pub clients: Vec<ScimClientResponse>,
  /// The base URL to give the identity provider, if the server knows its public address
  /// (`[mail] public_base_url`); else the console shows the path.
  #[serde(rename = "base_url", default, skip_serializing_if = "Option::is_none")]
  pub maybe_base_url: Option<String>,
}

/// `POST /api/v1/admin/scim/clients`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateScimClientRequest {
  pub name: String,
  pub identity_attribute: ScimIdentityAttribute,
}

/// A new or rotated token, shown this once. Never printed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScimClientTokenResponse {
  pub token: WebhookSecretText,
  pub client: ScimClientResponse,
}
