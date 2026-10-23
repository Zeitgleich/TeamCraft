//! Server authentication towards a push gateway (push-gateway.md §3).
//!
//! A TeamCraft server owns an Ed25519 key pair for the gateway. The public key identifies the
//! server (the gateway derives an opaque [`server_id`](GatewayPublicKey::server_id) from it), and
//! every request carries a signature over the method, path, timestamp and the SHA-256 of the body,
//! so the gateway can attribute quotas and block a server without accounts or shared secrets.
//!
//! The key authenticates a server to the gateway only. It protects no content: wake-ups are sealed
//! with the device's push key before they reach the gateway (e2ee.md §7).

use std::fmt;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use hpke_rs::{Hpke, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_crypto::types::{AeadAlgorithm, KdfAlgorithm, KemAlgorithm};
use hpke_rs_rust_crypto::HpkeRustCrypto;
use rand_core::CryptoRngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::domain;
use crate::secret::SymmetricKey;

/// Length of an Ed25519 signature.
pub const GATEWAY_SIGNATURE_LEN: usize = 64;
/// Bytes of the key digest that make up the server ID (128 bit).
const SERVER_ID_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum GatewayAuthError {
  #[error("sealing the device token failed")]
  SealFailed,
  #[error("the sealed device token is malformed or not sealed to this key")]
  OpenFailed,
  #[error("the gateway public key is not a valid Ed25519 point")]
  InvalidPublicKey,
  #[error("the request signature is invalid")]
  InvalidSignature,
}

/// The parts of an HTTP request a signature covers.
#[derive(Debug, Clone, Copy)]
pub struct GatewayRequest<'a> {
  /// Upper-case HTTP method.
  pub method: &'a str,
  /// Path without query, e.g. `/api/v1/push`.
  pub path: &'a str,
  /// Sender clock, milliseconds since the Unix epoch; the gateway rejects stale ones.
  pub timestamp_ms: u64,
  pub body: &'a [u8],
}

/// A server's private key for the gateway. Zeroized on drop, never printed.
pub struct GatewayServerKey {
  signing_key: SigningKey,
}

/// The public half; this is what a server registers with a gateway.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GatewayPublicKey([u8; 32]);

impl GatewayServerKey {
  pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
    Self {
      signing_key: SigningKey::generate(rng),
    }
  }

  /// Restores the key from its secret seed.
  pub fn from_secret(secret: &SymmetricKey) -> Self {
    Self {
      signing_key: SigningKey::from_bytes(secret.expose_secret()),
    }
  }

  /// The secret seed, for storing in the server's secrets.
  pub fn to_secret(&self) -> SymmetricKey {
    SymmetricKey::from_bytes(self.signing_key.to_bytes())
  }

  pub fn public_key(&self) -> GatewayPublicKey {
    GatewayPublicKey(self.signing_key.verifying_key().to_bytes())
  }

  pub fn sign(&self, request: &GatewayRequest<'_>) -> [u8; GATEWAY_SIGNATURE_LEN] {
    self.signing_key.sign(&signing_input(request)).to_bytes()
  }
}

impl fmt::Debug for GatewayServerKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("GatewayServerKey([redacted])")
  }
}

impl GatewayPublicKey {
  pub fn from_bytes(bytes: [u8; 32]) -> Self {
    Self(bytes)
  }

  pub fn as_bytes(&self) -> &[u8; 32] {
    &self.0
  }

  /// The opaque ID the gateway knows the server by: 128 bits of a domain-separated digest of the
  /// public key, lower-case hex. It reveals nothing about the server or its users.
  pub fn server_id(&self) -> String {
    let digest = Sha256::digest(domain::labeled(domain::PUSH_GATEWAY_SERVER_ID, &self.0));
    digest[..SERVER_ID_BYTES]
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect()
  }

  pub fn verify(&self, request: &GatewayRequest<'_>, signature: &[u8]) -> Result<(), GatewayAuthError> {
    let key = VerifyingKey::from_bytes(&self.0).map_err(|_| GatewayAuthError::InvalidPublicKey)?;
    let signature =
      ed25519_dalek::Signature::from_slice(signature).map_err(|_| GatewayAuthError::InvalidSignature)?;
    key
      .verify_strict(&signing_input(request), &signature)
      .map_err(|_| GatewayAuthError::InvalidSignature)
  }
}

impl ConstantTimeEq for GatewayPublicKey {
  fn ct_eq(&self, other: &Self) -> subtle::Choice {
    self.0.ct_eq(&other.0)
  }
}

impl fmt::Debug for GatewayPublicKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "GatewayPublicKey({})", self.server_id())
  }
}

/// Bytes of the key ID of a gateway token key.
pub const TOKEN_KEY_ID_LEN: usize = 8;
const TOKEN_KEY_LEN: usize = 32;
const BOUND_TOKEN_VERSION: u8 = 1;
const MAX_BOUND_TOKEN_BYTES: usize = 4096;

/// What an app seals for a gateway: the device token, bound to the one server that may push to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundToken {
  #[serde(rename = "v")]
  pub version: u8,
  /// `apns` or `fcm`.
  pub provider: String,
  /// The server's ID at the gateway ([`GatewayPublicKey::server_id`]).
  pub server_id: String,
  /// After this the gateway refuses the token (milliseconds since the Unix epoch).
  pub expires_at_ms: u64,
  pub token: String,
}

/// The ID of a gateway token key: the first 8 bytes of the SHA-256 of its public key.
pub fn token_key_id(public_key: &[u8; TOKEN_KEY_LEN]) -> [u8; TOKEN_KEY_ID_LEN] {
  let digest = Sha256::digest(public_key);
  let mut id = [0u8; TOKEN_KEY_ID_LEN];
  id.copy_from_slice(&digest[..TOKEN_KEY_ID_LEN]);
  id
}

/// Seals a device token for a gateway (push-gateway docs/api.md §6): HPKE base mode, DHKEM(X25519),
/// HKDF-SHA256, ChaCha20-Poly1305, `info` naming the key. The result is `key_id || encapsulated key ||
/// ciphertext`; only the gateway can open it, and only for `server_id`.
pub fn seal_bound_token(
  gateway_public_key: &[u8; TOKEN_KEY_LEN],
  provider: &str,
  server_id: &str,
  expires_at_ms: u64,
  token: &str,
) -> Result<Vec<u8>, GatewayAuthError> {
  let id = token_key_id(gateway_public_key);
  let plaintext = zeroize::Zeroizing::new(
    serde_json::to_vec(&BoundToken {
      version: BOUND_TOKEN_VERSION,
      provider: provider.to_owned(),
      server_id: server_id.to_owned(),
      expires_at_ms,
      token: token.to_owned(),
    })
    .map_err(|_| GatewayAuthError::SealFailed)?,
  );
  let (encapsulated, ciphertext) = hpke()
    .seal(
      &HpkePublicKey::new(gateway_public_key.to_vec()),
      &token_info(&id),
      &[],
      &plaintext,
      None,
      None,
      None,
    )
    .map_err(|_| GatewayAuthError::SealFailed)?;
  let mut blob = Vec::with_capacity(TOKEN_KEY_ID_LEN + encapsulated.len() + ciphertext.len());
  blob.extend_from_slice(&id);
  blob.extend_from_slice(&encapsulated);
  blob.extend_from_slice(&ciphertext);
  Ok(blob)
}

/// Opens a sealed token with the gateway's token key seed. This is the gateway's side; it is here so
/// both ends of the format are tested against each other and against the shared known answer.
pub fn open_bound_token(seed: &[u8; TOKEN_KEY_LEN], blob: &[u8]) -> Result<BoundToken, GatewayAuthError> {
  if blob.len() <= TOKEN_KEY_ID_LEN + TOKEN_KEY_LEN || blob.len() > MAX_BOUND_TOKEN_BYTES {
    return Err(GatewayAuthError::OpenFailed);
  }
  let (id, sealed) = blob.split_at(TOKEN_KEY_ID_LEN);
  let (encapsulated, ciphertext) = sealed.split_at(TOKEN_KEY_LEN);
  let (private, public) = hpke()
    .derive_key_pair(seed)
    .map_err(|_| GatewayAuthError::OpenFailed)?
    .into_keys();
  let public =
    <[u8; TOKEN_KEY_LEN]>::try_from(public.as_slice()).map_err(|_| GatewayAuthError::OpenFailed)?;
  if token_key_id(&public) != id {
    return Err(GatewayAuthError::OpenFailed);
  }
  let plaintext = zeroize::Zeroizing::new(
    hpke()
      .open(
        encapsulated,
        &HpkePrivateKey::new(private.as_slice().to_vec()),
        &token_info(&token_key_id(&public)),
        &[],
        ciphertext,
        None,
        None,
        None,
      )
      .map_err(|_| GatewayAuthError::OpenFailed)?,
  );
  let token: BoundToken = serde_json::from_slice(&plaintext).map_err(|_| GatewayAuthError::OpenFailed)?;
  (token.version == BOUND_TOKEN_VERSION)
    .then_some(token)
    .ok_or(GatewayAuthError::OpenFailed)
}

/// The public key of a gateway token key made from its seed (the gateway's side: tests and tools).
pub fn token_public_key(seed: &[u8; TOKEN_KEY_LEN]) -> Result<[u8; TOKEN_KEY_LEN], GatewayAuthError> {
  let (_, public) = hpke()
    .derive_key_pair(seed)
    .map_err(|_| GatewayAuthError::OpenFailed)?
    .into_keys();
  <[u8; TOKEN_KEY_LEN]>::try_from(public.as_slice()).map_err(|_| GatewayAuthError::OpenFailed)
}

fn token_info(key_id: &[u8; TOKEN_KEY_ID_LEN]) -> Vec<u8> {
  domain::labeled(domain::PUSH_GATEWAY_BOUND_TOKEN, key_id)
}

fn hpke() -> Hpke<HpkeRustCrypto> {
  Hpke::new(
    Mode::Base,
    KemAlgorithm::DhKem25519,
    KdfAlgorithm::HkdfSha256,
    AeadAlgorithm::ChaCha20Poly1305,
  )
}

/// `method \n path \n timestamp \n sha256(body)`, under the domain label. The newline cannot occur
/// in a method or path, so fields don't run into each other.
fn signing_input(request: &GatewayRequest<'_>) -> Vec<u8> {
  let mut body = Vec::new();
  body.extend_from_slice(request.method.as_bytes());
  body.push(b'\n');
  body.extend_from_slice(request.path.as_bytes());
  body.push(b'\n');
  body.extend_from_slice(request.timestamp_ms.to_string().as_bytes());
  body.push(b'\n');
  body.extend_from_slice(&Sha256::digest(request.body));
  domain::labeled(domain::PUSH_GATEWAY_REQUEST, &body)
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const BODY: &[u8] = br#"{"token":"abc"}"#;
  // Known answer shared with the gateway repository (docs/api.md §2): the same file is checked there.
  const KNOWN_ANSWER: &str = include_str!("../../teamcraft-proto/tests/fixtures/push_gateway/signature.json");

  const BOUND_KNOWN_ANSWER: &str =
    include_str!("../../teamcraft-proto/tests/fixtures/push_gateway/bound_token.json");

  #[test]
  fn the_gateways_known_sealed_token_opens_and_ours_open_the_same_way() {
    use base64::Engine as _;
    let fixture: serde_json::Value = serde_json::from_str(BOUND_KNOWN_ANSWER).unwrap();
    let text = |name: &str| fixture[name].as_str().unwrap().to_owned();
    let seed: [u8; 32] = [0x09; 32];
    let blob = base64::engine::general_purpose::STANDARD
      .decode(text("blob"))
      .unwrap();
    let opened = open_bound_token(&seed, &blob).unwrap();
    assert_eq!(serde_json::to_value(&opened).unwrap(), fixture["plaintext"]);
    // The blob that this code sealed (the gateway's tests open it too).
    let ours = base64::engine::general_purpose::STANDARD
      .decode(text("blob_teamcraft"))
      .unwrap();
    assert_eq!(
      serde_json::to_value(open_bound_token(&seed, &ours).unwrap()).unwrap(),
      fixture["plaintext"]
    );

    // Our own sealing to the same key opens with the same code, and with a distinct blob each time.
    let public: [u8; 32] = base64::engine::general_purpose::STANDARD
      .decode(text("public_key"))
      .unwrap()
      .try_into()
      .unwrap();
    assert_eq!(
      token_key_id(&public)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>(),
      text("key_id")
    );
    let ours = seal_bound_token(&public, "fcm", "aa", 99, "t").unwrap();
    assert_eq!(
      open_bound_token(&seed, &ours).unwrap(),
      BoundToken {
        version: 1,
        provider: "fcm".into(),
        server_id: "aa".into(),
        expires_at_ms: 99,
        token: "t".into(),
      }
    );
    assert_ne!(ours, seal_bound_token(&public, "fcm", "aa", 99, "t").unwrap());
  }

  #[test]
  fn a_sealed_token_does_not_open_when_changed_or_for_another_key() {
    let seed = [0x09u8; 32];
    let (_, public) = hpke().derive_key_pair(&seed).unwrap().into_keys();
    let public: [u8; 32] = public.as_slice().try_into().unwrap();
    let blob = seal_bound_token(&public, "apns", "aa", 1, "token").unwrap();
    for index in [0, 9, 41, blob.len() - 1] {
      let mut changed = blob.clone();
      changed[index] ^= 1;
      assert!(open_bound_token(&seed, &changed).is_err(), "byte {index}");
    }
    assert!(open_bound_token(&seed, &blob[..30]).is_err());
    assert!(open_bound_token(&[0x0au8; 32], &blob).is_err());
  }

  #[test]
  fn the_known_answer_of_the_contract_matches() {
    let fixture: serde_json::Value = serde_json::from_str(KNOWN_ANSWER).unwrap();
    let text = |name: &str| fixture[name].as_str().unwrap().to_owned();
    let hex = |bytes: &[u8]| bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let seed: [u8; 32] = [0x07; 32];
    assert_eq!(text("seed_hex"), hex(&seed));
    let key = GatewayServerKey::from_secret(&SymmetricKey::from_bytes(seed));
    assert_eq!(hex(key.public_key().as_bytes()), text("public_key_hex"));
    assert_eq!(key.public_key().server_id(), text("server_id"));
    let body = text("body");
    let request = GatewayRequest {
      method: "POST",
      path: "/v1/push",
      timestamp_ms: fixture["timestamp_ms"].as_u64().unwrap(),
      body: body.as_bytes(),
    };
    // Ed25519 is deterministic: the signature is the same bytes everywhere.
    assert_eq!(hex(&key.sign(&request)), text("signature_hex"));
  }

  #[test]
  fn a_signature_verifies_for_the_same_request() {
    let key = GatewayServerKey::generate(&mut OsRng);
    let signature = key.sign(&request(BODY));
    key.public_key().verify(&request(BODY), &signature).unwrap();
  }

  #[test]
  fn any_changed_part_of_the_request_is_rejected() {
    let key = GatewayServerKey::generate(&mut OsRng);
    let signature = key.sign(&request(BODY));
    let public = key.public_key();
    let changed = [
      GatewayRequest {
        body: b"{}",
        ..request(BODY)
      },
      GatewayRequest {
        method: "PUT",
        ..request(BODY)
      },
      GatewayRequest {
        path: "/api/v1/other",
        ..request(BODY)
      },
      GatewayRequest {
        timestamp_ms: 2,
        ..request(BODY)
      },
    ];
    for other in &changed {
      assert!(public.verify(other, &signature).is_err(), "{other:?}");
    }
  }

  #[test]
  fn another_key_or_a_short_signature_is_rejected() {
    let key = GatewayServerKey::generate(&mut OsRng);
    let other = GatewayServerKey::generate(&mut OsRng);
    let signature = key.sign(&request(BODY));
    assert!(other.public_key().verify(&request(BODY), &signature).is_err());
    assert!(key.public_key().verify(&request(BODY), &signature[..10]).is_err());
  }

  #[test]
  fn the_key_round_trips_through_its_secret_and_the_id_is_stable() {
    let key = GatewayServerKey::generate(&mut OsRng);
    let restored = GatewayServerKey::from_secret(&key.to_secret());
    assert_eq!(restored.public_key(), key.public_key());
    let id = key.public_key().server_id();
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(id, restored.public_key().server_id());
  }

  #[test]
  fn debug_output_hides_the_secret() {
    let key = GatewayServerKey::generate(&mut OsRng);
    assert_eq!(format!("{key:?}"), "GatewayServerKey([redacted])");
  }

  fn request(body: &[u8]) -> GatewayRequest<'_> {
    GatewayRequest {
      method: "POST",
      path: "/v1/push",
      timestamp_ms: 1,
      body,
    }
  }
}
