//! Checking an OIDC attestation of an identity key (e2ee.md §3.1, security review A1).
//!
//! The identity provider runs on a separate host and puts the claim `tc_identity_key` into the
//! user's ID token. The claim value binds the TeamCraft user ID and the identity key fingerprint
//! (`"<user id>:<fingerprint>"`, [`attestation_claim`]), so neither the identity key nor the mapping
//! from the token's subject to a TeamCraft user comes from the server's word. A client verifies the
//! token against the identity provider's own signing keys (JWKS, fetched from the configured
//! issuer, never through the TeamCraft server) before it pins a colleague's identity key.
//!
//! Supported signature algorithms: RS256 (what identity providers sign with unless told otherwise),
//! ES256 and EdDSA (Ed25519). The token's expiry is not checked: the attestation is a statement
//! about a key, kept as long as the key is.
//!
//! [`verify_role_evidence`] reads more out of the same token: the roles the provider gave the person
//! when it issued the token, and when that was (e2ee.md §3.1.3, issue #1276). Owners' apps sign the
//! admin roster on that evidence.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Verifier as _;
use rsa_sha2::Digest as _;
use serde_json::Value;

use crate::identity::IdentityPublicKey;
use crate::ids::UserId;

/// The ID token claim (and identity provider user attribute) carrying the attestation.
pub const ATTESTATION_CLAIM: &str = "tc_identity_key";
/// Longest token accepted (bytes); ID tokens are a few kilobytes at most.
const MAX_TOKEN_BYTES: usize = 16 * 1024;
/// RSA keys outside this range aren't taken: shorter ones are weak, longer ones cost a device a lot
/// of work for a signature a hostile server can make it check.
const MIN_RSA_BITS: usize = 2048;
const MAX_RSA_BITS: usize = 8192;

/// What a token must state to attest an identity key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestationExpectations<'a> {
  /// The configured issuer, e.g. `https://id.example.com/realms/company`.
  pub issuer: &'a str,
  /// The TeamCraft client ID the token was issued to.
  pub audience: &'a str,
}

/// An attestation that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAttestation {
  pub subject: String,
}

/// What the identity provider said about a person in a token that passed every check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleEvidence {
  pub subject: String,
  /// The token's `iat`: the provider's clock, seconds since the epoch. Evidence is ordered by this
  /// alone, never by a device's clock or the server's word.
  pub issued_at: i64,
  /// The client roles in the token (`resource_access.<client>.roles`). `None` when the token has no
  /// such claim, which says nothing about the person: the realm may simply not map roles into ID
  /// tokens.
  pub maybe_roles: Option<Vec<String>>,
}

/// What a token claims, before anybody checked its signature. Only for a token this device got
/// from the provider itself, to see what it still has to do; decisions take [`verify_role_evidence`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnverifiedClaims {
  pub issuer: String,
  pub audience: Vec<String>,
  /// The client the token was issued to, if it says (`azp`).
  pub maybe_authorized_party: Option<String>,
  pub maybe_issued_at: Option<i64>,
  pub maybe_key_claim: Option<String>,
  pub maybe_roles: Option<Vec<String>>,
}

#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
  #[error("the token is not a well-formed JWT")]
  Malformed,
  #[error("the token is signed with {0}, which isn't accepted")]
  UnsupportedAlgorithm(String),
  #[error("no key of the identity provider matches the token")]
  UnknownKey,
  #[error("the token's signature is invalid")]
  InvalidSignature,
  #[error("the token was issued by {0}, not the configured identity provider")]
  WrongIssuer(String),
  #[error("the token was issued to another client")]
  WrongAudience,
  #[error("the token attests another user or identity key")]
  WrongClaim,
  /// Not an ID token: an access or logout token names no key on purpose.
  #[error("the token is of the type {0}, not an ID token")]
  WrongType(String),
  #[error("the token doesn't say when it was issued")]
  NoIssueTime,
}

/// The claim value for `user`'s identity key.
pub fn attestation_claim(user: UserId, identity: &IdentityPublicKey) -> String {
  format!("{}:{}", user.get(), identity.fingerprint())
}

/// Verifies `token` against the identity provider's JWKS (`jwks_json`, the raw JSON document) and
/// checks that it attests `identity` for `user`.
pub fn verify_attestation(
  token: &str,
  jwks_json: &[u8],
  expected: &AttestationExpectations<'_>,
  user: UserId,
  identity: &IdentityPublicKey,
) -> Result<VerifiedAttestation, AttestationError> {
  let claims = verified_claims(token, jwks_json)?;
  check_claims(&claims, expected, user, identity)
}

/// [`verify_attestation`], and what else the token says: the roles and the time of issue, for
/// signing the admin roster (e2ee.md §3.1.3). Only ID tokens count here.
pub fn verify_role_evidence(
  token: &str,
  jwks_json: &[u8],
  expected: &AttestationExpectations<'_>,
  user: UserId,
  identity: &IdentityPublicKey,
) -> Result<RoleEvidence, AttestationError> {
  let claims = verified_claims(token, jwks_json)?;
  let verified = check_claims(&claims, expected, user, identity)?;
  if let Some(kind) = claims.get("typ").and_then(Value::as_str)
    && !kind.eq_ignore_ascii_case("ID")
  {
    return Err(AttestationError::WrongType(kind.to_string()));
  }
  let issued_at = claims
    .get("iat")
    .and_then(Value::as_i64)
    .ok_or(AttestationError::NoIssueTime)?;
  Ok(RoleEvidence {
    subject: verified.subject,
    issued_at,
    maybe_roles: client_roles(&claims, expected.audience),
  })
}

/// What an access token of this client says about a person's roles: the evidence that somebody lost a
/// role, kept by the server from the requests it saw (e2ee.md §3.1.3). Names no identity key, so it can
/// only count for an account already noted. No role of this client means none, not "not said": unlike ID
/// tokens, access tokens always carry them, or nobody could use TeamCraft at all.
pub fn verify_role_loss(
  token: &str,
  jwks_json: &[u8],
  expected: &AttestationExpectations<'_>,
) -> Result<RoleEvidence, AttestationError> {
  let claims = verified_claims(token, jwks_json)?;
  let issuer = claims.get("iss").and_then(Value::as_str).unwrap_or_default();
  if issuer.trim_end_matches('/') != expected.issuer.trim_end_matches('/') {
    return Err(AttestationError::WrongIssuer(issuer.to_string()));
  }
  // Access tokens name the API in `aud`, and the client that asked in `azp`.
  if claims.get("azp").and_then(Value::as_str) != Some(expected.audience) {
    return Err(AttestationError::WrongAudience);
  }
  let kind = claims.get("typ").and_then(Value::as_str).unwrap_or_default();
  if !kind.eq_ignore_ascii_case("Bearer") && !kind.eq_ignore_ascii_case("DPoP") {
    return Err(AttestationError::WrongType(kind.to_string()));
  }
  Ok(RoleEvidence {
    subject: claims
      .get("sub")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_string(),
    issued_at: claims
      .get("iat")
      .and_then(Value::as_i64)
      .ok_or(AttestationError::NoIssueTime)?,
    maybe_roles: Some(client_roles(&claims, expected.audience).unwrap_or_default()),
  })
}

/// Reads a token's claims without checking its signature: see [`UnverifiedClaims`].
pub fn unverified_claims(token: &str) -> Result<UnverifiedClaims, AttestationError> {
  if token.len() > MAX_TOKEN_BYTES {
    return Err(AttestationError::Malformed);
  }
  let payload = token.split('.').nth(1).ok_or(AttestationError::Malformed)?;
  let claims = decode_json(payload)?;
  let audience = match claims.get("aud") {
    Some(Value::String(audience)) => vec![audience.clone()],
    Some(Value::Array(audiences)) => audiences
      .iter()
      .filter_map(Value::as_str)
      .map(str::to_string)
      .collect(),
    _ => Vec::new(),
  };
  let maybe_authorized_party = claims.get("azp").and_then(Value::as_str).map(str::to_string);
  let maybe_roles = maybe_authorized_party
    .iter()
    .chain(audience.iter())
    .find_map(|client| client_roles(&claims, client));
  Ok(UnverifiedClaims {
    issuer: claims
      .get("iss")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_string(),
    audience,
    maybe_authorized_party,
    maybe_issued_at: claims.get("iat").and_then(Value::as_i64),
    maybe_key_claim: key_claim(&claims).map(str::to_string),
    maybe_roles,
  })
}

/// The roles of `client` in a token, without checking its signature: see [`UnverifiedClaims`].
pub fn unverified_client_roles(token: &str, client: &str) -> Option<Vec<String>> {
  if token.len() > MAX_TOKEN_BYTES {
    return None;
  }
  let claims = decode_json(token.split('.').nth(1)?).ok()?;
  client_roles(&claims, client)
}

/// The JSON claims of a token signed by a key of `jwks_json`.
fn verified_claims(token: &str, jwks_json: &[u8]) -> Result<Value, AttestationError> {
  if token.len() > MAX_TOKEN_BYTES {
    return Err(AttestationError::Malformed);
  }
  let mut parts = token.split('.');
  let (Some(header), Some(payload), Some(signature), None) =
    (parts.next(), parts.next(), parts.next(), parts.next())
  else {
    return Err(AttestationError::Malformed);
  };
  let header_json = decode_json(header)?;
  let claims = decode_json(payload)?;
  let signature = URL_SAFE_NO_PAD
    .decode(signature)
    .map_err(|_| AttestationError::Malformed)?;
  let signed = &token.as_bytes()[..header.len() + 1 + payload.len()];
  let algorithm = header_json
    .get("alg")
    .and_then(Value::as_str)
    .ok_or(AttestationError::Malformed)?;
  let maybe_kid = header_json.get("kid").and_then(Value::as_str);
  let jwks: Value = serde_json::from_slice(jwks_json).map_err(|_| AttestationError::UnknownKey)?;
  let keys = jwks
    .get("keys")
    .and_then(Value::as_array)
    .ok_or(AttestationError::UnknownKey)?;
  let is_verified = keys
    .iter()
    .filter(|key| maybe_kid.is_none_or(|kid| key.get("kid").and_then(Value::as_str) == Some(kid)))
    .any(|key| verify_with(algorithm, key, signed, &signature).unwrap_or(false));
  if !is_verified {
    return Err(match algorithm {
      "RS256" | "ES256" | "EdDSA" => {
        if keys.is_empty() {
          AttestationError::UnknownKey
        } else {
          AttestationError::InvalidSignature
        }
      },
      other => AttestationError::UnsupportedAlgorithm(other.to_string()),
    });
  }
  Ok(claims)
}

fn check_claims(
  claims: &Value,
  expected: &AttestationExpectations<'_>,
  user: UserId,
  identity: &IdentityPublicKey,
) -> Result<VerifiedAttestation, AttestationError> {
  let issuer = claims.get("iss").and_then(Value::as_str).unwrap_or_default();
  if issuer.trim_end_matches('/') != expected.issuer.trim_end_matches('/') {
    return Err(AttestationError::WrongIssuer(issuer.to_string()));
  }
  // Keycloak names the client in `azp` too; a token issued to another client with this one in its
  // audience isn't this client's token (the server's validator draws the same line).
  if let Some(authorized) = claims.get("azp").and_then(Value::as_str)
    && authorized != expected.audience
  {
    return Err(AttestationError::WrongAudience);
  }
  let is_for_us = match claims.get("aud") {
    Some(Value::String(audience)) => audience == expected.audience,
    Some(Value::Array(audiences)) => audiences
      .iter()
      .any(|audience| audience.as_str() == Some(expected.audience)),
    _ => false,
  };
  if !is_for_us {
    return Err(AttestationError::WrongAudience);
  }
  if key_claim(claims) != Some(attestation_claim(user, identity).as_str()) {
    return Err(AttestationError::WrongClaim);
  }
  Ok(VerifiedAttestation {
    subject: claims
      .get("sub")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_string(),
  })
}

fn key_claim(claims: &Value) -> Option<&str> {
  match claims.get(ATTESTATION_CLAIM) {
    Some(Value::String(value)) => Some(value.as_str()),
    // Multivalued user attributes arrive as arrays.
    Some(Value::Array(values)) => values.first().and_then(Value::as_str),
    _ => None,
  }
}

/// `resource_access.<client>.roles`, where the server's validator reads the roles of access tokens.
fn client_roles(claims: &Value, client: &str) -> Option<Vec<String>> {
  let roles = claims
    .get("resource_access")?
    .get(client)?
    .get("roles")?
    .as_array()?;
  Some(
    roles
      .iter()
      .filter_map(Value::as_str)
      .map(str::to_string)
      .collect(),
  )
}

/// `Ok(false)` for a key of another type than the algorithm needs.
fn verify_with(
  algorithm: &str,
  jwk: &Value,
  signed: &[u8],
  signature: &[u8],
) -> Result<bool, AttestationError> {
  let field = |name: &str| -> Result<Vec<u8>, AttestationError> {
    let value = jwk
      .get(name)
      .and_then(Value::as_str)
      .ok_or(AttestationError::UnknownKey)?;
    URL_SAFE_NO_PAD
      .decode(value)
      .map_err(|_| AttestationError::UnknownKey)
  };
  let kty = jwk.get("kty").and_then(Value::as_str);
  let crv = jwk.get("crv").and_then(Value::as_str);
  match (algorithm, kty, crv) {
    ("ES256", Some("EC"), Some("P-256")) => {
      let mut point = vec![0x04];
      point.extend(field("x")?);
      point.extend(field("y")?);
      let key =
        p256::ecdsa::VerifyingKey::from_sec1_bytes(&point).map_err(|_| AttestationError::UnknownKey)?;
      let signature =
        p256::ecdsa::Signature::from_slice(signature).map_err(|_| AttestationError::InvalidSignature)?;
      Ok(key.verify(signed, &signature).is_ok())
    },
    ("EdDSA", Some("OKP"), Some("Ed25519")) => {
      let x = <[u8; 32]>::try_from(field("x")?).map_err(|_| AttestationError::UnknownKey)?;
      let key = ed25519_dalek::VerifyingKey::from_bytes(&x).map_err(|_| AttestationError::UnknownKey)?;
      let signature =
        ed25519_dalek::Signature::from_slice(signature).map_err(|_| AttestationError::InvalidSignature)?;
      Ok(key.verify_strict(signed, &signature).is_ok())
    },
    ("RS256", Some("RSA"), _) => {
      let modulus = rsa::BigUint::from_bytes_be(&field("n")?);
      let exponent = rsa::BigUint::from_bytes_be(&field("e")?);
      if !(MIN_RSA_BITS..=MAX_RSA_BITS).contains(&modulus.bits()) {
        return Err(AttestationError::UnknownKey);
      }
      let key = rsa::RsaPublicKey::new_with_max_size(modulus, exponent, MAX_RSA_BITS)
        .map_err(|_| AttestationError::UnknownKey)?;
      let digest = rsa_sha2::Sha256::digest(signed);
      Ok(
        key
          .verify(rsa::Pkcs1v15Sign::new::<rsa_sha2::Sha256>(), &digest, signature)
          .is_ok(),
      )
    },
    _ => Ok(false),
  }
}

fn decode_json(part: &str) -> Result<Value, AttestationError> {
  let bytes = URL_SAFE_NO_PAD
    .decode(part)
    .map_err(|_| AttestationError::Malformed)?;
  serde_json::from_slice(&bytes).map_err(|_| AttestationError::Malformed)
}

#[cfg(test)]
mod tests {
  use p256::ecdsa::SigningKey;
  use p256::ecdsa::signature::Signer as _;
  use rand_core::OsRng;
  use serde_json::json;

  use super::*;
  use crate::identity::IdentityKeyPair;

  const ISSUER: &str = "https://id.example.com/realms/company";
  const CLIENT: &str = "teamcraft";
  const ALICE: UserId = UserId::new(7);

  #[test]
  fn an_es256_token_from_the_identity_provider_attests_the_key() {
    let provider = Provider::new();
    let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
    let token = provider.token(json!({
      "iss": ISSUER,
      "aud": [CLIENT, "account"],
      "sub": "abc",
      ATTESTATION_CLAIM: [attestation_claim(ALICE, &identity)],
    }));
    let verified = verify_attestation(&token, &provider.jwks(), &expectations(), ALICE, &identity).unwrap();
    assert_eq!(verified.subject, "abc");
  }

  mod role_evidence {
    use super::*;

    #[test]
    fn says_who_has_which_role_and_when() {
      let provider = Provider::new();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      let token = provider.token(json!({
        "iss": ISSUER, "aud": CLIENT, "azp": CLIENT, "sub": "abc", "typ": "ID", "iat": 1_000,
        "resource_access": { CLIENT: { "roles": ["admin", "member"] }, "other": { "roles": ["owner"] } },
        ATTESTATION_CLAIM: attestation_claim(ALICE, &identity),
      }));
      let evidence =
        verify_role_evidence(&token, &provider.jwks(), &expectations(), ALICE, &identity).unwrap();
      assert_eq!(evidence.subject, "abc");
      assert_eq!(evidence.issued_at, 1_000);
      assert_eq!(
        evidence.maybe_roles,
        Some(vec!["admin".to_string(), "member".to_string()])
      );
    }

    #[test]
    fn a_token_without_the_role_claim_says_nothing_about_roles() {
      let provider = Provider::new();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      // Roles of another client don't count either.
      let token = provider.token(json!({
        "iss": ISSUER, "aud": CLIENT, "sub": "abc", "iat": 1_000,
        "resource_access": { "other": { "roles": ["owner"] } },
        ATTESTATION_CLAIM: attestation_claim(ALICE, &identity),
      }));
      let evidence =
        verify_role_evidence(&token, &provider.jwks(), &expectations(), ALICE, &identity).unwrap();
      assert_eq!(evidence.maybe_roles, None);
    }

    #[test]
    fn only_a_dated_id_token_of_this_client_counts() {
      let provider = Provider::new();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      let jwks = provider.jwks();
      let check = |claims: Value| {
        verify_role_evidence(&provider.token(claims), &jwks, &expectations(), ALICE, &identity)
      };
      let claim = attestation_claim(ALICE, &identity);
      assert!(matches!(
        check(json!({ "iss": ISSUER, "aud": CLIENT, "sub": "abc", ATTESTATION_CLAIM: claim })),
        Err(AttestationError::NoIssueTime)
      ));
      assert!(matches!(
        check(json!({ "iss": ISSUER, "aud": CLIENT, "typ": "Bearer", "iat": 1, ATTESTATION_CLAIM: claim })),
        Err(AttestationError::WrongType(_))
      ));
      assert!(matches!(
        check(json!({ "iss": ISSUER, "aud": CLIENT, "azp": "other", "iat": 1, ATTESTATION_CLAIM: claim })),
        Err(AttestationError::WrongAudience)
      ));
    }

    #[test]
    fn a_device_can_read_its_own_token_before_anything_is_checked() {
      let provider = Provider::new();
      let token = provider.token(json!({
        "iss": ISSUER, "aud": [CLIENT, "account"], "iat": 5,
        "resource_access": { CLIENT: { "roles": ["owner"] } },
        ATTESTATION_CLAIM: ["7:ab"],
      }));
      let claims = unverified_claims(&token).unwrap();
      assert_eq!(claims.issuer, ISSUER);
      assert_eq!(claims.maybe_issued_at, Some(5));
      assert_eq!(claims.maybe_key_claim.as_deref(), Some("7:ab"));
      assert_eq!(claims.maybe_roles, Some(vec!["owner".to_string()]));
      assert!(unverified_claims("garbage").is_err());
    }
  }

  mod role_loss {
    use super::*;

    #[test]
    fn an_access_token_without_a_role_of_this_client_says_none() {
      let provider = Provider::new();
      let token = provider.token(json!({
        "iss": ISSUER, "aud": "account", "azp": CLIENT, "typ": "Bearer", "sub": "abc", "iat": 7,
        "resource_access": { "account": { "roles": ["manage-account"] } },
      }));
      let evidence = verify_role_loss(&token, &provider.jwks(), &expectations()).unwrap();
      assert_eq!(evidence.subject, "abc");
      assert_eq!(evidence.issued_at, 7);
      assert_eq!(evidence.maybe_roles, Some(Vec::new()));
    }

    #[test]
    fn a_forged_token_or_one_of_another_client_or_kind_says_nothing() {
      let provider = Provider::new();
      let jwks = provider.jwks();
      let claims =
        |azp: &str, typ: &str| json!({ "iss": ISSUER, "azp": azp, "typ": typ, "sub": "abc", "iat": 7 });
      assert!(verify_role_loss(&provider.token(claims(CLIENT, "DPoP")), &jwks, &expectations()).is_ok());
      assert!(matches!(
        verify_role_loss(
          &Provider::new().token(claims(CLIENT, "Bearer")),
          &jwks,
          &expectations()
        ),
        Err(AttestationError::InvalidSignature)
      ));
      assert!(matches!(
        verify_role_loss(&provider.token(claims("other", "Bearer")), &jwks, &expectations()),
        Err(AttestationError::WrongAudience)
      ));
      assert!(matches!(
        verify_role_loss(&provider.token(claims(CLIENT, "ID")), &jwks, &expectations()),
        Err(AttestationError::WrongType(_))
      ));
      let untyped = json!({ "iss": ISSUER, "azp": CLIENT, "sub": "abc", "iat": 7 });
      assert!(matches!(
        verify_role_loss(&provider.token(untyped), &jwks, &expectations()),
        Err(AttestationError::WrongType(_))
      ));
    }
  }

  mod rs256 {
    use rsa::RsaPrivateKey;
    use rsa::traits::PublicKeyParts as _;
    use rsa_sha2::Sha256;

    use super::*;

    #[test]
    fn a_token_signed_with_rsa_attests_the_key() {
      let key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      let token = rsa_token(
        &key,
        &json!({
          "iss": ISSUER, "aud": CLIENT, "sub": "abc", "iat": 9,
          ATTESTATION_CLAIM: attestation_claim(ALICE, &identity),
        }),
      );
      let jwks = rsa_jwks(&rsa::RsaPublicKey::from(&key));
      let evidence = verify_role_evidence(&token, &jwks, &expectations(), ALICE, &identity).unwrap();
      assert_eq!(evidence.issued_at, 9);
    }

    #[test]
    fn another_key_a_tampered_token_and_a_short_key_are_refused() {
      let key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      let claims = json!({
        "iss": ISSUER, "aud": CLIENT, "sub": "abc", "iat": 9,
        ATTESTATION_CLAIM: attestation_claim(ALICE, &identity),
      });
      let token = rsa_token(&key, &claims);
      let other = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
      assert!(matches!(
        verify_role_evidence(
          &token,
          &rsa_jwks(&rsa::RsaPublicKey::from(&other)),
          &expectations(),
          ALICE,
          &identity
        ),
        Err(AttestationError::InvalidSignature)
      ));
      let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
      let mut changed = claims.clone();
      changed["resource_access"] = json!({ CLIENT: { "roles": ["owner"] } });
      parts[1] = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).unwrap());
      assert!(matches!(
        verify_role_evidence(
          &parts.join("."),
          &rsa_jwks(&rsa::RsaPublicKey::from(&key)),
          &expectations(),
          ALICE,
          &identity
        ),
        Err(AttestationError::InvalidSignature)
      ));
      let short = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
      let short_token = rsa_token(&short, &claims);
      assert!(
        verify_role_evidence(
          &short_token,
          &rsa_jwks(&rsa::RsaPublicKey::from(&short)),
          &expectations(),
          ALICE,
          &identity
        )
        .is_err()
      );
    }

    fn rsa_token(key: &RsaPrivateKey, claims: &Value) -> String {
      let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"k1"}"#);
      let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
      let input = format!("{header}.{payload}");
      let digest = Sha256::digest(input.as_bytes());
      let signature = key.sign(rsa::Pkcs1v15Sign::new::<Sha256>(), &digest).unwrap();
      format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn rsa_jwks(key: &rsa::RsaPublicKey) -> Vec<u8> {
      serde_json::to_vec(&json!({
        "keys": [{
          "kty": "RSA", "kid": "k1", "alg": "RS256",
          "n": URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
          "e": URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
        }]
      }))
      .unwrap()
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn rejects_other_keys_users_issuers_and_signers() {
      let provider = Provider::new();
      let identity = IdentityKeyPair::generate(&mut OsRng).public_key();
      let other_key = IdentityKeyPair::generate(&mut OsRng).public_key();
      let claims = |iss: &str, aud: &str, claim: String| json!({ "iss": iss, "aud": aud, "sub": "abc", ATTESTATION_CLAIM: claim });
      let good = attestation_claim(ALICE, &identity);
      let jwks = provider.jwks();
      let check = |token: &str| verify_attestation(token, &jwks, &expectations(), ALICE, &identity);

      // The server substituted the identity key, or claims the token belongs to another user.
      assert!(matches!(
        check(&provider.token(claims(ISSUER, CLIENT, attestation_claim(ALICE, &other_key)))),
        Err(AttestationError::WrongClaim)
      ));
      assert!(matches!(
        check(&provider.token(claims(
          ISSUER,
          CLIENT,
          attestation_claim(UserId::new(8), &identity)
        ))),
        Err(AttestationError::WrongClaim)
      ));
      assert!(matches!(
        check(&provider.token(claims("https://evil.example", CLIENT, good.clone()))),
        Err(AttestationError::WrongIssuer(_))
      ));
      assert!(matches!(
        check(&provider.token(claims(ISSUER, "other-client", good.clone()))),
        Err(AttestationError::WrongAudience)
      ));
      // Signed by a key that isn't the identity provider's.
      let impostor = Provider::new();
      assert!(matches!(
        check(&impostor.token(claims(ISSUER, CLIENT, good.clone()))),
        Err(AttestationError::InvalidSignature)
      ));
      // Unsigned or with an algorithm the client doesn't accept.
      let token = provider.token(claims(ISSUER, CLIENT, good));
      let (_, rest) = token.split_once('.').unwrap();
      let none_header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
      assert!(matches!(
        check(&format!("{none_header}.{rest}")),
        Err(AttestationError::UnsupportedAlgorithm(_))
      ));
      for broken in ["", "a.b", "a.b.c.d", "!!.!!.!!"] {
        assert!(check(broken).is_err());
      }
    }
  }

  struct Provider {
    key: SigningKey,
  }

  impl Provider {
    fn new() -> Self {
      Self {
        key: SigningKey::random(&mut OsRng),
      }
    }

    fn token(&self, claims: Value) -> String {
      let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256","kid":"k1"}"#);
      let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
      let signing_input = format!("{header}.{payload}");
      let signature: p256::ecdsa::Signature = self.key.sign(signing_input.as_bytes());
      format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }

    fn jwks(&self) -> Vec<u8> {
      let point = self.key.verifying_key().to_encoded_point(false);
      serde_json::to_vec(&json!({
        "keys": [{
          "kty": "EC", "crv": "P-256", "kid": "k1", "alg": "ES256",
          "x": URL_SAFE_NO_PAD.encode(point.x().unwrap()),
          "y": URL_SAFE_NO_PAD.encode(point.y().unwrap()),
        }]
      }))
      .unwrap()
    }
  }

  fn expectations() -> AttestationExpectations<'static> {
    AttestationExpectations {
      issuer: ISSUER,
      audience: CLIENT,
    }
  }
}
