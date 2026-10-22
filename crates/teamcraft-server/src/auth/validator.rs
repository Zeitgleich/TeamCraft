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
  resource_access: HashMap<String, ClientAccess>,
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
