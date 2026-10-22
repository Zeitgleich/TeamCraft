use std::collections::HashMap;
use std::str::FromStr;

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use teamcraft_proto::Role;

use super::error::AuthError;
use super::jwks::JwksCache;
use super::roles::RoleMapping;

/// Validates OIDC access tokens: signature, issuer, expiry, client binding and role mapping.
pub struct TokenValidator {
  keys: JwksCache,
  roles: RoleMapping,
  options: ValidatorOptions,
}

#[derive(Debug, Clone)]
pub struct ValidatorOptions {
  pub issuer: String,
  pub client_id: String,
  pub algorithms: Vec<Algorithm>,
  pub leeway_secs: u64,
  /// Clients a back-channel logout token may be addressed to (the app client and the server client).
  pub logout_audiences: Vec<String>,
}

/// What a valid token tells us about the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenIdentity {
  pub issuer: String,
  pub subject: String,
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub maybe_email: Option<String>,
  /// The provider verified the email address (`email_verified` claim).
  pub is_email_verified: bool,
  pub role: Role,
  /// When the provider issued the token (Unix seconds); tokens older than a logout are refused.
  pub issued_at: i64,
  /// The provider's session ID (`sid` claim), if any.
  pub maybe_session_id: Option<String>,
}

/// Who a back-channel logout token ends sessions of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoutIdentity {
  pub maybe_subject: Option<String>,
  pub maybe_session_id: Option<String>,
}

#[derive(Deserialize)]
struct AccessClaims {
  sub: String,
  #[serde(default)]
  azp: Option<String>,
  #[serde(default)]
  aud: Audience,
  #[serde(default)]
  preferred_username: Option<String>,
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  email: Option<String>,
  #[serde(default)]
  email_verified: bool,
  #[serde(default)]
  iat: i64,
  #[serde(default)]
  sid: Option<String>,
  #[serde(default)]
  resource_access: HashMap<String, ClientAccess>,
}

const BACKCHANNEL_LOGOUT_EVENT: &str = "http://schemas.openid.net/event/backchannel-logout";

#[derive(Deserialize)]
struct LogoutClaims {
  #[serde(default)]
  sub: Option<String>,
  #[serde(default)]
  sid: Option<String>,
  #[serde(default)]
  iat: i64,
  #[serde(default)]
  jti: Option<String>,
  #[serde(default)]
  nonce: Option<String>,
  #[serde(default)]
  events: HashMap<String, serde_json::Value>,
}

#[derive(Deserialize, Default)]
#[serde(untagged)]
enum Audience {
  #[default]
  None,
  One(String),
  Many(Vec<String>),
}

#[derive(Deserialize)]
struct ClientAccess {
  #[serde(default)]
  roles: Vec<String>,
}

impl TokenValidator {
  pub fn new(keys: JwksCache, roles: RoleMapping, options: ValidatorOptions) -> Self {
    Self { keys, roles, options }
  }

  /// The issuer tokens must come from.
  pub fn issuer(&self) -> &str {
    &self.options.issuer
  }

  /// Parses configured algorithm names such as `RS256`.
  pub fn parse_algorithms(names: &[String]) -> Result<Vec<Algorithm>, String> {
    if names.is_empty() {
      return Err("at least one algorithm is required".into());
    }
    names
      .iter()
      .map(|name| Algorithm::from_str(name).map_err(|_| format!("unsupported algorithm {name:?}")))
      .collect()
  }

  pub async fn validate(&self, token: &str) -> Result<TokenIdentity, AuthError> {
    let header = decode_header(token).map_err(|e| invalid(&e))?;
    let key_id = header
      .kid
      .ok_or_else(|| AuthError::InvalidToken("token has no key ID".into()))?;
    let key = self.keys.key(&key_id).await?;
    self.verify(token, &key)
  }

  /// Validates an OIDC back-channel logout token (OpenID Connect Back-Channel Logout 1.0 §2.4).
  pub async fn validate_logout_token(&self, token: &str) -> Result<LogoutIdentity, AuthError> {
    let header = decode_header(token).map_err(|e| invalid(&e))?;
    let key_id = header
      .kid
      .ok_or_else(|| AuthError::InvalidToken("token has no key ID".into()))?;
    let key = self.keys.key(&key_id).await?;
    self.verify_logout(token, &key)
  }

  fn verify_logout(&self, token: &str, key: &DecodingKey) -> Result<LogoutIdentity, AuthError> {
    let header = decode_header(token).map_err(|e| invalid(&e))?;
    if !self.options.algorithms.contains(&header.alg) {
      return Err(AuthError::InvalidToken(format!(
        "algorithm {:?} is not accepted",
        header.alg
      )));
    }
    let mut validation = Validation::new(header.alg);
    validation.leeway = self.options.leeway_secs;
    validation.set_issuer(&[&self.options.issuer]);
    validation.set_audience(&self.options.logout_audiences);
    validation.set_required_spec_claims(&["iss", "aud"]);
    let claims = decode::<LogoutClaims>(token, key, &validation)
      .map_err(|e| invalid(&e))?
      .claims;

    if claims.iat == 0 || claims.jti.as_deref().is_none_or(str::is_empty) {
      return Err(AuthError::InvalidToken("logout token needs iat and jti".into()));
    }
    if claims.nonce.is_some() {
      return Err(AuthError::InvalidToken(
        "a logout token must not carry a nonce".into(),
      ));
    }
    if !claims.events.contains_key(BACKCHANNEL_LOGOUT_EVENT) {
      return Err(AuthError::InvalidToken("not a back-channel logout token".into()));
    }
    if claims.sub.is_none() && claims.sid.is_none() {
      return Err(AuthError::InvalidToken(
        "logout token names neither sub nor sid".into(),
      ));
    }
    Ok(LogoutIdentity {
      maybe_subject: claims.sub,
      maybe_session_id: claims.sid,
    })
  }

  /// Everything except fetching the key; separate so it can be tested offline.
  fn verify(&self, token: &str, key: &DecodingKey) -> Result<TokenIdentity, AuthError> {
    let header = decode_header(token).map_err(|e| invalid(&e))?;
    if !self.options.algorithms.contains(&header.alg) {
      return Err(AuthError::InvalidToken(format!(
        "algorithm {:?} is not accepted",
        header.alg
      )));
    }

    let mut validation = Validation::new(header.alg);
    validation.leeway = self.options.leeway_secs;
    validation.validate_nbf = true;
    // The audience of Keycloak access tokens is often "account"; the client binding is checked
    // below against `azp` and `aud` instead.
    validation.validate_aud = false;
    validation.set_issuer(&[&self.options.issuer]);
    validation.set_required_spec_claims(&["exp", "iss", "sub"]);

    let claims = decode::<AccessClaims>(token, key, &validation)
      .map_err(|e| invalid(&e))?
      .claims;

    // An empty subject would map every such token to one shared account.
    if claims.sub.trim().is_empty() {
      return Err(AuthError::InvalidToken("token has no subject".into()));
    }
    if !claims.is_issued_to(&self.options.client_id) {
      return Err(AuthError::InvalidToken(
        "token was not issued for this application".into(),
      ));
    }
    let role = self
      .roles
      .resolve(claims.client_roles(&self.options.client_id))
      .ok_or(AuthError::NoTeamcraftRole)?;

    let display_name = claims
      .name
      .filter(|name| !name.trim().is_empty())
      .or_else(|| claims.preferred_username.clone())
      .unwrap_or_else(|| claims.sub.clone());
    Ok(TokenIdentity {
      issuer: self.options.issuer.clone(),
      subject: claims.sub,
      maybe_username: claims.preferred_username,
      display_name,
      maybe_email: claims.email,
      is_email_verified: claims.email_verified,
      role,
      issued_at: claims.iat,
      maybe_session_id: claims.sid,
    })
  }
}

impl AccessClaims {
  fn is_issued_to(&self, client_id: &str) -> bool {
    if self.azp.as_deref() == Some(client_id) {
      return true;
    }
    match &self.aud {
      Audience::None => false,
      Audience::One(aud) => aud == client_id,
      Audience::Many(auds) => auds.iter().any(|aud| aud == client_id),
    }
  }

  fn client_roles(&self, client_id: &str) -> impl Iterator<Item = &str> {
    self
      .resource_access
      .get(client_id)
      .into_iter()
      .flat_map(|access| access.roles.iter().map(String::as_str))
  }
}

/// Maps library errors to messages that tell the client what is wrong without leaking internals.
fn invalid(error: &jsonwebtoken::errors::Error) -> AuthError {
  let reason = match error.kind() {
    ErrorKind::ExpiredSignature => "token expired",
    ErrorKind::ImmatureSignature => "token is not valid yet",
    ErrorKind::InvalidSignature => "bad signature",
    ErrorKind::InvalidIssuer => "wrong issuer",
    ErrorKind::MissingRequiredClaim(_) => "missing required claim",
    ErrorKind::InvalidAlgorithm => "algorithm not accepted",
    _ => "malformed token",
  };
  AuthError::InvalidToken(reason.into())
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use jsonwebtoken::{EncodingKey, Header, encode};
  use serde_json::{Value, json};

  use super::*;
  use crate::auth::jwks::{BoxFuture, JwkSource, JwksCacheOptions};
  use crate::auth::test_keys::{TEST_KEY_ID, TEST_PRIVATE_KEY_PEM, TEST_PUBLIC_KEY_PEM};
  use crate::config::RoleNames;

  const ISSUER: &str = "https://auth.example.com/realms/company";
  const CLIENT: &str = "teamcraft";

  #[test]
  fn accepts_valid_token_and_maps_highest_role() {
    let token = sign(&claims(
      json!({"resource_access": {CLIENT: {"roles": ["guest", "admin"]}}}),
    ));
    let identity = validator().verify(&token, &public_key()).unwrap();
    assert_eq!(identity.role, Role::Admin);
    assert_eq!(identity.subject, "user-1");
    assert_eq!(identity.display_name, "Alice Example");
    assert_eq!(identity.maybe_username.as_deref(), Some("alice"));
    assert_eq!(identity.maybe_email.as_deref(), Some("alice@example.com"));
    assert!(identity.is_email_verified);
  }

  #[test]
  fn unverified_email_is_flagged() {
    let token = sign(&claims(json!({"email_verified": false})));
    assert!(
      !validator()
        .verify(&token, &public_key())
        .unwrap()
        .is_email_verified
    );
    let token = sign(&claims(json!({"email_verified": null, "email": null})));
    let identity = validator().verify(&token, &public_key()).unwrap();
    assert_eq!((identity.maybe_email, identity.is_email_verified), (None, false));
  }

  #[test]
  fn rejects_users_without_teamcraft_role() {
    let token = sign(&claims(
      json!({"resource_access": {CLIENT: {"roles": ["offline_access"]}}}),
    ));
    assert!(matches!(
      validator().verify(&token, &public_key()),
      Err(AuthError::NoTeamcraftRole)
    ));
    // Roles of other clients do not count.
    let token = sign(&claims(
      json!({"resource_access": {"other-app": {"roles": ["owner"]}}}),
    ));
    assert!(matches!(
      validator().verify(&token, &public_key()),
      Err(AuthError::NoTeamcraftRole)
    ));
    let token = sign(&claims(json!({"resource_access": null})));
    assert!(validator().verify(&token, &public_key()).is_err());
  }

  #[test]
  fn rejects_expired_token_beyond_leeway() {
    let token = sign(&claims(json!({"exp": now() - 3600})));
    assert_invalid(validator().verify(&token, &public_key()), "token expired");
  }

  #[test]
  fn rejects_wrong_issuer() {
    let token = sign(&claims(json!({"iss": "https://evil.example.com/realms/company"})));
    assert_invalid(validator().verify(&token, &public_key()), "wrong issuer");
  }

  #[test]
  fn rejects_token_issued_to_another_client() {
    let token = sign(&claims(json!({"azp": "other-app", "aud": "account"})));
    assert!(matches!(
      validator().verify(&token, &public_key()),
      Err(AuthError::InvalidToken(_))
    ));
  }

  #[test]
  fn accepts_client_in_audience_list_without_azp() {
    let token = sign(&claims(json!({"azp": null, "aud": ["account", CLIENT]})));
    assert!(validator().verify(&token, &public_key()).is_ok());
  }

  #[test]
  fn rejects_tampered_payload() {
    let token = sign(&claims(
      json!({"resource_access": {CLIENT: {"roles": ["guest"]}}}),
    ));
    let forged = sign_parts_swapped(
      &token,
      &claims(json!({"resource_access": {CLIENT: {"roles": ["owner"]}}})),
    );
    assert_invalid(validator().verify(&forged, &public_key()), "bad signature");
  }

  #[test]
  fn rejects_unaccepted_algorithm() {
    let options = ValidatorOptions {
      algorithms: vec![Algorithm::ES256],
      ..options()
    };
    let validator = validator_with(options, RoleNames::default());
    let token = sign(&claims(json!({})));
    assert!(matches!(
      validator.verify(&token, &public_key()),
      Err(AuthError::InvalidToken(_))
    ));
  }

  #[test]
  fn rejects_hmac_token_signed_with_public_key_material() {
    // Classic algorithm confusion: HS256 token "signed" with the public key must not pass.
    let header = Header::new(Algorithm::HS256);
    let token = encode(
      &header,
      &claims(json!({})),
      &EncodingKey::from_secret(TEST_PUBLIC_KEY_PEM.as_bytes()),
    )
    .unwrap();
    assert!(validator().verify(&token, &public_key()).is_err());
  }

  #[test]
  fn rejects_tokens_that_are_not_valid_yet() {
    let token = sign(&claims(json!({"nbf": now() + 3600})));
    assert!(validator().verify(&token, &public_key()).is_err());
  }

  #[test]
  fn rejects_tokens_without_subject_or_expiry() {
    for missing in ["sub", "exp", "iss"] {
      let token = sign(&claims(json!({ missing: null })));
      assert!(
        validator().verify(&token, &public_key()).is_err(),
        "without {missing}"
      );
    }
    let token = sign(&claims(json!({"sub": ""})));
    assert!(
      validator().verify(&token, &public_key()).is_err(),
      "empty subject"
    );
  }

  #[test]
  fn rejects_unsigned_tokens_with_alg_none() {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT","kid":"test-key"}"#);
    let payload = URL_SAFE_NO_PAD.encode(claims(json!({})).to_string());
    for token in [format!("{header}.{payload}."), format!("{header}.{payload}.AAAA")] {
      assert!(validator().verify(&token, &public_key()).is_err());
    }
  }

  #[test]
  fn rejects_a_token_signed_by_another_key_even_with_a_known_key_id() {
    let other = EncodingKey::from_secret(b"another key");
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(TEST_KEY_ID.into());
    let token = encode(&header, &claims(json!({})), &other).unwrap();
    assert!(validator().verify(&token, &public_key()).is_err());
  }

  #[test]
  fn rejects_garbage() {
    assert!(validator().verify("not-a-jwt", &public_key()).is_err());
    assert!(validator().verify("", &public_key()).is_err());
    assert!(validator().verify("a.b.c", &public_key()).is_err());
  }

  #[test]
  fn display_name_falls_back_to_username_then_subject() {
    let token = sign(&claims(json!({"name": null})));
    assert_eq!(
      validator().verify(&token, &public_key()).unwrap().display_name,
      "alice"
    );
    let token = sign(&claims(json!({"name": null, "preferred_username": null})));
    assert_eq!(
      validator().verify(&token, &public_key()).unwrap().display_name,
      "user-1"
    );
  }

  #[test]
  fn custom_role_names_apply() {
    let names = RoleNames {
      guest: "external".into(),
      ..RoleNames::default()
    };
    let validator = validator_with(options(), names);
    let token = sign(&claims(
      json!({"resource_access": {CLIENT: {"roles": ["external"]}}}),
    ));
    assert_eq!(validator.verify(&token, &public_key()).unwrap().role, Role::Guest);
  }

  #[tokio::test]
  async fn validate_requires_a_key_id() {
    let header = Header::new(Algorithm::RS256);
    let key = EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).unwrap();
    let token = encode(&header, &claims(json!({})), &key).unwrap();
    assert!(matches!(
      validator().validate(&token).await,
      Err(AuthError::InvalidToken(_))
    ));
  }

  fn logout_claims(overrides: Value) -> Value {
    let mut claims = json!({
      "iss": ISSUER,
      "aud": CLIENT,
      "iat": now(),
      "jti": "logout-1",
      "sub": "user-1",
      "events": {"http://schemas.openid.net/event/backchannel-logout": {}},
    });
    if let (Some(base), Some(extra)) = (claims.as_object_mut(), overrides.as_object()) {
      for (key, value) in extra {
        if value.is_null() {
          base.remove(key);
        } else {
          base.insert(key.clone(), value.clone());
        }
      }
    }
    claims
  }

  #[test]
  fn accepts_a_valid_logout_token_for_a_subject_or_a_session() {
    let by_sub = validator()
      .verify_logout(&sign(&logout_claims(json!({}))), &public_key())
      .unwrap();
    assert_eq!(
      by_sub,
      LogoutIdentity {
        maybe_subject: Some("user-1".into()),
        maybe_session_id: None
      }
    );
    let by_sid = sign(&logout_claims(
      json!({"sub": null, "sid": "s-1", "aud": "teamcraft-server"}),
    ));
    let identity = validator().verify_logout(&by_sid, &public_key()).unwrap();
    assert_eq!(
      (identity.maybe_subject, identity.maybe_session_id.as_deref()),
      (None, Some("s-1"))
    );
  }

  #[test]
  fn rejects_logout_tokens_that_break_the_spec() {
    for overrides in [
      json!({"nonce": "x"}),
      json!({"events": {}}),
      json!({"events": {"http://schemas.openid.net/event/other": {}}}),
      json!({"sub": null}),
      json!({"jti": null}),
      json!({"iat": null}),
      json!({"aud": "some-other-client"}),
      json!({"iss": "https://evil.example.com/realms/company"}),
    ] {
      let token = sign(&logout_claims(overrides.clone()));
      assert!(
        validator().verify_logout(&token, &public_key()).is_err(),
        "{overrides}"
      );
    }
  }

  #[test]
  fn an_access_token_is_not_a_logout_token() {
    let token = sign(&claims(json!({})));
    assert!(validator().verify_logout(&token, &public_key()).is_err());
  }

  #[test]
  fn access_tokens_carry_issue_time_and_session() {
    let token = sign(&claims(json!({"iat": 1_800_000_000, "sid": "s-9"})));
    let identity = validator().verify(&token, &public_key()).unwrap();
    assert_eq!(
      (identity.issued_at, identity.maybe_session_id.as_deref()),
      (1_800_000_000, Some("s-9"))
    );
  }

  #[test]
  fn parses_algorithm_names() {
    assert_eq!(
      TokenValidator::parse_algorithms(&["RS256".into(), "EdDSA".into()])
        .unwrap()
        .len(),
      2
    );
    assert!(TokenValidator::parse_algorithms(&["none".into()]).is_err());
    assert!(TokenValidator::parse_algorithms(&[]).is_err());
  }

  fn assert_invalid(result: Result<TokenIdentity, AuthError>, expected: &str) {
    match result {
      Err(AuthError::InvalidToken(reason)) => assert_eq!(reason, expected),
      other => panic!("expected invalid token ({expected}), got {other:?}"),
    }
  }

  fn options() -> ValidatorOptions {
    ValidatorOptions {
      issuer: ISSUER.into(),
      client_id: CLIENT.into(),
      algorithms: vec![Algorithm::RS256],
      leeway_secs: 30,
      logout_audiences: vec![CLIENT.into(), "teamcraft-server".into()],
    }
  }

  fn validator() -> TokenValidator {
    validator_with(options(), RoleNames::default())
  }

  fn validator_with(options: ValidatorOptions, names: RoleNames) -> TokenValidator {
    let cache = JwksCache::new(
      Box::new(NoKeys),
      JwksCacheOptions {
        ttl: Duration::from_secs(60),
        min_refresh_interval: Duration::from_secs(1),
      },
    );
    TokenValidator::new(cache, RoleMapping::new(names), options)
  }

  struct NoKeys;

  impl JwkSource for NoKeys {
    fn fetch(&self) -> BoxFuture<'_, Result<jsonwebtoken::jwk::JwkSet, AuthError>> {
      Box::pin(async { Err(AuthError::KeysUnavailable("no network in tests".into())) })
    }
  }

  fn public_key() -> DecodingKey {
    DecodingKey::from_rsa_pem(TEST_PUBLIC_KEY_PEM.as_bytes()).unwrap()
  }

  fn now() -> i64 {
    i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap()
  }

  /// A valid claim set; `overrides` replaces or (with `null`) removes top-level claims.
  fn claims(overrides: Value) -> Value {
    let mut claims = json!({
      "iss": ISSUER,
      "sub": "user-1",
      "azp": CLIENT,
      "aud": "account",
      "exp": now() + 300,
      "preferred_username": "alice",
      "name": "Alice Example",
      "email": "alice@example.com",
      "email_verified": true,
      "resource_access": {CLIENT: {"roles": ["member"]}},
    });
    if let (Some(base), Some(extra)) = (claims.as_object_mut(), overrides.as_object()) {
      for (key, value) in extra {
        if value.is_null() {
          base.remove(key);
        } else {
          base.insert(key.clone(), value.clone());
        }
      }
    }
    claims
  }

  fn sign(claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(TEST_KEY_ID.into());
    let key = EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY_PEM.as_bytes()).unwrap();
    encode(&header, claims, &key).unwrap()
  }

  /// Header and signature of `token` with the payload of a differently signed token.
  fn sign_parts_swapped(token: &str, other_claims: &Value) -> String {
    let other = sign(other_claims);
    let parts: Vec<&str> = token.split('.').collect();
    let other_parts: Vec<&str> = other.split('.').collect();
    format!("{}.{}.{}", parts[0], other_parts[1], parts[2])
  }
}
