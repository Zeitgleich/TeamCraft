//! The notetaker's API key (e2ee.md §8.2, admin.md §3.6): admins store it write-only, and only the
//! bot account the signed policy names fetches it. Everything else about the notetaker is signed
//! policy ([`crate::NotetakerPolicy`]) or encrypted control messages.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::SecretUpdate;
use crate::UnixMillis;

/// Longest API key the server stores.
pub const MAX_NOTETAKER_API_KEY_BYTES: usize = 512;

/// `GET /api/v1/admin/notetaker`: whether a key is stored; the key itself never comes back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminNotetakerResponse {
  pub is_key_stored: bool,
  /// The server has a settings key (`[secrets]`); without it no key can be stored.
  pub can_store_secrets: bool,
  #[serde(rename = "key_updated_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_key_updated_at: Option<UnixMillis>,
}

/// `PUT /api/v1/admin/notetaker/key`: stores, replaces or removes the OpenRouter API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutNotetakerKeyRequest {
  pub key: SecretUpdate,
}

/// `GET /api/v1/notetaker/credentials`: the key, for the policy's notetaker bot only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NotetakerCredentialsResponse {
  pub api_key: NotetakerApiKey,
}

/// The OpenRouter API key on its way to the notetaker. Not printed, cleared on drop.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct NotetakerApiKey(String);

impl NotetakerApiKey {
  pub fn new(key: String) -> Self {
    Self(key)
  }

  /// The key, for the requests to OpenRouter. Never log it.
  pub fn expose_secret(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for NotetakerApiKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("[redacted]")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::WriteOnlySecret;

  #[test]
  fn the_key_is_never_printed() {
    let response = NotetakerCredentialsResponse {
      api_key: NotetakerApiKey::new("sk-or-v1-secret".into()),
    };
    assert!(!format!("{response:?}").contains("secret"));
    assert_eq!(
      serde_json::to_string(&response).unwrap(),
      r#"{"api_key":"sk-or-v1-secret"}"#
    );
  }

  #[test]
  fn requests_and_responses_have_a_stable_form() {
    let put = PutNotetakerKeyRequest {
      key: SecretUpdate::Set(WriteOnlySecret::new("sk".into())),
    };
    assert_eq!(
      serde_json::to_string(&put).unwrap(),
      r#"{"key":{"action":"set","value":"sk"}}"#
    );
    let status = AdminNotetakerResponse {
      is_key_stored: false,
      can_store_secrets: true,
      maybe_key_updated_at: None,
    };
    assert_eq!(
      serde_json::to_string(&status).unwrap(),
      r#"{"is_key_stored":false,"can_store_secrets":true}"#
    );
  }
}
