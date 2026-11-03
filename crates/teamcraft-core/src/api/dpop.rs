//! Sending DPoP proofs (RFC 9449; e2ee.md §3.1.2): a device's key behind [`DpopSigner`], so a key in a
//! platform's key store fits later, and [`DpopProver`] with what a client remembers about one server:
//! its nonce, and how far the local clock is from the server's.
//!
//! Proofs go with bound tokens only ([`Authorization::of`]): a proof proves nothing about an unbound
//! token (the server doesn't count it either), and would add a round trip for it.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_crypto::dpop::{
  DPOP_SIGNATURE_BYTES, DpopError, DpopProof, DpopProofDraft, DpopProofRequest, DpopPublicKey,
  DpopSigningKey, JwkThumbprint, MAX_NONCE_BYTES, token_bound_key,
};
use teamcraft_proto::ErrorCode;

use super::ApiError;
use crate::clock::{Clock, SystemClock};

/// The request header that carries a proof.
pub const DPOP_HEADER: &str = "DPoP";
/// The response header that carries the nonce for the next proof.
pub const DPOP_NONCE_HEADER: &str = "DPoP-Nonce";
/// What an identity provider's discovery document lists when it takes ES256 proofs (RFC 9449 §5.1).
pub const DPOP_ALGORITHM_FOR_DISCOVERY: &str = teamcraft_crypto::dpop::DPOP_ALGORITHM;
/// A `Date` this much off the current offset is worth one more try after `invalid_dpop_proof`. Less
/// than that can't be why the proof was refused: the server allows a minute by default.
const CLOCK_CORRECTION_SECS: i64 = 5;

/// A future of a signer: `Send` where the app signs from several threads, not in the browser, whose
/// Web Crypto objects stay on one.
#[cfg(not(target_arch = "wasm32"))]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<DpopSignature, DpopProofError>> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<DpopSignature, DpopProofError>> + 'a>>;

/// An ES256 signature as a JWS carries it.
pub type DpopSignature = [u8; DPOP_SIGNATURE_BYTES];

/// A device's proof key: all the core needs of it. Web Crypto implements it in the browser, a
/// hardware-backed key can on the platforms that have one (e2ee.md §3.1.2).
#[cfg(not(target_arch = "wasm32"))]
pub trait DpopSigner: Send + Sync {
  fn public_key(&self) -> &DpopPublicKey;
  fn sign<'a>(&'a self, message: &'a [u8]) -> SignFuture<'a>;
}

/// See the native trait: the same, without the thread bounds.
#[cfg(target_arch = "wasm32")]
pub trait DpopSigner {
  fn public_key(&self) -> &DpopPublicKey;
  fn sign<'a>(&'a self, message: &'a [u8]) -> SignFuture<'a>;
}

/// A key whose private half is in this process: the desktop's, from the sealed secrets file.
pub struct SoftwareDpopSigner {
  key: DpopSigningKey,
  public: DpopPublicKey,
}

/// Signs proofs for one server (or one identity provider), and remembers its nonce and clock.
pub struct DpopProver {
  signer: Arc<dyn DpopSigner>,
  state: Mutex<ProverState>,
}

#[derive(Debug, Default)]
struct ProverState {
  maybe_nonce: Option<String>,
  /// The server's clock minus the local one, in seconds.
  clock_offset_secs: i64,
}

/// Why no proof could be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DpopProofError {
  #[error("the proof could not be made: {0}")]
  Proof(#[from] DpopError),
  #[error("the key could not sign: {0}")]
  Signer(String),
}

/// How a token goes out: the value of `Authorization`, and the proof next to it for a bound token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authorization {
  Bearer,
  /// `Authorization: DPoP`; the key to prove is the prover's.
  Bound(JwkThumbprint),
}

/// What goes out with a request for a token: `Authorization`, and `DPoP` next to it for a bound one.
pub struct SignedHeaders {
  pub authorization: String,
  pub maybe_proof: Option<DpopProof>,
}

/// Why a token can't be sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DpopSendError {
  /// The token is bound to a key this device doesn't have any more. Signing in again is the way out.
  #[error("the key this sign-in is bound to is gone")]
  KeyLost,
  #[error(transparent)]
  Proof(#[from] DpopProofError),
}

/// One answer of a token endpoint as [`with_token_endpoint_proof`] looks at it.
pub struct TokenEndpointAttempt<R> {
  pub status: u16,
  pub body: Vec<u8>,
  pub maybe_nonce: Option<String>,
  pub maybe_date: Option<String>,
  /// What the caller's HTTP client keeps of the answer.
  pub response: R,
}

/// What a token answer of an identity provider or an API answer tells a prover.
#[derive(Debug, Clone, Copy, Default)]
pub struct DpopAnswer<'a> {
  pub maybe_nonce: Option<&'a str>,
  pub maybe_date: Option<&'a str>,
}

impl SoftwareDpopSigner {
  pub fn new(key: DpopSigningKey) -> Self {
    let public = key.public_key();
    Self { key, public }
  }
}

impl DpopSigner for SoftwareDpopSigner {
  fn public_key(&self) -> &DpopPublicKey {
    &self.public
  }

  fn sign<'a>(&'a self, message: &'a [u8]) -> SignFuture<'a> {
    Box::pin(std::future::ready(Ok(self.key.sign_bytes(message))))
  }
}

impl Authorization {
  /// A token that names a key (`cnf.jkt`) needs the `DPoP` scheme and a proof. Any other token is a
  /// bearer token, also under the policy `required`, which then refuses it with `dpop_required`:
  /// a proof wouldn't change that.
  pub fn of(token: &str) -> Self {
    token_bound_key(token).map_or(Self::Bearer, Self::Bound)
  }
}

impl DpopProver {
  pub fn new(signer: Arc<dyn DpopSigner>) -> Self {
    Self {
      signer,
      state: Mutex::default(),
    }
  }

  /// The same key for another party, the identity provider: its own nonce. The clock starts from
  /// this party's, since a browser often can't read the other's `Date`.
  pub fn for_another_party(&self) -> Self {
    Self {
      signer: Arc::clone(&self.signer),
      state: Mutex::new(ProverState {
        maybe_nonce: None,
        clock_offset_secs: self.state().clock_offset_secs,
      }),
    }
  }

  /// The thumbprint tokens are bound to with this key.
  pub fn thumbprint(&self) -> JwkThumbprint {
    self.signer.public_key().thumbprint()
  }

  /// A proof for `method` on `target_uri`, dated by the server's clock as far as known, with the
  /// nonce as far as known. `maybe_access_token` is `None` at the token endpoint.
  pub async fn proof(
    &self,
    method: &str,
    target_uri: &str,
    maybe_access_token: Option<&str>,
  ) -> Result<DpopProof, DpopProofError> {
    let (maybe_nonce, offset) = {
      let state = self.state();
      (state.maybe_nonce.clone(), state.clock_offset_secs)
    };
    let draft = DpopProofDraft::new(
      self.signer.public_key(),
      &DpopProofRequest {
        method,
        target_uri,
        issued_at_unix: now_unix().saturating_add(offset),
        maybe_access_token,
        maybe_nonce: maybe_nonce.as_deref(),
      },
    )?;
    let signature = self.signer.sign(draft.signing_input()).await?;
    Ok(draft.finish(&signature))
  }

  /// Takes in an answer's `DPoP-Nonce` and `Date`.
  pub fn note_answer(&self, answer: DpopAnswer<'_>) -> Learned {
    let mut learned = Learned::default();
    if let Some(nonce) = answer.maybe_nonce {
      learned.has_new_nonce = self.remember_nonce(nonce);
    }
    if let Some(server_now) = answer.maybe_date.and_then(parse_http_date) {
      let offset = server_now.saturating_sub(now_unix());
      let mut state = self.state();
      let moved = offset.saturating_sub(state.clock_offset_secs).abs();
      learned.has_clock_moved = moved >= CLOCK_CORRECTION_SECS;
      state.clock_offset_secs = offset;
    }
    learned
  }

  /// Takes in the nonce of a `dpop_nonce` frame or of an answer whose `Date` says nothing (one from
  /// a cache). True when it is not the one known already.
  pub fn remember_nonce(&self, nonce: &str) -> bool {
    // The server's own limit: a longer one could only come from something else on the way.
    if nonce.is_empty() || nonce.len() > MAX_NONCE_BYTES {
      return false;
    }
    let mut state = self.state();
    if state.maybe_nonce.as_deref() == Some(nonce) {
      return false;
    }
    state.maybe_nonce = Some(nonce.to_owned());
    true
  }

  /// Whether the nonce is known, for measuring and tests.
  pub fn has_nonce(&self) -> bool {
    self.state().maybe_nonce.is_some()
  }

  fn state(&self) -> std::sync::MutexGuard<'_, ProverState> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl std::fmt::Debug for DpopProver {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DpopProver")
      .field("key", &self.thumbprint())
      .finish_non_exhaustive()
  }
}

/// What an answer changed in a prover.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Learned {
  has_new_nonce: bool,
  has_clock_moved: bool,
}

impl Learned {
  /// Whether a request refused with `code` is worth its one more try: the nonce it lacked came with
  /// the answer, or its date was wrong and the answer's `Date` says so. Anything else would only
  /// be refused again, and refused proofs count as failed sign-ins.
  pub fn is_worth_a_retry(self, code: &ErrorCode) -> bool {
    match code {
      ErrorCode::UseDpopNonce => self.has_new_nonce,
      ErrorCode::InvalidDpopProof => self.has_clock_moved,
      _ => false,
    }
  }
}

impl From<DpopSendError> for ApiError {
  fn from(error: DpopSendError) -> Self {
    match error {
      DpopSendError::KeyLost => Self::BoundKeyLost,
      DpopSendError::Proof(error) => Self::Token(error.to_string()),
    }
  }
}

/// How `token` goes out on `method url`. A token bound to the key of `maybe_prover` gets a proof; one
/// bound to another key can't be used by this device ([`DpopSendError::KeyLost`]). On a platform with
/// no key, a bound token goes as a bearer token, and the server says `dpop_required`.
pub async fn signed_headers(
  maybe_prover: Option<&DpopProver>,
  token: &str,
  method: &str,
  url: &str,
) -> Result<SignedHeaders, DpopSendError> {
  let (Authorization::Bound(bound), Some(prover)) = (Authorization::of(token), maybe_prover) else {
    return Ok(SignedHeaders {
      authorization: format!("Bearer {token}"),
      maybe_proof: None,
    });
  };
  if prover.thumbprint() != bound {
    return Err(DpopSendError::KeyLost);
  }
  let proof = prover.proof(method, url, Some(token)).await?;
  Ok(SignedHeaders {
    authorization: format!("DPoP {token}"),
    maybe_proof: Some(proof),
  })
}

/// A request to an OAuth token endpoint with a proof: `attempt` sends it with the proof it is given.
/// The provider's nonce challenge (RFC 9449 §8) or a date it refuses gets exactly one more try.
pub async fn with_token_endpoint_proof<R, E, F, Fut>(
  prover: &DpopProver,
  token_endpoint: &str,
  attempt: F,
) -> Result<R, E>
where
  E: From<DpopProofError>,
  F: Fn(String) -> Fut,
  Fut: Future<Output = Result<TokenEndpointAttempt<R>, E>>,
{
  let proof = prover.proof("POST", token_endpoint, None).await?;
  let first = attempt(proof.as_str().to_owned()).await?;
  let learned = prover.note_answer(DpopAnswer {
    maybe_nonce: first.maybe_nonce.as_deref(),
    maybe_date: first.maybe_date.as_deref(),
  });
  let is_retried =
    token_endpoint_error(first.status, &first.body).is_some_and(|code| learned.is_worth_a_retry(&code));
  if !is_retried {
    return Ok(first.response);
  }
  let proof = prover.proof("POST", token_endpoint, None).await?;
  let second = attempt(proof.as_str().to_owned()).await?;
  prover.note_answer(DpopAnswer {
    maybe_nonce: second.maybe_nonce.as_deref(),
    maybe_date: second.maybe_date.as_deref(),
  });
  Ok(second.response)
}

/// What the error of an OAuth token endpoint means for its proof (RFC 9449 §8): `use_dpop_nonce` and
/// `invalid_dpop_proof` come as `error` in a JSON body with status 400, and sometimes 401.
pub fn token_endpoint_error(status: u16, body: &[u8]) -> Option<ErrorCode> {
  #[derive(serde::Deserialize)]
  struct OAuthError {
    error: String,
  }
  if !matches!(status, 400 | 401) {
    return None;
  }
  match serde_json::from_slice::<OAuthError>(body).ok()?.error.as_str() {
    "use_dpop_nonce" => Some(ErrorCode::UseDpopNonce),
    "invalid_dpop_proof" => Some(ErrorCode::InvalidDpopProof),
    _ => None,
  }
}

/// The Unix time of an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the only form a server may
/// send today (RFC 9110 §5.6.7). Anything else is `None`: then the clock stays as it is.
pub fn parse_http_date(value: &str) -> Option<i64> {
  let mut parts = value.trim().split(' ');
  let _weekday = parts.next()?.strip_suffix(',')?;
  let day: i64 = parts.next()?.parse().ok()?;
  let month = parts.next()?;
  let year: i64 = parts.next()?.parse().ok()?;
  let mut time = parts.next()?.split(':').map(str::parse::<i64>);
  let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
  if parts.next()? != "GMT" || parts.next().is_some() || time.next().is_some() {
    return None;
  }
  const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];
  let month = MONTHS.iter().position(|name| *name == month)? as i64 + 1;
  let is_valid = (1..=31).contains(&day)
    && (0..24).contains(&hour)
    && (0..60).contains(&minute)
    && (0..=60).contains(&second);
  is_valid.then(|| days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
  let year = if month <= 2 { year - 1 } else { year };
  let era = year.div_euclid(400);
  let year_of_era = year.rem_euclid(400);
  let shifted_month = (month + 9) % 12;
  let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  era * 146_097 + day_of_era - 719_468
}

fn now_unix() -> i64 {
  SystemClock.now_ms().div_euclid(1_000)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#[allow(clippy::unwrap_used)]
mod tests {
  use base64::Engine as _;
  use base64::engine::general_purpose::URL_SAFE_NO_PAD;
  use serde_json::Value;

  use super::*;

  const TARGET: &str = "https://chat.example.com/api/v1/me?x=1";
  const NONCE: &str = "the-nonce";

  mod proofs {
    use super::*;

    #[tokio::test]
    async fn a_proof_has_the_token_hash_the_nonce_and_no_query() {
      let prover = prover();
      prover.remember_nonce(NONCE);
      let claims = claims_of(&prover.proof("GET", TARGET, Some("token")).await.unwrap());
      assert_eq!(claims["htm"], "GET");
      assert_eq!(claims["htu"], "https://chat.example.com/api/v1/me");
      assert_eq!(claims["nonce"], NONCE);
      assert_eq!(claims["ath"], teamcraft_crypto::dpop::access_token_hash("token"));
    }

    #[tokio::test]
    async fn at_the_token_endpoint_there_is_no_token_hash() {
      let claims = claims_of(&prover().proof("POST", TARGET, None).await.unwrap());
      assert!(claims.get("ath").is_none());
      assert!(claims.get("nonce").is_none());
    }

    #[tokio::test]
    async fn the_date_goes_by_the_servers_clock() {
      let prover = prover();
      let server_now = now_unix() + 3_600;
      let learned = prover.note_answer(DpopAnswer {
        maybe_nonce: None,
        maybe_date: Some(&http_date(server_now)),
      });
      assert!(learned.has_clock_moved);
      let iat = claims_of(&prover.proof("GET", TARGET, None).await.unwrap())["iat"]
        .as_i64()
        .unwrap();
      assert!((iat - server_now).abs() <= 2, "{iat} {server_now}");
    }

    #[tokio::test]
    async fn two_proofs_never_share_a_jti() {
      let prover = prover();
      let first = claims_of(&prover.proof("GET", TARGET, None).await.unwrap());
      let second = claims_of(&prover.proof("GET", TARGET, None).await.unwrap());
      assert_ne!(first["jti"], second["jti"]);
    }

    #[tokio::test]
    async fn another_party_shares_the_key_and_not_the_nonce() {
      let prover = prover();
      prover.remember_nonce(NONCE);
      let provider = prover.for_another_party();
      assert_eq!(provider.thumbprint(), prover.thumbprint());
      assert!(!provider.has_nonce());
    }
  }

  mod retries {
    use super::*;

    #[test]
    fn a_nonce_challenge_is_retried_only_with_a_new_nonce() {
      let prover = prover();
      let with_nonce = DpopAnswer {
        maybe_nonce: Some(NONCE),
        maybe_date: None,
      };
      let first = prover.note_answer(with_nonce);
      assert!(first.is_worth_a_retry(&ErrorCode::UseDpopNonce));
      // The nonce it already used: a retry could only get the same answer. This is what ends it.
      let second = prover.note_answer(with_nonce);
      assert!(!second.is_worth_a_retry(&ErrorCode::UseDpopNonce));
      // A proxy that strips the header: nothing to retry with.
      let stripped = prover.note_answer(DpopAnswer::default());
      assert!(!stripped.is_worth_a_retry(&ErrorCode::UseDpopNonce));
    }

    #[test]
    fn an_invalid_proof_is_retried_only_when_the_clock_was_off() {
      let prover = prover();
      let on_time = prover.note_answer(DpopAnswer {
        maybe_nonce: None,
        maybe_date: Some(&http_date(now_unix())),
      });
      assert!(!on_time.is_worth_a_retry(&ErrorCode::InvalidDpopProof));
      let off = prover.note_answer(DpopAnswer {
        maybe_nonce: None,
        maybe_date: Some(&http_date(now_unix() + 600)),
      });
      assert!(off.is_worth_a_retry(&ErrorCode::InvalidDpopProof));
      let corrected = prover.note_answer(DpopAnswer {
        maybe_nonce: None,
        maybe_date: Some(&http_date(now_unix() + 600)),
      });
      assert!(!corrected.is_worth_a_retry(&ErrorCode::InvalidDpopProof));
    }

    #[tokio::test]
    async fn the_token_endpoint_gets_one_more_try_and_never_a_third() {
      let prover = prover();
      let attempts = std::cell::Cell::new(0);
      let body: Result<u16, DpopProofError> =
        with_token_endpoint_proof(&prover, "https://sso.example.com/token", |_proof| {
          attempts.set(attempts.get() + 1);
          let nonce = format!("nonce-{}", attempts.get());
          std::future::ready(Ok(TokenEndpointAttempt {
            status: 400,
            body: br#"{"error":"use_dpop_nonce"}"#.to_vec(),
            maybe_nonce: Some(nonce),
            maybe_date: None,
            response: 400,
          }))
        })
        .await;
      assert_eq!(body, Ok(400));
      assert_eq!(attempts.get(), 2);
    }

    #[tokio::test]
    async fn a_token_endpoint_that_says_yes_is_asked_once() {
      let prover = prover();
      let attempts = std::cell::Cell::new(0);
      let body: Result<u16, DpopProofError> =
        with_token_endpoint_proof(&prover, "https://sso.example.com/token", |proof| {
          assert_eq!(proof.split('.').count(), 3);
          attempts.set(attempts.get() + 1);
          std::future::ready(Ok(TokenEndpointAttempt {
            status: 200,
            body: b"{}".to_vec(),
            maybe_nonce: None,
            maybe_date: None,
            response: 200,
          }))
        })
        .await;
      assert_eq!(body, Ok(200));
      assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn nothing_else_is_retried() {
      let learned = Learned {
        has_new_nonce: true,
        has_clock_moved: true,
      };
      for code in [
        ErrorCode::DpopRequired,
        ErrorCode::InvalidToken,
        ErrorCode::Unauthorized,
      ] {
        assert!(!learned.is_worth_a_retry(&code), "{code}");
      }
    }

    #[test]
    fn a_hostile_nonce_is_not_kept() {
      let prover = prover();
      assert!(!prover.remember_nonce(""));
      assert!(!prover.remember_nonce(&"a".repeat(MAX_NONCE_BYTES + 1)));
      assert!(!prover.has_nonce());
    }

    #[test]
    fn the_token_endpoint_says_so_in_its_body() {
      let nonce = br#"{"error":"use_dpop_nonce"}"#;
      assert_eq!(token_endpoint_error(400, nonce), Some(ErrorCode::UseDpopNonce));
      assert_eq!(token_endpoint_error(401, nonce), Some(ErrorCode::UseDpopNonce));
      assert_eq!(token_endpoint_error(200, nonce), None);
      assert_eq!(
        token_endpoint_error(400, br#"{"error":"invalid_dpop_proof"}"#),
        Some(ErrorCode::InvalidDpopProof)
      );
      assert_eq!(token_endpoint_error(400, br#"{"error":"invalid_grant"}"#), None);
      assert_eq!(token_endpoint_error(400, b"<html>"), None);
    }
  }

  mod authorization {
    use super::*;

    #[tokio::test]
    async fn a_bound_token_goes_with_its_proof_and_an_unbound_one_as_before() {
      let prover = prover();
      let bound = bound_token(&prover.thumbprint());
      let url = "https://chat.example.com/api/v1/me";
      let signed = signed_headers(Some(&prover), &bound, "GET", url).await.unwrap();
      assert_eq!(signed.authorization, format!("DPoP {bound}"));
      assert!(signed.maybe_proof.is_some());

      let plain = signed_headers(Some(&prover), "opaque", "GET", url).await.unwrap();
      assert_eq!(plain.authorization, "Bearer opaque");
      assert!(plain.maybe_proof.is_none());
    }

    #[tokio::test]
    async fn a_token_bound_to_another_key_is_a_lost_key() {
      let bound = bound_token(&prover().thumbprint());
      let url = "https://chat.example.com/api/v1/me";
      let refused = signed_headers(Some(&prover()), &bound, "GET", url).await;
      assert_eq!(refused.err(), Some(DpopSendError::KeyLost));
    }

    #[tokio::test]
    async fn without_a_key_a_bound_token_goes_as_a_bearer_token() {
      let bound = bound_token(&prover().thumbprint());
      let url = "https://chat.example.com/api/v1/me";
      let plain = signed_headers(None, &bound, "GET", url).await.unwrap();
      assert_eq!(plain.authorization, format!("Bearer {bound}"));
    }

    fn bound_token(jkt: &JwkThumbprint) -> String {
      let claims = format!(r#"{{"cnf":{{"jkt":"{}"}}}}"#, jkt.as_str());
      format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(claims))
    }

    #[test]
    fn a_token_that_names_a_key_is_bound_and_others_are_bearer() {
      let key = DpopSigningKey::generate();
      let jkt = key.thumbprint();
      let claims = format!(r#"{{"typ":"DPoP","cnf":{{"jkt":"{}"}}}}"#, jkt.as_str());
      let token = format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(claims));
      assert_eq!(Authorization::of(&token), Authorization::Bound(jkt));
      assert_eq!(Authorization::of("opaque"), Authorization::Bearer);
      let unbound = format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode(r#"{"sub":"1"}"#));
      assert_eq!(Authorization::of(&unbound), Authorization::Bearer);
    }
  }

  mod dates {
    use super::*;

    #[test]
    fn known_dates_parse() {
      assert_eq!(
        parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(784_111_777)
      );
      assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
      assert_eq!(
        parse_http_date("Tue, 03 Nov 2026 12:00:00 GMT"),
        Some(1_793_707_200)
      );
      assert_eq!(
        parse_http_date("Thu, 29 Feb 2024 00:00:01 GMT"),
        Some(1_709_164_801)
      );
    }

    #[test]
    fn anything_else_says_nothing() {
      for value in [
        "",
        "yesterday",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun Nov  6 08:49:37 1994",
        "Sun, 06 Nov 1994 08:49:37 UTC",
        "Sun, 06 Nov 1994 08:49:37 GMT extra",
        "Sun, 06 Foo 1994 08:49:37 GMT",
        "Sun, 32 Nov 1994 08:49:37 GMT",
        "Sun, 06 Nov 1994 25:49:37 GMT",
        "Sun, 06 Nov 1994 08:49 GMT",
      ] {
        assert_eq!(parse_http_date(value), None, "{value}");
      }
    }
  }

  fn prover() -> DpopProver {
    DpopProver::new(Arc::new(SoftwareDpopSigner::new(DpopSigningKey::generate())))
  }

  fn claims_of(proof: &DpopProof) -> Value {
    let payload = proof.as_str().split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
  }

  /// `unix` as an IMF-fixdate.
  fn http_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    // civil_from_days, the inverse of `days_from_civil`.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
      shifted_month + 3
    } else {
      shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    const MONTHS: [&str; 12] = [
      "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
      "Mon, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
      MONTHS[(month - 1) as usize],
      seconds / 3_600,
      seconds / 60 % 60,
      seconds % 60
    )
  }
}
