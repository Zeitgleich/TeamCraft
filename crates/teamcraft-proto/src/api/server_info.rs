//! What a native app needs to know about a server before anyone signs in (docs/operations.md
//! "Server address in the apps").

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::BrandResponse;

/// Value of [`ServerInfoResponse::product`]; anything else is not a TeamCraft server.
pub const SERVER_INFO_PRODUCT: &str = "teamcraft";

/// `GET /api/v1/server-info` (no sign-in): everything a client needs to show the server to its
/// user and to sign in. Public and the same for every caller of a host. Clients validate it
/// strictly (`teamcraft-client-api` `server_entry`); it never contains a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerInfoResponse {
  /// Always [`SERVER_INFO_PRODUCT`]: tells a client that the address is a TeamCraft server and not
  /// some other web server or a captive portal answering with JSON.
  pub product: String,
  pub server_version: String,
  /// Older clients ask their user to update (as `GET /api/v1/client-compatibility`).
  pub minimum_client_version: String,
  /// The brand of the host the request arrived on (as `GET /api/v1/brand`).
  pub brand: BrandResponse,
  pub sign_in: SignInInfo,
}

/// How the apps sign in to this server: an OpenID Connect public client with PKCE.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignInInfo {
  /// Issuer of the identity provider; clients require `https` (plain `http` only on loopback).
  pub issuer: String,
  /// The public client the apps sign in with.
  pub client_id: String,
  /// Scopes the apps ask for; always contains `openid`.
  pub scopes: Vec<String>,
  /// Redirect URIs the identity provider allows for the apps, if the operator listed them. A
  /// client whose own redirect URI is missing tells its user that the server is not set up for it
  /// instead of failing at the identity provider. Absent: the client does not check.
  #[serde(rename = "redirect_uris", default, skip_serializing_if = "Option::is_none")]
  pub maybe_redirect_uris: Option<Vec<String>>,
  /// Whether guests can also sign in with a local account (e-mail and password).
  #[serde(default)]
  pub has_local_accounts: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_info_wire_format_is_stable() {
    let json = serde_json::to_value(sample()).unwrap();
    assert_eq!(json["product"], "teamcraft");
    assert_eq!(json["minimum_client_version"], "1.0.0");
    assert_eq!(
      json["sign_in"],
      serde_json::json!({
        "issuer": "https://auth.example.com/realms/company",
        "client_id": "teamcraft",
        "scopes": ["openid", "offline_access"],
        "has_local_accounts": false,
      })
    );
    assert_eq!(json["brand"]["display_name"], "TeamCraft");
  }

  #[test]
  fn redirect_uris_round_trip_and_unknown_fields_are_ignored() {
    let mut info = sample();
    info.sign_in.maybe_redirect_uris = Some(vec!["http://127.0.0.1/callback".into()]);
    let mut json = serde_json::to_value(&info).unwrap();
    json["sign_in"]["future_field"] = serde_json::json!(true);
    json["future_field"] = serde_json::json!(1);
    let back: ServerInfoResponse = serde_json::from_value(json).unwrap();
    assert_eq!(back, info);
  }

  fn sample() -> ServerInfoResponse {
    ServerInfoResponse {
      product: SERVER_INFO_PRODUCT.into(),
      server_version: "1.0.0".into(),
      minimum_client_version: "1.0.0".into(),
      brand: BrandResponse {
        id: "teamcraft".into(),
        display_name: "TeamCraft".into(),
        maybe_organization_name: None,
        origin: "https://team.example.com".into(),
        assets: Default::default(),
        colors: Default::default(),
        login_title: Default::default(),
        login_text: Default::default(),
        links: Default::default(),
      },
      sign_in: SignInInfo {
        issuer: "https://auth.example.com/realms/company".into(),
        client_id: "teamcraft".into(),
        scopes: vec!["openid".into(), "offline_access".into()],
        maybe_redirect_uris: None,
        has_local_accounts: false,
      },
    }
  }
}
