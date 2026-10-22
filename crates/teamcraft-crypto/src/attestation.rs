//! Checking an OIDC attestation of an identity key (e2ee.md §3.1, security review A1).
//!
//! The identity provider runs on a separate host and puts the claim `tc_identity_key` into the
//! user's ID token. The claim value binds the TeamCraft user ID and the identity key fingerprint
//! (`"<user id>:<fingerprint>"`, [`attestation_claim`]), so neither the identity key nor the mapping
//! from the token's subject to a TeamCraft user comes from the server's word. A client verifies the
//! token against the identity provider's own signing keys (JWKS, fetched from the configured
//! issuer, never through the TeamCraft server) before it pins a colleague's identity key.
//!
//! Supported signature algorithms: ES256 and EdDSA (Ed25519), so no RSA implementation is needed.
//! The token's expiry is not checked: the attestation is a statement about a key, kept as long as
//! the key is.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Verifier as _;
use serde_json::Value;

use crate::identity::IdentityPublicKey;
use crate::ids::UserId;

/// The ID token claim (and identity provider user attribute) carrying the attestation.
pub const ATTESTATION_CLAIM: &str = "tc_identity_key";
/// Longest token accepted (bytes); ID tokens are a few kilobytes at most.
const MAX_TOKEN_BYTES: usize = 16 * 1024;

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
      "ES256" | "EdDSA" => {
        if keys.is_empty() {
          AttestationError::UnknownKey
        } else {
          AttestationError::InvalidSignature
        }
      },
      other => AttestationError::UnsupportedAlgorithm(other.to_string()),
    });
  }
  check_claims(&claims, expected, user, identity)
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
  let claim = match claims.get(ATTESTATION_CLAIM) {
    Some(Value::String(value)) => Some(value.as_str()),
    // Multivalued user attributes arrive as arrays.
    Some(Value::Array(values)) => values.first().and_then(Value::as_str),
    _ => None,
  };
  if claim != Some(attestation_claim(user, identity).as_str()) {
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
