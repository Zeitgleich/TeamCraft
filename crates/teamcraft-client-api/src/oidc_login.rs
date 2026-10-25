//! What every interactive OIDC sign-in sends besides the code flow (issue #448), and what it keeps
//! of the answer, for the web client and the desktop shell (the mobile apps do the same in AppAuth
//! and `ASWebAuthenticationSession`).
//!
//! - `prompt=login`: the identity provider shows its login page also when the browser still has a
//!   session there, so users see which account they sign in with and can switch. An instance
//!   setting (`SignInInfo::is_login_page_always_shown`, default on). Only for sign-ins the user
//!   starts: silent renewals (the web client after a reload) never send it.
//! - `login_hint`: the account last used on this device for this server, prefilled. Taken from the
//!   ID token's `preferred_username` or `email` claim and stored with the server's profile, so a
//!   hint never goes to another server.
//! - `ui_locales`: the app's language, so the provider's page matches it.
//!
//! All three are OIDC Core parameters (§3.1.2.1); providers that don't know them ignore them.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use oauth2::basic::{
  BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenIntrospectionResponse, BasicTokenType,
};
use oauth2::url::Url;
use oauth2::{
  AuthUrl, Client, ClientId, CsrfToken, EndpointNotSet, EndpointSet, ExtraTokenFields, PkceCodeChallenge,
  RedirectUrl, Scope, StandardRevocableToken, StandardTokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};

/// `prompt` value that asks for the login page (OIDC Core §3.1.2.1).
const PROMPT_LOGIN: &str = "login";
/// Longest login hint that is kept; user names and e-mail addresses are far shorter.
const MAX_LOGIN_HINT_CHARS: usize = 256;

/// The extra parameters of one interactive sign-in. The default sends none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoginRequest<'a> {
  /// Ask for the login page even with an existing provider session.
  pub is_login_page_shown: bool,
  pub maybe_login_hint: Option<&'a str>,
  /// The app's language as a BCP 47 tag (`de`, `en`).
  pub maybe_locale: Option<&'a str>,
}

/// The token response of an OIDC sign-in: the standard one plus the ID token, for the login hint.
pub type OidcTokenResponse = StandardTokenResponse<IdTokenFields, BasicTokenType>;

/// An `oauth2` client with the authorization and token endpoints set, answering with
/// [`OidcTokenResponse`].
pub type OidcClient = Client<
  BasicErrorResponse,
  OidcTokenResponse,
  BasicTokenIntrospectionResponse,
  StandardRevocableToken,
  BasicRevocationErrorResponse,
  EndpointSet,
  EndpointNotSet,
  EndpointNotSet,
  EndpointNotSet,
  EndpointSet,
>;

/// The client of a sign-in at `client_id`, with the provider's endpoints and the app's redirect.
pub fn oidc_client(
  client_id: ClientId,
  auth_url: AuthUrl,
  token_url: TokenUrl,
  redirect_url: RedirectUrl,
) -> OidcClient {
  Client::new(client_id)
    .set_auth_uri(auth_url)
    .set_token_uri(token_url)
    .set_redirect_uri(redirect_url)
}

/// The authorization URL of a sign-in: the code flow with PKCE and `scopes`, plus the login page,
/// hint and language of `request` (none for silent renewals: [`LoginRequest::default`]).
pub fn authorization_url(
  client: &OidcClient,
  scopes: &[String],
  challenge: PkceCodeChallenge,
  request: &LoginRequest<'_>,
) -> (Url, CsrfToken) {
  let mut authorize = client
    .authorize_url(CsrfToken::new_random)
    .add_scopes(scopes.iter().cloned().map(Scope::new))
    .set_pkce_challenge(challenge);
  for (name, value) in request.params() {
    authorize = authorize.add_extra_param(name, value);
  }
  authorize.url()
}

/// The `id_token` of a token response. No `Debug` with content: it names the user.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct IdTokenFields {
  #[serde(rename = "id_token", default, skip_serializing_if = "Option::is_none")]
  pub maybe_id_token: Option<String>,
}

impl ExtraTokenFields for IdTokenFields {}

impl LoginRequest<'_> {
  /// The query parameters to add to the authorization URL, in this order.
  pub fn params(&self) -> Vec<(&'static str, String)> {
    let mut params = Vec::with_capacity(3);
    if self.is_login_page_shown {
      params.push(("prompt", PROMPT_LOGIN.to_owned()));
    }
    if let Some(hint) = self.maybe_login_hint.and_then(clean_login_hint) {
      params.push(("login_hint", hint));
    }
    if let Some(locale) = self.maybe_locale.filter(|locale| is_language_tag(locale)) {
      params.push(("ui_locales", locale.to_owned()));
    }
    params
  }
}

/// The account an ID token names, as a login hint: `preferred_username`, else `email`. The token
/// is not verified: it came from the token endpoint over TLS and only prefills a form.
pub fn login_hint_from_id_token(id_token: &str) -> Option<String> {
  #[derive(Deserialize)]
  struct Claims {
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    email: Option<String>,
  }
  let payload = id_token.split('.').nth(1)?;
  let json = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
  let claims: Claims = serde_json::from_slice(&json).ok()?;
  claims
    .preferred_username
    .as_deref()
    .and_then(clean_login_hint)
    .or_else(|| claims.email.as_deref().and_then(clean_login_hint))
}

impl IdTokenFields {
  /// The login hint of the ID token, if it has one.
  pub fn login_hint(&self) -> Option<String> {
    self.maybe_id_token.as_deref().and_then(login_hint_from_id_token)
  }
}

impl std::fmt::Debug for IdTokenFields {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("IdTokenFields")
      .field("has_id_token", &self.maybe_id_token.is_some())
      .finish()
  }
}

/// A hint without surrounding space, control characters or absurd length; `None` if nothing is left.
fn clean_login_hint(hint: &str) -> Option<String> {
  let hint = hint.trim();
  let is_usable =
    !hint.is_empty() && hint.chars().count() <= MAX_LOGIN_HINT_CHARS && !hint.chars().any(char::is_control);
  is_usable.then(|| hint.to_owned())
}

/// `de`, `en-GB`, `zh-Hant-TW`: letters, digits and hyphens, a letter first.
fn is_language_tag(tag: &str) -> bool {
  tag.len() <= 35
    && tag
      .split('-')
      .all(|part| !part.is_empty() && part.len() <= 8 && part.chars().all(|c| c.is_ascii_alphanumeric()))
    && tag.starts_with(|c: char| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_interactive_sign_in_asks_for_the_login_page_with_hint_and_language() {
    let request = LoginRequest {
      is_login_page_shown: true,
      maybe_login_hint: Some(" alice@example.com "),
      maybe_locale: Some("de"),
    };
    assert_eq!(
      request.params(),
      vec![
        ("prompt", "login".to_owned()),
        ("login_hint", "alice@example.com".to_owned()),
        ("ui_locales", "de".to_owned()),
      ]
    );
  }

  #[test]
  fn the_instance_setting_and_missing_values_drop_their_parameters() {
    let request = LoginRequest {
      is_login_page_shown: false,
      maybe_login_hint: None,
      maybe_locale: None,
    };
    assert!(request.params().is_empty());
    let odd = LoginRequest {
      is_login_page_shown: false,
      maybe_login_hint: Some("line\nbreak"),
      maybe_locale: Some("de\"; x"),
    };
    assert!(odd.params().is_empty());
  }

  #[test]
  fn the_hint_comes_from_the_id_token() {
    let token = id_token(r#"{"sub":"1","preferred_username":"alice","email":"alice@example.com"}"#);
    assert_eq!(login_hint_from_id_token(&token).as_deref(), Some("alice"));
    let email_only = id_token(r#"{"sub":"1","email":"bob@example.com"}"#);
    assert_eq!(
      login_hint_from_id_token(&email_only).as_deref(),
      Some("bob@example.com")
    );
    assert_eq!(login_hint_from_id_token(&id_token(r#"{"sub":"1"}"#)), None);
    assert_eq!(login_hint_from_id_token("not-a-jwt"), None);
    let fields = IdTokenFields {
      maybe_id_token: Some(token),
    };
    assert_eq!(fields.login_hint().as_deref(), Some("alice"));
    assert!(!format!("{fields:?}").contains("alice"));
  }

  #[test]
  fn the_token_response_keeps_the_id_token() {
    let json = r#"{"access_token":"a","token_type":"Bearer","id_token":"h.e.s"}"#;
    let response: OidcTokenResponse = serde_json::from_str(json).unwrap();
    use oauth2::TokenResponse as _;
    assert_eq!(response.access_token().secret(), "a");
    assert_eq!(response.extra_fields().maybe_id_token.as_deref(), Some("h.e.s"));
  }

  fn id_token(claims: &str) -> String {
    format!("eyJhbGciOiJSUzI1NiJ9.{}.c2ln", URL_SAFE_NO_PAD.encode(claims))
  }
}
