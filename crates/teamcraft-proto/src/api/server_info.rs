//! What a native app needs to know about a server before anyone signs in (docs/operations.md
//! "Server address in the apps").

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::BrandResponse;
use crate::wire_enum::wire_enum;

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

wire_enum! {
  /// A way to sign in that the sign-in page offers.
  pub enum SignInMethod {
    /// The identity provider (OpenID Connect): "Sign in with …".
    Sso => "sso",
    /// E-mail and password of a local account, with a one-time code when the account has one.
    Local => "local",
  }
}

/// How the apps sign in to this server: an OpenID Connect public client with PKCE, and how the
/// sign-in page looks.
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
  /// Whether local accounts (e-mail and password) can sign in; the same as `methods` containing
  /// `local`.
  #[serde(default)]
  pub has_local_accounts: bool,
  /// Every local account must enter its one-time code (TOTP). Otherwise only accounts that set
  /// one up do; the form then shows the code field as optional. The server never says which
  /// accounts have one (a missing code is a plain refusal), so the field is always there.
  #[serde(default)]
  pub local_requires_code: bool,
  /// The methods the sign-in page offers, in this order. Never empty; clients skip methods they
  /// don't know and fall back to `sso` when nothing is left.
  #[serde(default = "default_methods")]
  pub methods: Vec<SignInMethod>,
  /// The provider part of "Sign in with …" (a brand setting, e.g. "Microsoft"). Absent: the
  /// client uses the brand's display name.
  #[serde(rename = "sso_label", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sso_label: Option<String>,
  /// Path of the provider icon on the server (`/api/v1/brand/assets/sso_icon`), if the brand has
  /// one.
  #[serde(rename = "sso_icon", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sso_icon: Option<String>,
  /// Help below the form ("Problems signing in? Call 1234."), language code -> text, `en` is the
  /// fallback.
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub help_text: BTreeMap<String, String>,
  /// A help page (https) linked below the form.
  #[serde(rename = "help_link", default, skip_serializing_if = "Option::is_none")]
  pub maybe_help_link: Option<String>,
}

fn default_methods() -> Vec<SignInMethod> {
  vec![SignInMethod::Sso]
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
        "local_requires_code": false,
        "methods": ["sso"],
      })
    );
    assert_eq!(json["brand"]["display_name"], "TeamCraft");
  }

  #[test]
  fn older_answers_without_methods_offer_sso_and_unknown_methods_survive() {
    let mut json = serde_json::to_value(sample()).unwrap();
    json["sign_in"].as_object_mut().unwrap().remove("methods");
    let back: ServerInfoResponse = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back.sign_in.methods, [SignInMethod::Sso]);
    json["sign_in"]["methods"] = serde_json::json!(["passkey", "local", "sso"]);
    let back: ServerInfoResponse = serde_json::from_value(json).unwrap();
    assert_eq!(
      back.sign_in.methods,
      [
        SignInMethod::Unknown("passkey".into()),
        SignInMethod::Local,
        SignInMethod::Sso
      ]
    );
  }

  #[test]
  fn redirect_uris_round_trip_and_unknown_fields_are_ignored() {
    let mut info = sample();
    info.sign_in.maybe_redirect_uris = Some(vec!["http://127.0.0.1/callback".into()]);
    info.sign_in.maybe_sso_label = Some("Example SSO".into());
    info.sign_in.maybe_sso_icon = Some("/api/v1/brand/assets/sso_icon".into());
    info.sign_in.help_text = [("en".to_owned(), "Call the help desk.".to_owned())].into();
    info.sign_in.maybe_help_link = Some("https://help.example.com".into());
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
        local_requires_code: false,
        methods: vec![SignInMethod::Sso],
        maybe_sso_label: None,
        maybe_sso_icon: None,
        help_text: BTreeMap::new(),
        maybe_help_link: None,
      },
    }
  }
}
