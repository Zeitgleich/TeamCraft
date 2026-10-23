//! The server entry of the mobile apps (docs/operations.md "Server address in the apps"): the
//! checks of `teamcraft-client-api` `server_entry`, so Android and iOS decide exactly like the
//! desktop app. The apps fetch `GET /api/v1/server-info` themselves (without following redirects)
//! and hand the answer over.

use teamcraft_client_api::server_entry::{
  self, HttpPolicy, ServerAddressError, ServerAnswer, ServerCheckError, ServerOrigin, ServerProfile,
};

use crate::login_brand;
use crate::types::LoginBrand;

/// Why a server address or a server's answer can't be used. Flat: the app shows its own text per
/// variant; the message is for logs.
#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ServerEntryError {
  #[error("no server address")]
  Empty,
  #[error("not a web address")]
  InvalidAddress,
  /// Plain `http` to another device, or another scheme.
  #[error("only https addresses are supported")]
  HttpsOnly,
  #[error("the address contains a user name or password")]
  HasCredentials,
  /// Some other web server, or a TeamCraft server that does not serve this host name.
  #[error("not a TeamCraft server")]
  NotTeamCraft,
  /// An HTML page or a redirect instead of the server: the network wants a sign-in first.
  #[error("the network asks for a sign-in first")]
  CaptivePortal,
  #[error("the server is not available: {0}")]
  Unavailable(String),
  #[error("this app is too old for the server: {0}")]
  AppTooOld(String),
  #[error("the server is too old for this app: {0}")]
  ServerTooOld(String),
  #[error("the server's sign-in settings are invalid: {0}")]
  InvalidSignIn(String),
  #[error("the server is not set up for this app")]
  RedirectNotAllowed,
}

/// A normalised server address.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ServerAddress {
  /// `https://host[:port]`: what everything of the account is keyed by.
  pub origin: String,
  /// Host and port, for showing.
  pub host: String,
  /// A file and key name for the account's storage.
  pub storage_key: String,
  /// Where to fetch the server's answer from.
  pub server_info_url: String,
}

/// A server that passed the checks: what to store and what to show for confirmation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CheckedServer {
  pub server: StoredServer,
  /// The brand for the confirmation screen; `None` when the brand part is unusable.
  pub maybe_brand: Option<LoginBrand>,
}

/// The account's server and how to sign in to it. `profile_json` is what the app stores; it is
/// read back with [`stored_server`], which checks it again.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StoredServer {
  pub address: ServerAddress,
  pub issuer: String,
  pub client_id: String,
  pub scopes: Vec<String>,
  pub display_name: String,
  pub server_version: String,
  pub profile_json: String,
}

/// Normalises what the user typed or pasted (`team.example.com`, a URL, an invite or message
/// link) to the server's origin. `allow_insecure_http` only in development builds.
#[uniffi::export]
pub fn normalize_server_address(
  input: String,
  allow_insecure_http: bool,
) -> Result<ServerAddress, ServerEntryError> {
  let origin = ServerOrigin::parse(&input, policy(allow_insecure_http))?;
  Ok(address(&origin))
}

/// Checks the answer of `origin` to `GET /api/v1/server-info`. `status`, `content_type` and `body`
/// are the HTTP answer as received (redirects not followed); `redirect_uri` is the one this app
/// signs in with; `language` picks the brand texts.
#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn check_server_info(
  origin: String,
  status: u16,
  content_type: Option<String>,
  body: Vec<u8>,
  client_version: String,
  redirect_uri: String,
  allow_insecure_http: bool,
  language: String,
) -> Result<CheckedServer, ServerEntryError> {
  let policy = policy(allow_insecure_http);
  let origin = ServerOrigin::parse(&origin, policy)?;
  let answer = ServerAnswer {
    status,
    maybe_content_type: content_type.as_deref(),
    body: &body,
  };
  let checked =
    server_entry::check_server_info(&origin, answer, &client_version, Some(&redirect_uri), policy)?;
  let maybe_brand = serde_json::to_string(&checked.brand)
    .ok()
    .and_then(|json| login_brand(json, origin.as_str().to_owned(), language));
  Ok(CheckedServer {
    server: stored(&checked.profile)?,
    maybe_brand,
  })
}

/// Reads a stored `profile_json` back. `None` when it is missing pieces or was tampered with: the
/// app then shows the server entry again.
#[uniffi::export]
pub fn stored_server(profile_json: String) -> Option<StoredServer> {
  let profile: ServerProfile = serde_json::from_str(&profile_json).ok()?;
  stored(&profile).ok()
}

fn stored(profile: &ServerProfile) -> Result<StoredServer, ServerEntryError> {
  let profile_json =
    serde_json::to_string(profile).map_err(|error| ServerEntryError::InvalidSignIn(error.to_string()))?;
  Ok(StoredServer {
    address: address(&profile.origin),
    issuer: profile.issuer.clone(),
    client_id: profile.client_id.clone(),
    scopes: profile.scopes.clone(),
    display_name: profile.display_name.clone(),
    server_version: profile.server_version.clone(),
    profile_json,
  })
}

fn address(origin: &ServerOrigin) -> ServerAddress {
  ServerAddress {
    origin: origin.as_str().to_owned(),
    host: origin.host().to_owned(),
    storage_key: origin.storage_key(),
    server_info_url: origin.server_info_url(),
  }
}

fn policy(allow_insecure_http: bool) -> HttpPolicy {
  if allow_insecure_http {
    HttpPolicy::Allowed
  } else {
    HttpPolicy::LoopbackOnly
  }
}

impl From<ServerAddressError> for ServerEntryError {
  fn from(error: ServerAddressError) -> Self {
    match error {
      ServerAddressError::Empty => Self::Empty,
      ServerAddressError::Invalid => Self::InvalidAddress,
      ServerAddressError::UnsupportedScheme | ServerAddressError::InsecureHttp => Self::HttpsOnly,
      ServerAddressError::HasCredentials => Self::HasCredentials,
    }
  }
}

impl From<ServerCheckError> for ServerEntryError {
  fn from(error: ServerCheckError) -> Self {
    match error {
      ServerCheckError::NotTeamCraft => Self::NotTeamCraft,
      ServerCheckError::CaptivePortal => Self::CaptivePortal,
      ServerCheckError::Unavailable(status) => Self::Unavailable(format!("HTTP {status}")),
      ServerCheckError::ClientTooOld { minimum } => Self::AppTooOld(minimum),
      ServerCheckError::ServerTooOld { version } => Self::ServerTooOld(version),
      ServerCheckError::InvalidSignIn(reason) => Self::InvalidSignIn(reason),
      ServerCheckError::RedirectNotAllowed => Self::RedirectNotAllowed,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const REDIRECT: &str = "de.zeitgleich.teamcraft:/oauth/callback";

  #[test]
  fn a_checked_server_round_trips_through_its_stored_json() {
    let checked = check(200, &answer_json()).unwrap();
    assert_eq!(checked.server.address.origin, "https://team.example.com");
    assert_eq!(checked.server.address.storage_key, "team.example.com");
    assert_eq!(checked.server.scopes, ["openid", "profile", "email"]);
    assert_eq!(checked.maybe_brand.unwrap().display_name, "Example Chat");
    let back = stored_server(checked.server.profile_json.clone()).unwrap();
    assert_eq!(back, checked.server);
  }

  #[test]
  fn tampered_stored_json_is_refused() {
    let checked = check(200, &answer_json()).unwrap();
    let tampered = checked
      .server
      .profile_json
      .replace("https://team.example.com", "https://team.example.com/evil");
    assert!(stored_server(tampered).is_none());
    assert!(stored_server("{}".into()).is_none());
  }

  #[test]
  fn errors_keep_their_kind_across_the_boundary() {
    assert!(matches!(
      normalize_server_address("http://team.example.com".into(), false),
      Err(ServerEntryError::HttpsOnly)
    ));
    assert!(matches!(
      normalize_server_address(" ".into(), false),
      Err(ServerEntryError::Empty)
    ));
    assert!(matches!(
      check(200, "<html>Hotel Wi-Fi</html>"),
      Err(ServerEntryError::CaptivePortal)
    ));
    assert!(matches!(check(404, ""), Err(ServerEntryError::NotTeamCraft)));
    assert!(matches!(check(503, ""), Err(ServerEntryError::Unavailable(_))));
  }

  fn check(status: u16, body: &str) -> Result<CheckedServer, ServerEntryError> {
    check_server_info(
      "https://team.example.com".into(),
      status,
      Some("application/json".into()),
      body.as_bytes().to_vec(),
      "1.0.0".into(),
      REDIRECT.into(),
      false,
      "en".into(),
    )
  }

  fn answer_json() -> String {
    serde_json::json!({
      "product": "teamcraft",
      "server_version": "1.0.0",
      "minimum_client_version": "1.0.0",
      "brand": { "id": "example", "display_name": "Example Chat", "origin": "https://team.example.com" },
      "sign_in": {
        "issuer": "https://auth.example.com/realms/company",
        "client_id": "teamcraft",
        "scopes": ["openid", "profile", "email"],
        "redirect_uris": [REDIRECT],
      },
    })
    .to_string()
  }
}
