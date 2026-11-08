//! DPoP proofs (RFC 9449, e2ee.md §3.1.2) with a key the app keeps: on iOS in the Secure Enclave, or
//! a software key where there is none. The app stores the key and signs bytes; what a proof says, the
//! nonce and clock memory and what to do about an answer stay in the core
//! ([`teamcraft_core::api::dpop`]), so there is one implementation of RFC 9449.
//!
//! Two sets of callers: the engine's token source (API requests, gateway and relay first frames) and
//! the app's own token requests to the identity provider ([`ProviderProver`]).

use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_core::api::ApiError;
use teamcraft_core::api::dpop::{
  DpopProofError, DpopProver, DpopSignature, DpopSigner, ProofFallback, SignFuture, TokenEndpointAttempt,
  with_token_endpoint_proof,
};
use teamcraft_crypto::dpop::{DPOP_SIGNATURE_BYTES, DpopPublicKey};
use tokio::sync::oneshot;

/// The device's proof key as the app keeps it. Called on a thread of the core's or of its own, never
/// the engine's, so the app may wait for its key store here.
#[uniffi::export(with_foreign)]
pub trait DeviceProofKey: Send + Sync {
  /// The public half as an uncompressed SEC1 point (`0x04`, x, y). Throws
  /// [`DeviceProofError::KeyLost`] only if the store was read and has no key; a store that can't be
  /// read is [`DeviceProofError::KeyUnavailable`], which says nothing about the sign-in.
  fn public_key(&self) -> Result<Vec<u8>, DeviceProofError>;

  /// The ES256 signature of `message`: 64 bytes, `r || s`. The same errors as [`Self::public_key`].
  fn sign(&self, message: Vec<u8>) -> Result<Vec<u8>, DeviceProofError>;
}

/// One request to a token endpoint. Called with the proof to send, or `None` when the core lets a
/// session that isn't bound go without one.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait TokenEndpointTransport: Send + Sync {
  /// The answer as received: any status is an answer. Throw [`DeviceProofError::NoAnswer`] when
  /// there is none.
  async fn send(&self, proof: Option<String>) -> Result<TokenEndpointAnswer, DeviceProofError>;
}

/// Why a proof wasn't made or a token request didn't go through. No secrets in here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DeviceProofError {
  /// The store was read and the key isn't there: signing in again is the way out.
  #[error("the device's key is gone")]
  KeyLost,
  /// The key can't be used right now (locked, or its store doesn't answer): nothing is decided.
  #[error("the device's key can't be used right now")]
  KeyUnavailable,
  /// The token endpoint didn't answer.
  #[error("no answer from the token endpoint: {reason}")]
  NoAnswer { reason: String },
}

/// An answer of a token endpoint, as far as proofs care. `body` is the answer's body, for the token
/// endpoint's error and for the caller: it holds tokens, so this has no `Debug`.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct TokenEndpointAnswer {
  pub status: u16,
  pub body: Vec<u8>,
  /// The `DPoP-Nonce` header.
  pub nonce: Option<String>,
  /// The `Date` header.
  pub date: Option<String>,
}

/// When a request goes without its proof after all ([`ProofFallback`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TokenProofFallback {
  /// Never: the session is bound, and only its proof will do.
  None,
  /// After the provider refused it (400 or 401, not `invalid_grant`), for a session that isn't bound.
  OnRefusal,
}

/// The device's key with what is known about the identity provider: its nonce and clock.
#[derive(uniffi::Object)]
pub struct ProviderProver {
  prover: DpopProver,
}

/// The app's key as the engine's token source uses it: read when a bound token first needs it.
#[derive(Clone)]
pub(crate) struct DeviceKeyProofs {
  key: Arc<dyn DeviceProofKey>,
  maybe_prover: Arc<Mutex<Option<Arc<DpopProver>>>>,
}

/// The app's key as the core signs with it.
struct ForeignSigner {
  key: Arc<dyn DeviceProofKey>,
  public: DpopPublicKey,
}

impl DpopSigner for ForeignSigner {
  fn public_key(&self) -> &DpopPublicKey {
    &self.public
  }

  fn sign<'a>(&'a self, message: &'a [u8]) -> SignFuture<'a> {
    let key = Arc::clone(&self.key);
    let message = message.to_vec();
    Box::pin(async move {
      // On a thread of its own: the Secure Enclave takes a while, and a proof must not hold the
      // thread that runs the sync loop.
      let (answer, answered) = oneshot::channel();
      std::thread::Builder::new()
        .name("teamcraft-dpop".into())
        .spawn(move || {
          // The caller may have been dropped meanwhile; nobody needs the signature then.
          let _ = answer.send(key.sign(message));
        })
        .map_err(|_| DpopProofError::KeyUnavailable)?;
      let signature = answered
        .await
        .map_err(|_| DpopProofError::KeyUnavailable)?
        .map_err(DpopProofError::from)?;
      <DpopSignature>::try_from(signature.as_slice()).map_err(|_| {
        DpopProofError::Signer(format!(
          "the key signed {} bytes, not {DPOP_SIGNATURE_BYTES}",
          signature.len()
        ))
      })
    })
  }
}

impl From<DeviceProofError> for DpopProofError {
  fn from(error: DeviceProofError) -> Self {
    match error {
      DeviceProofError::KeyLost => Self::KeyLost,
      DeviceProofError::KeyUnavailable | DeviceProofError::NoAnswer { .. } => Self::KeyUnavailable,
    }
  }
}

impl From<DpopProofError> for DeviceProofError {
  fn from(error: DpopProofError) -> Self {
    match error {
      DpopProofError::KeyLost => Self::KeyLost,
      // Nothing about the session: only what the app's key store says ends it.
      DpopProofError::KeyUnavailable | DpopProofError::Signer(_) | DpopProofError::Proof(_) => {
        Self::KeyUnavailable
      },
    }
  }
}

impl From<uniffi::UnexpectedUniFFICallbackError> for DeviceProofError {
  fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
    // The reason isn't logged: nothing says what is in an error the app didn't mean to throw.
    tracing::warn!("the app's key failed in an unexpected way");
    Self::KeyUnavailable
  }
}

impl From<TokenProofFallback> for ProofFallback {
  fn from(fallback: TokenProofFallback) -> Self {
    match fallback {
      TokenProofFallback::None => Self::None,
      TokenProofFallback::OnRefusal => Self::OnRefusal,
    }
  }
}

#[uniffi::export]
impl ProviderProver {
  /// Reads the key's public half; the errors of [`DeviceProofKey::public_key`].
  #[uniffi::constructor]
  pub fn new(key: Arc<dyn DeviceProofKey>) -> Result<Arc<Self>, DeviceProofError> {
    Ok(Arc::new(Self {
      prover: DpopProver::new(foreign_signer(key)?),
    }))
  }

  /// The thumbprint tokens are bound to with this key (`cnf.jkt`).
  pub fn thumbprint(&self) -> String {
    self.prover.thumbprint().as_str().to_owned()
  }

  /// The provider refused a proof for a session that isn't bound: refreshes go without one for now.
  pub fn has_refused_proof(&self) -> bool {
    self.prover.has_refused_proof()
  }

  /// One request to the token endpoint, signed: the provider's nonce challenge or a date it refuses
  /// gets exactly one more try, and `fallback` says when the request goes without the proof.
  pub async fn token_request(
    &self,
    endpoint: String,
    fallback: TokenProofFallback,
    transport: Arc<dyn TokenEndpointTransport>,
  ) -> Result<TokenEndpointAnswer, DeviceProofError> {
    let transport = &transport;
    Box::pin(with_token_endpoint_proof(
      &self.prover,
      &endpoint,
      fallback.into(),
      |maybe_proof: Option<String>| async move {
        let answer = transport.send(maybe_proof).await?;
        Ok::<_, DeviceProofError>(TokenEndpointAttempt {
          status: answer.status,
          body: answer.body.clone(),
          maybe_nonce: answer.nonce.clone(),
          maybe_date: answer.date.clone(),
          response: answer,
        })
      },
    ))
    .await
  }
}

impl DeviceKeyProofs {
  pub(crate) fn new(key: Arc<dyn DeviceProofKey>) -> Self {
    Self {
      key,
      maybe_prover: Arc::default(),
    }
  }

  /// What signs the engine's requests right now, read without waiting.
  pub(crate) fn prover(&self) -> Option<Arc<DpopProver>> {
    self.slot().clone()
  }

  /// Gets the prover ready for `token`, on the thread that asks the app for tokens, which is allowed
  /// to wait for the key store. A token that names no key needs none. A key that can't be read is a
  /// wait, a key that is gone (or isn't the one the token names) is a sign-in: nothing goes out.
  pub(crate) fn prepare_for(&self, token: &str) -> Result<(), ApiError> {
    let Some(bound) = teamcraft_crypto::dpop::token_bound_key(token) else {
      return Ok(());
    };
    if self.prover().is_some_and(|prover| prover.thumbprint() == bound) {
      return Ok(());
    }
    let signer = foreign_signer(Arc::clone(&self.key)).map_err(|error| match error {
      DeviceProofError::KeyLost => ApiError::BoundKeyLost,
      DeviceProofError::KeyUnavailable | DeviceProofError::NoAnswer { .. } => {
        ApiError::Network(error.to_string())
      },
    })?;
    let prover = Arc::new(DpopProver::new(signer));
    if prover.thumbprint() != bound {
      return Err(ApiError::BoundKeyLost);
    }
    *self.slot() = Some(prover);
    Ok(())
  }

  fn slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<DpopProver>>> {
    self.maybe_prover.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

fn foreign_signer(key: Arc<dyn DeviceProofKey>) -> Result<Arc<ForeignSigner>, DeviceProofError> {
  let point = key.public_key()?;
  let public = DpopPublicKey::from_uncompressed_point(&point).map_err(|error| {
    tracing::warn!(%error, "the app's key has no usable public half");
    DeviceProofError::KeyUnavailable
  })?;
  Ok(Arc::new(ForeignSigner { key, public }))
}

/// The key `access_token` is bound to (`cnf.jkt`), read from the token without checking it: whether
/// the session has to prove a key from now on. `None` for an opaque or unbound token.
#[uniffi::export]
pub fn token_bound_key(access_token: String) -> Option<String> {
  teamcraft_crypto::dpop::token_bound_key(&access_token).map(|key| key.as_str().to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod test_support {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use serde_json::Value;
  use teamcraft_crypto::dpop::{DpopPublicKey, DpopSigningKey, JwkThumbprint};

  use super::*;

  pub(crate) struct StandInKey {
    pub(crate) key: Mutex<Option<DpopSigningKey>>,
    pub(crate) maybe_failure: Mutex<Option<DeviceProofError>>,
    pub(crate) public_key_reads: AtomicUsize,
  }

  impl StandInKey {
    pub(crate) fn new() -> Arc<Self> {
      Arc::new(Self {
        key: Mutex::new(Some(DpopSigningKey::generate())),
        maybe_failure: Mutex::default(),
        public_key_reads: AtomicUsize::new(0),
      })
    }

    pub(crate) fn fail_with(&self, failure: DeviceProofError) {
      *self.maybe_failure.lock().unwrap() = Some(failure);
    }

    pub(crate) fn thumbprint(&self) -> JwkThumbprint {
      self.key.lock().unwrap().as_ref().unwrap().thumbprint()
    }
  }

  impl DeviceProofKey for StandInKey {
    fn public_key(&self) -> Result<Vec<u8>, DeviceProofError> {
      self.public_key_reads.fetch_add(1, Ordering::SeqCst);
      if let Some(failure) = self.maybe_failure.lock().unwrap().clone() {
        return Err(failure);
      }
      let guard = self.key.lock().unwrap();
      let key = guard.as_ref().ok_or(DeviceProofError::KeyLost)?;
      Ok(uncompressed_point(&key.public_key()))
    }

    fn sign(&self, message: Vec<u8>) -> Result<Vec<u8>, DeviceProofError> {
      if let Some(failure) = self.maybe_failure.lock().unwrap().clone() {
        return Err(failure);
      }
      let guard = self.key.lock().unwrap();
      let key = guard.as_ref().ok_or(DeviceProofError::KeyLost)?;
      Ok(key.sign_bytes(&message).to_vec())
    }
  }

  /// `key`'s public half as a key store hands it out.
  fn uncompressed_point(key: &DpopPublicKey) -> Vec<u8> {
    // The thumbprint input has the coordinates: read them from the public JWK of a proof.
    let draft = teamcraft_crypto::dpop::DpopProofDraft::new(
      key,
      &teamcraft_crypto::dpop::DpopProofRequest {
        method: "GET",
        target_uri: "https://example.com/",
        issued_at_unix: 0,
        maybe_access_token: None,
        maybe_nonce: None,
      },
    )
    .unwrap();
    let header = String::from_utf8(draft.signing_input().to_vec()).unwrap();
    let header: Value =
      serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header.split('.').next().unwrap()).unwrap()).unwrap();
    let mut point = vec![0x04];
    point.extend(
      URL_SAFE_NO_PAD
        .decode(header["jwk"]["x"].as_str().unwrap())
        .unwrap(),
    );
    point.extend(
      URL_SAFE_NO_PAD
        .decode(header["jwk"]["y"].as_str().unwrap())
        .unwrap(),
    );
    point
  }

  pub(crate) fn bound_token(jkt: &JwkThumbprint) -> String {
    let claims = format!(r#"{{"cnf":{{"jkt":"{}"}}}}"#, jkt.as_str());
    format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(claims))
  }
}

#[cfg(test)]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#[allow(clippy::unwrap_used)]
mod tests {
  use std::sync::atomic::Ordering;

  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use serde_json::Value;
  use teamcraft_core::api::dpop::signed_headers;
  use teamcraft_crypto::dpop::{DpopSigningKey, access_token_hash};

  use super::*;

  const ENDPOINT: &str = "https://id.example.com/realms/team/token?x=1";
  const NONCE: &str = "provider-nonce";

  use super::test_support::{StandInKey, bound_token};

  fn claims_of(proof: &str) -> Value {
    let payload = proof.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
  }

  /// Keeps what it was asked to send and answers from a script.
  struct Script {
    sent: Mutex<Vec<Option<String>>>,
    answers: Mutex<Vec<Result<TokenEndpointAnswer, DeviceProofError>>>,
  }

  impl Script {
    fn answering(answers: Vec<Result<TokenEndpointAnswer, DeviceProofError>>) -> Arc<Self> {
      Arc::new(Self {
        sent: Mutex::default(),
        answers: Mutex::new(answers),
      })
    }

    fn sent(&self) -> Vec<Option<String>> {
      self.sent.lock().unwrap().clone()
    }
  }

  #[async_trait::async_trait]
  impl TokenEndpointTransport for Script {
    async fn send(&self, proof: Option<String>) -> Result<TokenEndpointAnswer, DeviceProofError> {
      self.sent.lock().unwrap().push(proof);
      self.answers.lock().unwrap().remove(0)
    }
  }

  fn answer(status: u16, body: &str, nonce: Option<&str>) -> Result<TokenEndpointAnswer, DeviceProofError> {
    Ok(TokenEndpointAnswer {
      status,
      body: body.as_bytes().to_vec(),
      nonce: nonce.map(str::to_owned),
      date: None,
    })
  }

  mod the_key_as_the_core_signs_with_it {
    use super::*;

    #[tokio::test]
    async fn a_foreign_key_signs_a_proof_with_the_right_claims() {
      let key = StandInKey::new();
      let prover = DpopProver::new(foreign_signer(key.clone()).unwrap());
      assert_eq!(prover.thumbprint(), key.thumbprint());
      let proof = prover
        .proof("POST", "https://chat.example.com/api/v1/me?x=1", Some("access"))
        .await
        .unwrap();
      let claims = claims_of(proof.as_str());
      assert_eq!(claims["htm"], "POST");
      assert_eq!(claims["htu"], "https://chat.example.com/api/v1/me");
      assert_eq!(claims["ath"], access_token_hash("access"));
    }

    #[tokio::test]
    async fn a_signature_that_cant_be_made_is_a_wait_and_a_lost_key_a_sign_in() {
      let key = StandInKey::new();
      let prover = DpopProver::new(foreign_signer(key.clone()).unwrap());
      key.fail_with(DeviceProofError::KeyUnavailable);
      assert_eq!(
        prover
          .proof("GET", "https://chat.example.com/x", None)
          .await
          .err(),
        Some(DpopProofError::KeyUnavailable)
      );
      key.fail_with(DeviceProofError::KeyLost);
      assert_eq!(
        prover
          .proof("GET", "https://chat.example.com/x", None)
          .await
          .err(),
        Some(DpopProofError::KeyLost)
      );
    }

    #[tokio::test]
    async fn a_signature_of_the_wrong_length_is_no_signature() {
      struct Short(Arc<StandInKey>);
      impl DeviceProofKey for Short {
        fn public_key(&self) -> Result<Vec<u8>, DeviceProofError> {
          self.0.public_key()
        }
        fn sign(&self, _: Vec<u8>) -> Result<Vec<u8>, DeviceProofError> {
          Ok(vec![0; 3])
        }
      }
      let prover = DpopProver::new(foreign_signer(Arc::new(Short(StandInKey::new()))).unwrap());
      assert!(matches!(
        prover
          .proof("GET", "https://chat.example.com/x", None)
          .await
          .err(),
        Some(DpopProofError::Signer(_))
      ));
    }
  }

  mod the_engines_token_source {
    use super::*;

    #[test]
    fn a_token_that_names_no_key_reads_no_key() {
      let key = StandInKey::new();
      let proofs = DeviceKeyProofs::new(key.clone());
      proofs.prepare_for("opaque").unwrap();
      assert_eq!(key.public_key_reads.load(Ordering::SeqCst), 0);
      assert!(proofs.prover().is_none());
    }

    #[tokio::test]
    async fn a_bound_token_gets_the_key_and_keeps_it_for_the_next_one() {
      let key = StandInKey::new();
      let proofs = DeviceKeyProofs::new(key.clone());
      let token = bound_token(&key.thumbprint());
      proofs.prepare_for(&token).unwrap();
      proofs.prepare_for(&token).unwrap();
      assert_eq!(key.public_key_reads.load(Ordering::SeqCst), 1);
      let prover = proofs.prover().unwrap();
      let url = "https://chat.example.com/api/v1/me";
      let signed = signed_headers(Some(&prover), &token, "GET", url).await.unwrap();
      assert_eq!(signed.authorization, format!("DPoP {token}"));
      let claims = claims_of(signed.maybe_proof.unwrap().as_str());
      assert_eq!(claims["ath"], access_token_hash(&token));
    }

    #[test]
    fn a_key_that_cant_be_read_is_a_wait_and_decides_nothing() {
      let key = StandInKey::new();
      let proofs = DeviceKeyProofs::new(key.clone());
      key.fail_with(DeviceProofError::KeyUnavailable);
      let error = proofs.prepare_for(&bound_token(&key.thumbprint())).unwrap_err();
      assert!(error.is_transient() && !error.is_sign_in_required(), "{error}");
    }

    #[test]
    fn a_key_that_is_gone_or_another_one_is_a_lost_key() {
      let key = StandInKey::new();
      let proofs = DeviceKeyProofs::new(key.clone());
      let token = bound_token(&key.thumbprint());
      *key.key.lock().unwrap() = None;
      assert!(matches!(
        proofs.prepare_for(&token).unwrap_err(),
        ApiError::BoundKeyLost
      ));

      let other = StandInKey::new();
      let proofs = DeviceKeyProofs::new(other.clone());
      let error = proofs.prepare_for(&token).unwrap_err();
      assert!(matches!(error, ApiError::BoundKeyLost) && error.is_sign_in_required());
    }
  }

  mod the_token_endpoint {
    use super::*;

    #[tokio::test]
    async fn a_nonce_challenge_gets_one_more_try_with_its_nonce() {
      let key = StandInKey::new();
      let prover = ProviderProver::new(key).unwrap();
      let script = Script::answering(vec![
        answer(400, r#"{"error":"use_dpop_nonce"}"#, Some(NONCE)),
        answer(200, "{}", None),
      ]);
      let answer = prover
        .token_request(ENDPOINT.into(), TokenProofFallback::None, script.clone())
        .await
        .unwrap();
      assert_eq!(answer.status, 200);
      let sent = script.sent();
      assert_eq!(sent.len(), 2);
      let first = claims_of(sent[0].as_ref().unwrap());
      assert!(first.get("nonce").is_none());
      assert!(first.get("ath").is_none());
      let second = claims_of(sent[1].as_ref().unwrap());
      assert_eq!(second["nonce"], NONCE);
      assert_eq!(second["htm"], "POST");
      assert_eq!(second["htu"], "https://id.example.com/realms/team/token");
    }

    #[tokio::test]
    async fn a_provider_that_keeps_challenging_is_asked_twice_and_no_more() {
      let prover = ProviderProver::new(StandInKey::new()).unwrap();
      let script = Script::answering(vec![
        answer(400, r#"{"error":"use_dpop_nonce"}"#, Some("one")),
        answer(400, r#"{"error":"use_dpop_nonce"}"#, Some("two")),
      ]);
      let answer = prover
        .token_request(ENDPOINT.into(), TokenProofFallback::None, script.clone())
        .await
        .unwrap();
      assert_eq!(answer.status, 400);
      assert_eq!(script.sent().len(), 2);
    }

    #[tokio::test]
    async fn an_unbound_session_goes_without_after_a_refusal_and_a_bound_one_never_does() {
      let prover = ProviderProver::new(StandInKey::new()).unwrap();
      let script = Script::answering(vec![
        answer(400, r#"{"error":"invalid_dpop_proof"}"#, None),
        answer(200, "{}", None),
      ]);
      prover
        .token_request(ENDPOINT.into(), TokenProofFallback::OnRefusal, script.clone())
        .await
        .unwrap();
      assert_eq!(
        script.sent().iter().map(Option::is_some).collect::<Vec<_>>(),
        vec![true, false]
      );
      assert!(prover.has_refused_proof());

      let prover = ProviderProver::new(StandInKey::new()).unwrap();
      let script = Script::answering(vec![answer(400, r#"{"error":"invalid_dpop_proof"}"#, None)]);
      let answer = prover
        .token_request(ENDPOINT.into(), TokenProofFallback::None, script.clone())
        .await
        .unwrap();
      assert_eq!(answer.status, 400);
      assert_eq!(script.sent().len(), 1);
      assert!(script.sent()[0].is_some());
      assert!(!prover.has_refused_proof());
    }

    #[tokio::test]
    async fn no_answer_comes_back_as_it_is() {
      let prover = ProviderProver::new(StandInKey::new()).unwrap();
      let script = Script::answering(vec![Err(DeviceProofError::NoAnswer {
        reason: "offline".into(),
      })]);
      let error = prover
        .token_request(ENDPOINT.into(), TokenProofFallback::OnRefusal, script.clone())
        .await
        .err()
        .unwrap();
      assert_eq!(
        error,
        DeviceProofError::NoAnswer {
          reason: "offline".into()
        }
      );
      assert_eq!(script.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_lost_key_goes_no_further_than_the_key() {
      let key = StandInKey::new();
      let prover = ProviderProver::new(key.clone()).unwrap();
      key.fail_with(DeviceProofError::KeyLost);
      let script = Script::answering(vec![]);
      let error = prover
        .token_request(ENDPOINT.into(), TokenProofFallback::None, script.clone())
        .await
        .err()
        .unwrap();
      assert_eq!(error, DeviceProofError::KeyLost);
      assert!(script.sent().is_empty());
    }

    #[test]
    fn a_missing_key_at_the_start_is_a_lost_key_and_says_so() {
      let key = StandInKey::new();
      *key.key.lock().unwrap() = None;
      assert_eq!(ProviderProver::new(key).err(), Some(DeviceProofError::KeyLost));
    }
  }

  #[test]
  fn the_key_of_a_token_is_read_from_its_claims() {
    let jkt = DpopSigningKey::generate().thumbprint();
    assert_eq!(token_bound_key(bound_token(&jkt)).as_deref(), Some(jkt.as_str()));
    assert_eq!(token_bound_key("opaque".into()), None);
  }
}
