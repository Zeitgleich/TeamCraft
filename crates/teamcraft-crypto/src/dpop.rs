//! DPoP proofs (RFC 9449): binding an access token to a key that only the device holds
//! (e2ee.md §3.1.2, issue #120).
//!
//! A bearer token works for whoever has it, so a look-alike server that was handed a real
//! instance's token can use it there. A proof is a short-lived JWT, signed with the device's key,
//! that names the request's method and URL and the token's hash; the identity provider puts the
//! key's thumbprint into the token (`cnf.jkt`). A look-alike only ever gets proofs that name its
//! own origin, and the real server refuses those.
//!
//! This module has both sides and no opinion on policy: signing, the checks of RFC 9449 §4.3, and
//! the server's nonces. The replay cache and the policy live with the server, the only party that
//! has them. ES256 only, through the `p256` crate, as for Web Push.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use rand_core::{OsRng, RngCore as _};
use serde::Deserialize;
use serde::de::IgnoredAny;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use url::{Origin, Url};
use zeroize::Zeroizing;

/// The only signature algorithm accepted, also the `algs` value of a `WWW-Authenticate` challenge.
pub const DPOP_ALGORITHM: &str = "ES256";
/// Longest proof accepted (bytes): a real one is about 500 bytes plus the URL and the nonce.
pub const MAX_PROOF_BYTES: usize = 4096;
/// The `typ` a proof's header must carry (RFC 9449 §4.2).
const PROOF_TYPE: &str = "dpop+jwt";
/// Longest `jti` accepted (bytes); ours are 22 characters.
const MAX_JTI_BYTES: usize = 128;
/// Longest `nonce` accepted (bytes); ours are 43 characters.
const MAX_NONCE_BYTES: usize = 256;
/// Bytes of a P-256 coordinate.
const COORDINATE_BYTES: usize = 32;
/// Bytes of a SHA-256 hash, and characters of one in unpadded base64url.
const HASH_BYTES: usize = 32;
const HASH_CHARS: usize = 43;
/// Random bytes of a `jti`: more than the 96 bits RFC 9449 §4.2 asks for.
const JTI_RANDOM_BYTES: usize = 16;
const NONCE_RANDOM_BYTES: usize = 32;

/// A proof, ready for the `DPoP` header. Its `Debug` doesn't print it.
#[derive(Clone, PartialEq, Eq)]
pub struct DpopProof(String);

/// A device's proof key. The private scalar is wiped on drop (`SigningKey` zeroizes itself) and the
/// type is not `Clone`. Hardware-backed keys never become one of these: they sign elsewhere.
pub struct DpopSigningKey {
  key: SigningKey,
}

/// What a client signs a proof for.
#[derive(Debug, Clone, Copy)]
pub struct DpopProofRequest<'a> {
  pub method: &'a str,
  /// The request's URL; a query and a fragment are left out of the proof.
  pub target_uri: &'a str,
  /// By the client's best knowledge of the server's time, see e2ee.md §3.1.2.
  pub issued_at_unix: i64,
  /// The token the request carries; `None` only for a request to the token endpoint.
  pub maybe_access_token: Option<&'a str>,
  pub maybe_nonce: Option<&'a str>,
}

/// The RFC 7638 thumbprint of a proof key: what a bound token names as `cnf.jkt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JwkThumbprint(String);

/// An origin this server is publicly reachable on: `scheme://host[:port]`, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin(Origin);

/// What a proof must state to be accepted for a request.
#[derive(Debug, Clone, Copy)]
pub struct DpopExpectation<'a> {
  pub method: &'a str,
  /// The request's path as the server received it.
  pub path: &'a str,
  /// From the server's own configuration, never from a header of the request.
  pub public_origins: &'a [PublicOrigin],
  pub now_unix: i64,
  /// How old a proof may be, by the server's clock.
  pub max_age_secs: u64,
  /// How far the client's clock may be off, on both ends of the window.
  pub leeway_secs: u64,
  pub access_token: &'a str,
  /// The key the token is bound to; `None` for a token without `cnf.jkt`.
  pub maybe_bound_key: Option<&'a JwkThumbprint>,
}

/// A proof that passed every check that needs no server state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDpopProof {
  pub key: JwkThumbprint,
  /// Names the proof in a replay cache: fixed size and per key, so a client's choice of `jti` can
  /// neither grow the cache's entries nor collide with another client's.
  pub replay_id: [u8; HASH_BYTES],
  pub maybe_nonce: Option<String>,
}

/// The server's nonces: random and only in memory, so a restart invalidates every proof made
/// before it (e2ee.md §3.1.2). The current one and its predecessor are accepted.
#[derive(Debug)]
pub struct DpopNonces {
  current: String,
  current_since_unix: i64,
  maybe_previous: Option<String>,
}

/// Why a proof was refused. The texts are fixed: they go into `WWW-Authenticate` as they are, so
/// they quote nothing from the proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DpopError {
  #[error("the proof is too long")]
  Oversized,
  #[error("the proof is not a well-formed JWT")]
  Malformed,
  #[error("the proof header has the wrong typ")]
  WrongType,
  #[error("the proof must be signed with ES256")]
  UnsupportedAlgorithm,
  #[error("the proof header has no usable P-256 public key")]
  BadKey,
  #[error("the proof signature is invalid")]
  BadSignature,
  #[error("the proof names another method")]
  WrongMethod,
  #[error("the proof names another URL")]
  WrongUri,
  #[error("the proof is too old or from the future")]
  NotFresh,
  #[error("the proof needs a usable jti")]
  BadJti,
  #[error("the proof names another access token")]
  WrongAccessToken,
  #[error("the proof is signed with another key than the token is bound to")]
  WrongKey,
}

#[derive(Deserialize)]
struct ProofHeader {
  typ: String,
  alg: String,
  jwk: ProofJwk,
  /// Extensions we don't know must be understood (RFC 7515 §4.1.11), so any of them is a refusal.
  #[serde(default)]
  crit: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct ProofJwk {
  kty: String,
  crv: String,
  x: String,
  y: String,
  /// A private key in a proof is a broken client (RFC 9449 §4.3).
  #[serde(default)]
  d: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct ProofClaims {
  jti: String,
  htm: String,
  htu: String,
  iat: i64,
  #[serde(default)]
  ath: Option<String>,
  #[serde(default)]
  nonce: Option<String>,
}

impl DpopProof {
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl std::fmt::Debug for DpopProof {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("DpopProof([redacted])")
  }
}

impl DpopSigningKey {
  /// A fresh key from the system's random source.
  pub fn generate() -> Self {
    Self {
      key: SigningKey::random(&mut OsRng),
    }
  }

  /// From the raw 32-byte private scalar, for a platform that stores it in its key store.
  pub fn from_secret_bytes(secret: &[u8]) -> Result<Self, DpopError> {
    // `from_slice` would pad a short slice instead of refusing it.
    if secret.len() != COORDINATE_BYTES {
      return Err(DpopError::BadKey);
    }
    let key = SigningKey::from_slice(secret).map_err(|_| DpopError::BadKey)?;
    Ok(Self { key })
  }

  /// The raw private scalar, to put into the platform's key store.
  pub fn to_secret_bytes(&self) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(self.key.to_bytes().to_vec())
  }

  /// The thumbprint the identity provider binds tokens to.
  pub fn thumbprint(&self) -> JwkThumbprint {
    let (x, y) = self.public_coordinates();
    JwkThumbprint::of_coordinates(&x, &y)
  }

  /// Signs a proof.
  pub fn sign_proof(&self, request: &DpopProofRequest<'_>) -> Result<DpopProof, DpopError> {
    let mut target = Url::parse(request.target_uri).map_err(|_| DpopError::WrongUri)?;
    target.set_query(None);
    target.set_fragment(None);

    let (x, y) = self.public_coordinates();
    let header = serde_json::json!({
      "typ": PROOF_TYPE,
      "alg": DPOP_ALGORITHM,
      "jwk": {"kty": "EC", "crv": "P-256", "x": x, "y": y},
    });
    let mut claims = serde_json::json!({
      "jti": random_base64url::<JTI_RANDOM_BYTES>(),
      "htm": request.method,
      "htu": target.as_str(),
      "iat": request.issued_at_unix,
    });
    if let Some(token) = request.maybe_access_token {
      claims["ath"] = serde_json::Value::String(access_token_hash(token));
    }
    if let Some(nonce) = request.maybe_nonce {
      claims["nonce"] = serde_json::Value::String(nonce.to_owned());
    }
    let signing_input = format!(
      "{}.{}",
      URL_SAFE_NO_PAD.encode(header.to_string()),
      URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signature: Signature = self.key.sign(signing_input.as_bytes());
    Ok(DpopProof(format!(
      "{signing_input}.{}",
      URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )))
  }

  fn public_coordinates(&self) -> (String, String) {
    let point = self.key.verifying_key().to_encoded_point(false);
    // Invariant: an uncompressed point of a valid key is 0x04, x, y, of 32 bytes each.
    let (x, y) = point.as_bytes()[1..].split_at(COORDINATE_BYTES);
    (URL_SAFE_NO_PAD.encode(x), URL_SAFE_NO_PAD.encode(y))
  }
}

impl std::fmt::Debug for DpopSigningKey {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("DpopSigningKey([redacted])")
  }
}

impl JwkThumbprint {
  /// From a token's `cnf.jkt`: base64url of a SHA-256 hash (RFC 9449 §6.1).
  pub fn parse(jkt: &str) -> Result<Self, DpopError> {
    let is_a_hash = jkt.len() == HASH_CHARS
      && URL_SAFE_NO_PAD
        .decode(jkt)
        .is_ok_and(|bytes| bytes.len() == HASH_BYTES);
    if !is_a_hash {
      return Err(DpopError::BadKey);
    }
    Ok(Self(jkt.to_owned()))
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// RFC 7638 §3: the hash of the required members, in lexicographic order, without whitespace.
  /// The coordinates are known to be canonical base64url, so nothing needs escaping.
  fn of_coordinates(x: &str, y: &str) -> Self {
    let canonical = format!(r#"{{"crv":"P-256","kty":"EC","x":"{x}","y":"{y}"}}"#);
    Self(URL_SAFE_NO_PAD.encode(Sha256::digest(canonical.as_bytes())))
  }
}

impl PublicOrigin {
  /// From `http(s)://host[:port]`, with or without a trailing slash. A path, query or credentials
  /// are refused: an origin that isn't one is a configuration mistake to report, not to guess at.
  pub fn parse(origin: &str) -> Result<Self, DpopError> {
    let url = Url::parse(origin).map_err(|_| DpopError::WrongUri)?;
    let is_plain = matches!(url.scheme(), "http" | "https")
      && url.path() == "/"
      && url.query().is_none()
      && url.fragment().is_none()
      && url.username().is_empty()
      && url.password().is_none();
    if !is_plain {
      return Err(DpopError::WrongUri);
    }
    Ok(Self(url.origin()))
  }
}

impl DpopNonces {
  pub fn new(now_unix: i64) -> Self {
    Self {
      current: random_base64url::<NONCE_RANDOM_BYTES>(),
      current_since_unix: now_unix,
      maybe_previous: None,
    }
  }

  /// The nonce to hand out now.
  pub fn current(&mut self, now_unix: i64, rotation_secs: u64) -> &str {
    self.rotate(now_unix, rotation_secs);
    &self.current
  }

  /// Whether a proof's nonce is the current one or its predecessor.
  pub fn accepts(&mut self, nonce: &str, now_unix: i64, rotation_secs: u64) -> bool {
    self.rotate(now_unix, rotation_secs);
    is_same(nonce, &self.current)
      || self
        .maybe_previous
        .as_deref()
        .is_some_and(|previous| is_same(nonce, previous))
  }

  /// Rotates lazily, so an idle server has no timer for this. After a long idle time the old nonce
  /// is gone too: a nonce is accepted for at most two rotation periods.
  fn rotate(&mut self, now_unix: i64, rotation_secs: u64) {
    let rotation = i64::try_from(rotation_secs).unwrap_or(i64::MAX).max(1);
    let age = now_unix.saturating_sub(self.current_since_unix);
    if age < rotation {
      return;
    }
    let fresh = random_base64url::<NONCE_RANDOM_BYTES>();
    let previous = std::mem::replace(&mut self.current, fresh);
    self.maybe_previous = (age < rotation.saturating_mul(2)).then_some(previous);
    self.current_since_unix = now_unix;
  }
}

/// The `ath` of a token: base64url of the SHA-256 of its ASCII form (RFC 9449 §4.2).
pub fn access_token_hash(token: &str) -> String {
  URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

/// How long a replay cache has to remember a proof it accepted: until its `iat` window ended in the
/// worst case, a proof dated `leeway` ahead of the server's clock.
pub fn replay_retention_secs(max_age_secs: u64, leeway_secs: u64) -> u64 {
  max_age_secs
    .saturating_add(leeway_secs)
    .saturating_add(leeway_secs)
}

/// Runs the checks of RFC 9449 §4.3 that need no server state; the caller still has to check the
/// nonce and record [`VerifiedDpopProof::replay_id`]. Cheap checks come before the signature.
pub fn verify_proof(proof: &str, expected: &DpopExpectation<'_>) -> Result<VerifiedDpopProof, DpopError> {
  if proof.len() > MAX_PROOF_BYTES {
    return Err(DpopError::Oversized);
  }
  let (signing_input, signature) = proof.rsplit_once('.').ok_or(DpopError::Malformed)?;
  let (header, claims) = signing_input.split_once('.').ok_or(DpopError::Malformed)?;
  let header: ProofHeader = decode_json(header)?;
  let claims: ProofClaims = decode_json(claims)?;

  let verifying_key = check_header(&header)?;
  let key = JwkThumbprint::of_coordinates(&header.jwk.x, &header.jwk.y);
  if let Some(bound) = expected.maybe_bound_key
    && !is_same(bound.as_str(), key.as_str())
  {
    return Err(DpopError::WrongKey);
  }
  check_claims(&claims, expected)?;

  let signature = URL_SAFE_NO_PAD
    .decode(signature)
    .map_err(|_| DpopError::Malformed)?;
  let signature = Signature::from_slice(&signature).map_err(|_| DpopError::BadSignature)?;
  verifying_key
    .verify(signing_input.as_bytes(), &signature)
    .map_err(|_| DpopError::BadSignature)?;

  let replay_id = replay_id(&key, &claims.jti);
  Ok(VerifiedDpopProof {
    key,
    replay_id,
    maybe_nonce: claims.nonce,
  })
}

fn check_header(header: &ProofHeader) -> Result<VerifyingKey, DpopError> {
  if header.typ != PROOF_TYPE {
    return Err(DpopError::WrongType);
  }
  if header.alg != DPOP_ALGORITHM {
    return Err(DpopError::UnsupportedAlgorithm);
  }
  if header.crit.is_some() {
    return Err(DpopError::Malformed);
  }
  let jwk = &header.jwk;
  if jwk.kty != "EC" || jwk.crv != "P-256" || jwk.d.is_some() {
    return Err(DpopError::BadKey);
  }
  let x = decode_coordinate(&jwk.x)?;
  let y = decode_coordinate(&jwk.y)?;
  let mut point = Vec::with_capacity(1 + 2 * COORDINATE_BYTES);
  point.push(0x04);
  point.extend_from_slice(&x);
  point.extend_from_slice(&y);
  // Refuses points that are not on the curve.
  VerifyingKey::from_sec1_bytes(&point).map_err(|_| DpopError::BadKey)
}

fn check_claims(claims: &ProofClaims, expected: &DpopExpectation<'_>) -> Result<(), DpopError> {
  if claims.jti.is_empty() || claims.jti.len() > MAX_JTI_BYTES {
    return Err(DpopError::BadJti);
  }
  if claims
    .nonce
    .as_deref()
    .is_some_and(|nonce| nonce.len() > MAX_NONCE_BYTES)
  {
    return Err(DpopError::Malformed);
  }
  if claims.htm != expected.method {
    return Err(DpopError::WrongMethod);
  }
  check_target(&claims.htu, expected)?;
  check_issued_at(claims.iat, expected)?;
  match claims.ath.as_deref() {
    Some(ath) if is_same(ath, &access_token_hash(expected.access_token)) => Ok(()),
    _ => Err(DpopError::WrongAccessToken),
  }
}

/// `htu` against the server's own origins and the path the request came in on. `Url` normalizes
/// both (case of scheme and host, default ports, dot segments) as RFC 9449 §4.3 allows; a query and
/// a fragment are ignored as it says.
fn check_target(htu: &str, expected: &DpopExpectation<'_>) -> Result<(), DpopError> {
  let target = Url::parse(htu).map_err(|_| DpopError::WrongUri)?;
  if !target.username().is_empty() || target.password().is_some() {
    return Err(DpopError::WrongUri);
  }
  let origin = target.origin();
  if !expected.public_origins.iter().any(|known| known.0 == origin) {
    return Err(DpopError::WrongUri);
  }
  let mut request = target.clone();
  request.set_path(expected.path);
  if request.path() != target.path() {
    return Err(DpopError::WrongUri);
  }
  Ok(())
}

/// A proof counts from `leeway` before its `iat` until `max_age + leeway` after it. A replay cache
/// keeps entries for [`replay_retention_secs`], which is that whole window.
fn check_issued_at(iat: i64, expected: &DpopExpectation<'_>) -> Result<(), DpopError> {
  let leeway = i64::try_from(expected.leeway_secs).unwrap_or(i64::MAX);
  let max_age = i64::try_from(expected.max_age_secs).unwrap_or(i64::MAX);
  let is_from_the_future = iat > expected.now_unix.saturating_add(leeway);
  let is_too_old = expected.now_unix.saturating_sub(iat) > max_age.saturating_add(leeway);
  if is_from_the_future || is_too_old {
    return Err(DpopError::NotFresh);
  }
  Ok(())
}

fn replay_id(key: &JwkThumbprint, jti: &str) -> [u8; HASH_BYTES] {
  let mut hash = Sha256::new();
  hash.update(key.as_str().as_bytes());
  hash.update(b".");
  hash.update(jti.as_bytes());
  hash.finalize().into()
}

fn decode_json<T: serde::de::DeserializeOwned>(part: &str) -> Result<T, DpopError> {
  let bytes = URL_SAFE_NO_PAD.decode(part).map_err(|_| DpopError::Malformed)?;
  serde_json::from_slice(&bytes).map_err(|_| DpopError::Malformed)
}

/// A coordinate of exactly 32 bytes. The base64url engine refuses padding and stray trailing bits,
/// so one key has one encoding and with it one thumbprint.
fn decode_coordinate(coordinate: &str) -> Result<Vec<u8>, DpopError> {
  let bytes = URL_SAFE_NO_PAD
    .decode(coordinate)
    .map_err(|_| DpopError::BadKey)?;
  if bytes.len() != COORDINATE_BYTES {
    return Err(DpopError::BadKey);
  }
  Ok(bytes)
}

fn random_base64url<const BYTES: usize>() -> String {
  let mut bytes = [0_u8; BYTES];
  OsRng.fill_bytes(&mut bytes);
  URL_SAFE_NO_PAD.encode(bytes)
}

fn is_same(left: &str, right: &str) -> bool {
  bool::from(left.as_bytes().ct_eq(right.as_bytes()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use serde_json::{Value, json};

  use super::*;

  const NOW: i64 = 1_800_000_000;
  const TOKEN: &str = "an.access.token";
  const ORIGIN: &str = "https://chat.example.com";
  const PATH: &str = "/api/v1/me";
  const URI: &str = "https://chat.example.com/api/v1/me";
  const MAX_AGE: u64 = 60;
  const LEEWAY: u64 = 30;
  /// The example of RFC 9449 §7.1 (Figures 13 and 14), reassembled from its line wrapping.
  const RFC_PROOF: &str = concat!(
    "eyJ0eXAiOiJkcG9wK2p3dCIsImFsZyI6IkVTMjU2IiwiandrIjp7Imt0eSI6Ik",
    "VDIiwieCI6Imw4dEZyaHgtMzR0VjNoUklDUkRZOXpDa0RscEJoRjQyVVFVZldWQVdCR",
    "nMiLCJ5IjoiOVZFNGpmX09rX282NHpiVFRsY3VOSmFqSG10NnY5VERWclUwQ2R2R1JE",
    "QSIsImNydiI6IlAtMjU2In19.eyJqdGkiOiJlMWozVl9iS2ljOC1MQUVCIiwiaHRtIj",
    "oiR0VUIiwiaHR1IjoiaHR0cHM6Ly9yZXNvdXJjZS5leGFtcGxlLm9yZy9wcm90ZWN0Z",
    "WRyZXNvdXJjZSIsImlhdCI6MTU2MjI2MjYxOCwiYXRoIjoiZlVIeU8ycjJaM0RaNTNF",
    "c05yV0JiMHhXWG9hTnk1OUlpS0NBcWtzbVFFbyJ9.2oW9RP35yRqzhrtNP86L-Ey71E",
    "OptxRimPPToA1plemAgR6pxHF8y6-yqyVnmcw6Fy1dqd-jfxSYoMxhAJpLjA",
  );
  const RFC_TOKEN: &str = "Kz~8mXK1EalYznwH-LC-1fBAo.4Ljp~zsPE_NeO.gxU";
  const RFC_ATH: &str = "fUHyO2r2Z3DZ53EsNrWBb0xWXoaNy59IiKCAqksmQEo";
  const RFC_JKT: &str = "0ZcOCORZNYy-DWpqq30jZyJGHTN0d2HglBV3uiguA4I";
  const RFC_IAT: i64 = 1_562_262_618;

  #[test]
  fn the_rfc_example_verifies_with_its_thumbprint_and_token_hash() {
    assert_eq!(access_token_hash(RFC_TOKEN), RFC_ATH);
    let origins = [PublicOrigin::parse("https://resource.example.org").unwrap()];
    let bound = JwkThumbprint::parse(RFC_JKT).unwrap();
    let verified = verify_proof(
      RFC_PROOF,
      &DpopExpectation {
        method: "GET",
        path: "/protectedresource",
        public_origins: &origins,
        now_unix: RFC_IAT,
        max_age_secs: MAX_AGE,
        leeway_secs: LEEWAY,
        access_token: RFC_TOKEN,
        maybe_bound_key: Some(&bound),
      },
    )
    .unwrap();
    assert_eq!(verified.key, bound);
    assert_eq!(verified.maybe_nonce, None);
  }

  mod round_trip {
    use super::*;

    #[test]
    fn a_signed_proof_verifies_and_keeps_its_nonce() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), Some("server-nonce"));
      let verified = check(&proof, &bound_to(&key)).unwrap();
      assert_eq!(verified.key, key.thumbprint());
      assert_eq!(verified.maybe_nonce.as_deref(), Some("server-nonce"));
    }

    #[test]
    fn a_stored_key_makes_the_same_thumbprint() {
      let key = DpopSigningKey::generate();
      let again = DpopSigningKey::from_secret_bytes(&key.to_secret_bytes()).unwrap();
      assert_eq!(again.thumbprint(), key.thumbprint());
      assert!(DpopSigningKey::from_secret_bytes(&[0; 32]).is_err());
      assert!(DpopSigningKey::from_secret_bytes(&[1; 31]).is_err());
    }

    #[test]
    fn a_query_and_fragment_stay_out_of_the_proof() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", &format!("{URI}?limit=1#top"), NOW, Some(TOKEN), None);
      assert_eq!(claims_of(&proof)["htu"], json!(URI));
      assert!(check(&proof, &bound_to(&key)).is_ok());
    }

    #[test]
    fn two_proofs_never_share_a_replay_id() {
      let key = DpopSigningKey::generate();
      let first = check(&sign(&key, "GET", URI, NOW, Some(TOKEN), None), &bound_to(&key)).unwrap();
      let second = check(&sign(&key, "GET", URI, NOW, Some(TOKEN), None), &bound_to(&key)).unwrap();
      assert_ne!(first.replay_id, second.replay_id);
    }

    #[test]
    fn the_same_jti_of_another_key_is_another_replay_id() {
      let first = DpopSigningKey::generate();
      let second = DpopSigningKey::generate();
      assert_ne!(
        replay_id(&first.thumbprint(), "jti"),
        replay_id(&second.thumbprint(), "jti")
      );
    }
  }

  mod refusals {
    use super::*;

    #[test]
    fn wrong_method() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "POST", URI, NOW, Some(TOKEN), None);
      assert_eq!(check(&proof, &bound_to(&key)), Err(DpopError::WrongMethod));
      let lower = sign(&key, "get", URI, NOW, Some(TOKEN), None);
      assert_eq!(check(&lower, &bound_to(&key)), Err(DpopError::WrongMethod));
    }

    #[test]
    fn a_look_alike_origin_or_scheme_or_port() {
      let key = DpopSigningKey::generate();
      for uri in [
        "https://chat.example.com.evil.example/api/v1/me",
        "https://lookalike.example.net/api/v1/me",
        "http://chat.example.com/api/v1/me",
        "https://chat.example.com:8443/api/v1/me",
      ] {
        let proof = sign(&key, "GET", uri, NOW, Some(TOKEN), None);
        assert_eq!(check(&proof, &bound_to(&key)), Err(DpopError::WrongUri), "{uri}");
      }
    }

    #[test]
    fn another_path_or_credentials_in_the_url() {
      let key = DpopSigningKey::generate();
      for uri in [
        "https://chat.example.com/api/v1/spaces",
        "https://chat.example.com/api/v1/me/",
        "https://user:secret@chat.example.com/api/v1/me",
      ] {
        let proof = sign(&key, "GET", uri, NOW, Some(TOKEN), None);
        assert_eq!(check(&proof, &bound_to(&key)), Err(DpopError::WrongUri), "{uri}");
      }
    }

    #[test]
    fn a_proof_for_another_token_or_none() {
      let key = DpopSigningKey::generate();
      let other = sign(&key, "GET", URI, NOW, Some("another.token"), None);
      assert_eq!(check(&other, &bound_to(&key)), Err(DpopError::WrongAccessToken));
      let none = sign(&key, "GET", URI, NOW, None, None);
      assert_eq!(check(&none, &bound_to(&key)), Err(DpopError::WrongAccessToken));
    }

    #[test]
    fn a_key_the_token_is_not_bound_to() {
      let key = DpopSigningKey::generate();
      let thief = DpopSigningKey::generate();
      let proof = sign(&thief, "GET", URI, NOW, Some(TOKEN), None);
      assert_eq!(check(&proof, &bound_to(&key)), Err(DpopError::WrongKey));
    }

    #[test]
    fn a_signature_by_another_key_or_tampered_content() {
      let key = DpopSigningKey::generate();
      let other = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      let signature = proof.rsplit_once('.').unwrap().1;
      let foreign = sign(&other, "GET", URI, NOW, Some(TOKEN), None);
      let (foreign_input, _) = foreign.rsplit_once('.').unwrap();
      let forged = format!("{foreign_input}.{signature}");
      assert_eq!(check(&forged, &unbound()), Err(DpopError::BadSignature));

      let mut claims = claims_of(&proof);
      claims["jti"] = json!("another");
      assert_eq!(
        check(&with_claims(&proof, &claims), &unbound()),
        Err(DpopError::BadSignature)
      );
    }

    #[test]
    fn typ_alg_and_extensions_that_are_not_allowed() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      for (change, error) in [
        (json!({"typ": "JWT"}), DpopError::WrongType),
        (json!({"typ": "DPOP+JWT"}), DpopError::WrongType),
        (json!({"alg": "none"}), DpopError::UnsupportedAlgorithm),
        (json!({"alg": "HS256"}), DpopError::UnsupportedAlgorithm),
        (json!({"alg": "ES384"}), DpopError::UnsupportedAlgorithm),
        (json!({"crit": ["exp"]}), DpopError::Malformed),
      ] {
        assert_eq!(
          check(&with_header(&proof, &change), &unbound()),
          Err(error),
          "{change}"
        );
      }
    }

    #[test]
    fn keys_that_are_not_usable() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      for (name, value) in [
        ("kty", json!("RSA")),
        ("crv", json!("P-384")),
        ("d", json!("c2VjcmV0")),
        ("x", json!("AAAA")),
        ("y", json!("AQ")),
        // On no curve.
        ("x", json!(URL_SAFE_NO_PAD.encode([7_u8; 32]))),
      ] {
        let mut jwk = header_of(&proof)["jwk"].clone();
        jwk[name] = value;
        let changed = with_header(&proof, &json!({"jwk": jwk}));
        assert_eq!(check(&changed, &unbound()), Err(DpopError::BadKey), "{name}");
      }
    }

    #[test]
    fn a_jti_that_is_empty_or_too_long() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      for jti in [String::new(), "j".repeat(MAX_JTI_BYTES + 1)] {
        let mut claims = claims_of(&proof);
        claims["jti"] = json!(jti);
        assert_eq!(
          check(&with_claims(&proof, &claims), &unbound()),
          Err(DpopError::BadJti)
        );
      }
    }

    #[test]
    fn a_nonce_that_is_too_long() {
      let key = DpopSigningKey::generate();
      let nonce = "n".repeat(MAX_NONCE_BYTES + 1);
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), Some(&nonce));
      assert_eq!(check(&proof, &unbound()), Err(DpopError::Malformed));
    }
  }

  mod clock {
    use super::*;

    #[test]
    fn the_window_is_exact_at_both_ends() {
      let key = DpopSigningKey::generate();
      let leeway = i64::try_from(LEEWAY).unwrap();
      let max_age = i64::try_from(MAX_AGE).unwrap();
      for (iat, is_fresh) in [
        (NOW, true),
        (NOW + leeway, true),
        (NOW + leeway + 1, false),
        (NOW - max_age - leeway, true),
        (NOW - max_age - leeway - 1, false),
        (i64::MAX, false),
        (i64::MIN, false),
        (0, false),
      ] {
        let proof = sign(&key, "GET", URI, iat, Some(TOKEN), None);
        let result = check(&proof, &unbound());
        if is_fresh {
          assert!(result.is_ok(), "iat {iat}");
        } else {
          assert_eq!(result, Err(DpopError::NotFresh), "iat {iat}");
        }
      }
    }

    #[test]
    fn a_fractional_iat_is_refused() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      let mut claims = claims_of(&proof);
      claims["iat"] = json!(1.5);
      assert_eq!(
        check(&with_claims(&proof, &claims), &unbound()),
        Err(DpopError::Malformed)
      );
    }

    #[test]
    fn the_replay_retention_covers_the_whole_window() {
      // Accepted at `t` with `iat = t + leeway`: still fresh until `iat + max_age + leeway`.
      assert_eq!(replay_retention_secs(MAX_AGE, LEEWAY), MAX_AGE + 2 * LEEWAY);
      assert_eq!(replay_retention_secs(u64::MAX, 1), u64::MAX);
    }
  }

  mod hostile_input {
    use super::*;

    #[test]
    fn oversized_proofs_are_refused_before_parsing() {
      assert_eq!(
        check(&"a".repeat(MAX_PROOF_BYTES + 1), &unbound()),
        Err(DpopError::Oversized)
      );
    }

    #[test]
    fn malformed_proofs_are_refused() {
      let key = DpopSigningKey::generate();
      let valid = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      let (input, _) = valid.rsplit_once('.').unwrap();
      for proof in [
        String::new(),
        ".".to_owned(),
        "..".to_owned(),
        "a.b".to_owned(),
        "a.b.c".to_owned(),
        "!!!.!!!.!!!".to_owned(),
        format!("{input}."),
        format!("{input}.!!!"),
        format!("{}.{}.sig", encode(&json!([])), encode(&json!({}))),
        format!("{}.{}.sig", encode(&json!(null)), encode(&json!(1))),
        format!("{valid}.extra"),
      ] {
        assert!(check(&proof, &unbound()).is_err(), "{proof}");
      }
    }

    #[test]
    fn a_duplicate_claim_is_refused() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      let (header, rest) = proof.split_once('.').unwrap();
      let (_, signature) = rest.split_once('.').unwrap();
      let raw = format!(
        r#"{{"jti":"a","jti":"b","htm":"GET","htu":"{URI}","iat":{NOW},"ath":"{}"}}"#,
        access_token_hash(TOKEN)
      );
      let duplicated = format!("{header}.{}.{signature}", URL_SAFE_NO_PAD.encode(raw));
      assert_eq!(check(&duplicated, &unbound()), Err(DpopError::Malformed));
    }

    #[test]
    fn padded_base64_is_refused() {
      let key = DpopSigningKey::generate();
      let proof = sign(&key, "GET", URI, NOW, Some(TOKEN), None);
      assert_eq!(check(&format!("{proof}="), &unbound()), Err(DpopError::Malformed));
    }

    #[test]
    fn a_thumbprint_must_look_like_one() {
      assert!(JwkThumbprint::parse(RFC_JKT).is_ok());
      for jkt in [
        "",
        "short",
        "0ZcOCORZNYy-DWpqq30jZyJGHTN0d2HglBV3uiguA4",
        "0ZcOCORZNYy+DWpqq30jZyJGHTN0d2HglBV3uiguA4I",
        "0ZcOCORZNYy-DWpqq30jZyJGHTN0d2HglBV3uiguA4Ia",
      ] {
        assert!(JwkThumbprint::parse(jkt).is_err(), "{jkt}");
      }
    }
  }

  mod origins {
    use super::*;

    #[test]
    fn urls_are_compared_after_normalizing() {
      let key = DpopSigningKey::generate();
      for uri in [
        "HTTPS://CHAT.EXAMPLE.COM/api/v1/me",
        "https://chat.example.com:443/api/v1/me",
        "https://chat.example.com/api/v1/x/../me",
      ] {
        let proof = sign(&key, "GET", uri, NOW, Some(TOKEN), None);
        assert!(check(&proof, &unbound()).is_ok(), "{uri}");
      }
    }

    #[test]
    fn a_configured_origin_has_no_path_and_no_credentials() {
      assert!(PublicOrigin::parse("https://chat.example.com").is_ok());
      assert!(PublicOrigin::parse("https://chat.example.com/").is_ok());
      assert!(PublicOrigin::parse("http://localhost:8080").is_ok());
      assert!(PublicOrigin::parse("https://[2001:db8::1]:8443").is_ok());
      for origin in [
        "chat.example.com",
        "https://chat.example.com/app",
        "https://user@chat.example.com",
        "https://chat.example.com?x=1",
        "ftp://chat.example.com",
        "",
      ] {
        assert!(PublicOrigin::parse(origin).is_err(), "{origin}");
      }
    }
  }

  mod nonces {
    use super::*;

    const ROTATION: u64 = 300;
    const ROTATION_I64: i64 = 300;

    #[test]
    fn the_current_and_the_previous_nonce_count() {
      let mut nonces = DpopNonces::new(NOW);
      let first = nonces.current(NOW, ROTATION).to_owned();
      assert!(nonces.accepts(&first, NOW + 1, ROTATION));
      assert!(!nonces.accepts("guess", NOW + 1, ROTATION));

      let second = nonces.current(NOW + ROTATION_I64, ROTATION).to_owned();
      assert_ne!(first, second);
      assert!(nonces.accepts(&first, NOW + ROTATION_I64, ROTATION));
      assert!(nonces.accepts(&second, NOW + ROTATION_I64, ROTATION));

      let third = nonces.current(NOW + 2 * ROTATION_I64, ROTATION).to_owned();
      assert!(!nonces.accepts(&first, NOW + 2 * ROTATION_I64, ROTATION));
      assert!(nonces.accepts(&second, NOW + 2 * ROTATION_I64, ROTATION));
      assert!(nonces.accepts(&third, NOW + 2 * ROTATION_I64, ROTATION));
    }

    #[test]
    fn after_a_long_idle_time_the_old_nonce_is_gone() {
      let mut nonces = DpopNonces::new(NOW);
      let old = nonces.current(NOW, ROTATION).to_owned();
      assert!(!nonces.accepts(&old, NOW + 2 * ROTATION_I64, ROTATION));
    }

    #[test]
    fn a_restart_invalidates_every_nonce() {
      let mut before = DpopNonces::new(NOW);
      let nonce = before.current(NOW, ROTATION).to_owned();
      let mut after = DpopNonces::new(NOW);
      assert!(!after.accepts(&nonce, NOW, ROTATION));
    }

    #[test]
    fn a_clock_that_steps_back_keeps_the_nonce() {
      let mut nonces = DpopNonces::new(NOW);
      let nonce = nonces.current(NOW, ROTATION).to_owned();
      assert!(nonces.accepts(&nonce, NOW - 1000, ROTATION));
    }
  }

  mod secrets {
    use super::*;

    #[test]
    fn debug_shows_neither_the_key_nor_the_proof() {
      let key = DpopSigningKey::generate();
      assert_eq!(format!("{key:?}"), "DpopSigningKey([redacted])");
      let proof = key
        .sign_proof(&DpopProofRequest {
          method: "GET",
          target_uri: URI,
          issued_at_unix: NOW,
          maybe_access_token: Some(TOKEN),
          maybe_nonce: None,
        })
        .unwrap();
      assert_eq!(format!("{proof:?}"), "DpopProof([redacted])");
    }

    #[test]
    fn the_private_key_is_wiped_on_drop() {
      fn assert_zeroizes<T: zeroize::ZeroizeOnDrop>() {}
      assert_zeroizes::<SigningKey>();
    }
  }

  fn check(proof: &str, maybe_bound_key: &Option<JwkThumbprint>) -> Result<VerifiedDpopProof, DpopError> {
    let origins = [PublicOrigin::parse(ORIGIN).unwrap()];
    verify_proof(
      proof,
      &DpopExpectation {
        method: "GET",
        path: PATH,
        public_origins: &origins,
        now_unix: NOW,
        max_age_secs: MAX_AGE,
        leeway_secs: LEEWAY,
        access_token: TOKEN,
        maybe_bound_key: maybe_bound_key.as_ref(),
      },
    )
  }

  fn bound_to(key: &DpopSigningKey) -> Option<JwkThumbprint> {
    Some(key.thumbprint())
  }

  fn unbound() -> Option<JwkThumbprint> {
    None
  }

  fn sign(
    key: &DpopSigningKey,
    method: &str,
    uri: &str,
    iat: i64,
    maybe_token: Option<&str>,
    maybe_nonce: Option<&str>,
  ) -> String {
    key
      .sign_proof(&DpopProofRequest {
        method,
        target_uri: uri,
        issued_at_unix: iat,
        maybe_access_token: maybe_token,
        maybe_nonce,
      })
      .unwrap()
      .as_str()
      .to_owned()
  }

  fn encode(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
  }

  fn decode(part: &str) -> Value {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
  }

  fn header_of(proof: &str) -> Value {
    decode(proof.split('.').next().unwrap())
  }

  fn claims_of(proof: &str) -> Value {
    decode(proof.split('.').nth(1).unwrap())
  }

  /// The proof with members of its header replaced; its signature no longer fits, which these
  /// tests don't reach.
  fn with_header(proof: &str, change: &Value) -> String {
    let mut header = header_of(proof);
    for (name, value) in change.as_object().unwrap() {
      header[name] = value.clone();
    }
    let (_, rest) = proof.split_once('.').unwrap();
    format!("{}.{rest}", encode(&header))
  }

  /// The proof with other claims; its signature no longer fits.
  fn with_claims(proof: &str, claims: &Value) -> String {
    let (header, rest) = proof.split_once('.').unwrap();
    let (_, signature) = rest.split_once('.').unwrap();
    format!("{header}.{}.{signature}", encode(claims))
  }
}
