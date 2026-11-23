//! Checking DPoP proofs on requests (RFC 9449; e2ee.md §3.1.2, operations.md "Binding tokens to
//! devices"). The proof's own checks are in `teamcraft_crypto::dpop`; here are the policy, the
//! server's origins, the nonces and the replay cache.
//!
//! Both live only in memory. A restart forgets which proofs were seen, and that is safe because it
//! forgets the nonces too: every proof needs a nonce, so proofs made before the restart are refused
//! with a fresh challenge and the client signs a new one. Run exactly one server (operations.md);
//! several would each refuse the others' nonces.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use teamcraft_crypto::dpop::{
  DPOP_ALGORITHM, DpopError, DpopExpectation, DpopNonces, JwkThumbprint, PublicOrigin, replay_retention_secs,
  verify_proof,
};
use teamcraft_proto::{DpopPolicy, ErrorCode, TokenBindingSettings};
use tracing::{debug, error, warn};

use super::error::AuthError;
use crate::metrics::DpopOutcome;
use crate::state::AppState;

/// The request header that carries a proof.
pub const DPOP_HEADER: HeaderName = HeaderName::from_static("dpop");
/// The response header that carries the nonce for the next proof.
pub const DPOP_NONCE_HEADER: HeaderName = HeaderName::from_static("dpop-nonce");

/// How often a full replay cache is logged at most: it stays full for a while, and refuses every
/// request with a proof meanwhile.
const FULL_CACHE_LOG_INTERVAL_SECS: i64 = 300;

/// What the server remembers about proofs. Created once at startup.
#[derive(Debug)]
pub struct DpopGuard {
  nonces: Mutex<DpopNonces>,
  replays: Mutex<ReplayCache>,
  has_warned_about_origins: AtomicBool,
  /// When a full replay cache was logged last.
  full_cache_logged_at_unix: AtomicI64,
}

/// A token as a request presents it. Its `Debug` doesn't print the token or the proof.
#[derive(Clone, Copy)]
pub struct PresentedToken<'a> {
  pub token: &'a str,
  pub scheme: TokenScheme,
  pub proof: PresentedProof<'a>,
  pub method: &'a str,
  /// The path the request came in on.
  pub path: &'a str,
}

/// How the token reached the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScheme {
  /// `Authorization: Bearer`.
  Bearer,
  /// `Authorization: DPoP`.
  Dpop,
  /// In the first frame of a WebSocket, with its proof next to it.
  Frame,
}

/// The request's proof, before anything is known about it. Its `Debug` doesn't print the proof.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PresentedProof<'a> {
  Absent,
  One(&'a str),
  /// Several `DPoP` headers, or one that isn't text (RFC 9449 §4.3 allows exactly one).
  Unusable,
}

/// What a token says about being bound to a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenBinding {
  /// A session, bot or guest token this server made itself: not from the identity provider, so a
  /// look-alike can't obtain one from an existing sign-in, and there is no key to bind it to yet.
  ServerIssued,
  Unbound,
  /// `cnf.jkt` names this key, and no proof was checked (yet).
  Bound(JwkThumbprint),
  /// This request proved that it holds the key.
  Verified(JwkThumbprint),
}

/// Why a request's token is refused although the token itself is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DpopRefusal {
  /// The proof has no or an old nonce: the normal first answer, not a failure.
  #[error("the DPoP proof needs the current nonce")]
  UseNonce,
  #[error("{0}")]
  InvalidProof(DpopError),
  #[error("the DPoP proof was used already")]
  Replayed,
  #[error("this token is bound to a key and needs a DPoP proof")]
  ProofMissing,
  #[error("a token bound to a key must use the DPoP scheme")]
  BearerScheme,
  #[error("this server requires tokens bound to a key")]
  TokenNotBound,
}

/// The proofs of bound tokens seen lately, kept per account (e2ee.md §3.1.2, "Replay and restart").
/// A proof names the token it goes with (`ath`), and a bound token names its account, so a proof can
/// only ever be replayed with that account's token: each account's proofs are compared with its own
/// only, and one account's entries say nothing about another's.
///
/// Room comes in two tiers, so that no group of accounts can fill the cache for everybody else:
///
/// - Every account has a reserve of its own (`reserved_per_owner`), enough for one person's apps.
///   Another account's flood never takes it.
/// - Above its reserve an account uses a pool shared by all (`max_shared`), up to
///   `max_entries_per_owner`. When the pool is full, the accounts above their reserve wait (429),
///   and those within it go on.
///
/// The reserves together are bounded as well (`max_reserved`): only that many accounts, all sending
/// at the rate of their reserve at the same moment, fill them, and then the cache fails closed (503).
///
/// A queue for expiry: every entry lives equally long, so the oldest is at the front, and a request
/// pays for the ones that just expired, not for all.
#[derive(Debug, Default)]
struct ReplayCache {
  seen_per_owner: HashMap<Arc<str>, HashSet<[u8; 32]>>,
  by_expiry: VecDeque<ReplayEntry>,
  reserved_in_use: usize,
  shared_in_use: usize,
}

#[derive(Debug)]
struct ReplayEntry {
  id: [u8; 32],
  /// The last moment the proof can be fresh; the entry goes after it.
  last_fresh_unix: i64,
  owner: Arc<str>,
  /// Which room the entry took, given back when it expires. Kept with the entry because the limits
  /// can change while it lives.
  tier: ReplayTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayTier {
  Reserved,
  Shared,
}

#[derive(Debug, Clone, Copy)]
struct ReplayLimits {
  retention_secs: u64,
  /// Entries all accounts share above their reserves.
  max_shared: usize,
  /// Entries in all accounts' reserves together.
  max_reserved: usize,
  reserved_per_owner: usize,
  max_entries_per_owner: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayOutcome {
  New,
  Seen,
  /// This account is past its reserve, and has as many proofs in the cache as one may or the
  /// shared pool is full: it waits, nobody else does.
  OwnerFull,
  /// The reserves of all accounts together are full.
  Full,
}

impl Default for DpopGuard {
  fn default() -> Self {
    Self {
      nonces: Mutex::new(DpopNonces::new(now_unix())),
      replays: Mutex::default(),
      has_warned_about_origins: AtomicBool::new(false),
      full_cache_logged_at_unix: AtomicI64::new(i64::MIN),
    }
  }
}

impl DpopGuard {
  /// The nonce for the next proof.
  pub fn current_nonce(&self, rules: &TokenBindingSettings) -> String {
    self.current_nonce_at(rules, now_unix())
  }

  fn current_nonce_at(&self, rules: &TokenBindingSettings, now: i64) -> String {
    self
      .nonces
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .current(now, u64::from(rules.nonce_rotation_secs))
      .to_owned()
  }

  /// Applies the policy to a request. On success `binding` says whether a proof was verified.
  fn check(
    &self,
    rules: &TokenBindingSettings,
    origins: &[PublicOrigin],
    presented: &PresentedToken<'_>,
    subject: &str,
    binding: &mut TokenBinding,
    now: i64,
  ) -> Result<Option<DpopOutcome>, AuthError> {
    // A policy this version doesn't know (a row from a newer version, see `runtime_settings::load`)
    // checks like `optional`: bound tokens stay protected, and nobody is locked out.
    if rules.dpop_policy == DpopPolicy::Off {
      return Ok(None);
    }
    let maybe_bound_key = match &*binding {
      TokenBinding::ServerIssued | TokenBinding::Verified(_) => return Ok(None),
      TokenBinding::Unbound => None,
      TokenBinding::Bound(key) => Some(key.clone()),
    };
    match maybe_bound_key {
      Some(key) => {
        match presented.proof {
          PresentedProof::Absent => return Err(DpopRefusal::ProofMissing.into()),
          _ if presented.scheme == TokenScheme::Bearer => return Err(DpopRefusal::BearerScheme.into()),
          _ => {},
        }
        let replay_id = self.check_proof(rules, origins, presented, Some(&key), now)?;
        self.remember(rules, replay_id, subject, now)?;
        *binding = TokenBinding::Verified(key);
        Ok(Some(DpopOutcome::Bound))
      },
      None => {
        if rules.dpop_policy == DpopPolicy::Required {
          return Err(DpopRefusal::TokenNotBound.into());
        }
        // A proof with an unbound token protects nothing yet, but a wrong one is a broken or a
        // hostile client either way. Only if the server can't check it, it is ignored: the token
        // would be accepted without the proof all the same. For the same reason it isn't remembered:
        // a replay could as well leave the proof out, and anybody with a token could otherwise fill
        // the cache with keys of their own making.
        if presented.proof != PresentedProof::Absent && !origins.is_empty() {
          self.check_proof(rules, origins, presented, None, now)?;
        }
        Ok(Some(DpopOutcome::Unbound))
      },
    }
  }

  fn check_proof(
    &self,
    rules: &TokenBindingSettings,
    origins: &[PublicOrigin],
    presented: &PresentedToken<'_>,
    maybe_bound_key: Option<&JwkThumbprint>,
    now: i64,
  ) -> Result<[u8; 32], AuthError> {
    let proof = match presented.proof {
      PresentedProof::One(proof) => proof,
      PresentedProof::Absent | PresentedProof::Unusable => {
        return Err(DpopRefusal::InvalidProof(DpopError::Malformed).into());
      },
    };
    let max_age_secs = u64::from(rules.proof_max_age_secs);
    let leeway_secs = u64::from(rules.proof_leeway_secs);
    let verified = verify_proof(
      proof,
      &DpopExpectation {
        method: presented.method,
        path: presented.path,
        public_origins: origins,
        now_unix: now,
        max_age_secs,
        leeway_secs,
        access_token: presented.token,
        maybe_bound_key,
      },
    )
    .map_err(DpopRefusal::InvalidProof)?;

    // Before the replay cache: a proof that only lacks the nonce isn't used up.
    let has_valid_nonce = verified.maybe_nonce.as_deref().is_some_and(|nonce| {
      self
        .nonces
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .accepts(nonce, now, u64::from(rules.nonce_rotation_secs))
    });
    if !has_valid_nonce {
      return Err(DpopRefusal::UseNonce.into());
    }
    Ok(verified.replay_id)
  }

  /// Records a valid proof of a bound token. After the signature: only somebody with a bound token
  /// and its key can make the server remember anything.
  fn remember(
    &self,
    rules: &TokenBindingSettings,
    replay_id: [u8; 32],
    subject: &str,
    now: i64,
  ) -> Result<(), AuthError> {
    let limits = ReplayLimits::from_rules(rules);
    let outcome = self
      .replays
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .record(replay_id, subject, now, limits);
    match outcome {
      ReplayOutcome::New => Ok(()),
      ReplayOutcome::Seen => Err(DpopRefusal::Replayed.into()),
      // The accounts that send this much wait, and nobody else notices.
      ReplayOutcome::OwnerFull => Err(AuthError::DpopUserLimit),
      // Fail closed: taking the request could let a replay through. Clients retry on 503. Only
      // reachable when the reserves of very many accounts are full at once.
      ReplayOutcome::Full => {
        self.log_full_cache(now);
        Err(AuthError::DpopReplayCacheFull)
      },
    }
  }

  fn log_full_cache(&self, now: i64) {
    let last = self.full_cache_logged_at_unix.load(Ordering::Relaxed);
    let is_due = now.saturating_sub(last) >= FULL_CACHE_LOG_INTERVAL_SECS;
    if is_due
      && self
        .full_cache_logged_at_unix
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
      error!(
        "the DPoP replay cache is full: requests with bound tokens get 503 until entries expire; \
         see replay_cache_max_entries"
      );
    }
  }

  fn warn_once_about_missing_origins(&self) {
    if !self.has_warned_about_origins.swap(true, Ordering::Relaxed) {
      warn!(
        "a DPoP proof arrived, but the server does not know its public origin: set \
         security.public_origins, or bound tokens are refused"
      );
    }
  }
}

impl<'a> PresentedProof<'a> {
  /// The proof of an HTTP request.
  pub fn from_headers(headers: &'a HeaderMap) -> Self {
    let mut values = headers.get_all(DPOP_HEADER).iter();
    match (values.next(), values.next()) {
      (None, _) => Self::Absent,
      (Some(value), None) => value.to_str().map_or(Self::Unusable, Self::One),
      (Some(_), Some(_)) => Self::Unusable,
    }
  }

  /// The proof in a WebSocket's first frame.
  pub fn from_frame(maybe_proof: Option<&'a str>) -> Self {
    maybe_proof.map_or(Self::Absent, Self::One)
  }
}

impl std::fmt::Debug for PresentedToken<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PresentedToken")
      .field("token", &"[redacted]")
      .field("scheme", &self.scheme)
      .field("proof", &self.proof)
      .field("method", &self.method)
      .field("path", &self.path)
      .finish()
  }
}

impl std::fmt::Debug for PresentedProof<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self {
      Self::Absent => "Absent",
      Self::One(_) => "One([redacted])",
      Self::Unusable => "Unusable",
    })
  }
}

impl TokenBinding {
  pub fn is_verified(&self) -> bool {
    matches!(self, Self::Verified(_))
  }
}

impl DpopRefusal {
  pub fn error_code(self) -> ErrorCode {
    match self {
      Self::UseNonce => ErrorCode::UseDpopNonce,
      Self::InvalidProof(_) | Self::Replayed => ErrorCode::InvalidDpopProof,
      Self::ProofMissing | Self::BearerScheme | Self::TokenNotBound => ErrorCode::DpopRequired,
    }
  }

  /// The `WWW-Authenticate` value (RFC 9449 §7.1 and §9).
  pub fn challenge(self) -> HeaderValue {
    let error = match self {
      Self::UseNonce => "use_dpop_nonce",
      Self::InvalidProof(_) | Self::Replayed => "invalid_dpop_proof",
      Self::ProofMissing | Self::BearerScheme | Self::TokenNotBound => "invalid_token",
    };
    // The descriptions are fixed texts without quotes, see `DpopError`.
    let value = format!(r#"DPoP error="{error}", error_description="{self}", algs="{DPOP_ALGORITHM}""#);
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("DPoP"))
  }
}

impl ReplayLimits {
  fn from_rules(rules: &TokenBindingSettings) -> Self {
    let max_shared = rules.replay_cache_max_entries as usize;
    Self {
      retention_secs: replay_retention_secs(
        u64::from(rules.proof_max_age_secs),
        u64::from(rules.proof_leeway_secs),
      ),
      max_shared,
      // As much again as the pool: the cache holds at most twice `replay_cache_max_entries`.
      max_reserved: max_shared,
      reserved_per_owner: rules.replay_cache_reserved_per_user as usize,
      max_entries_per_owner: rules.replay_cache_max_entries_per_user as usize,
    }
  }
}

impl ReplayCache {
  /// Remembers a proof, unless it was seen or there is no room. No eviction of live entries: that
  /// would let exactly those proofs be replayed.
  fn record(&mut self, id: [u8; 32], owner: &str, now: i64, limits: ReplayLimits) -> ReplayOutcome {
    self.forget_expired(now);
    let maybe_seen = self.seen_per_owner.get(owner);
    if maybe_seen.is_some_and(|seen| seen.contains(&id)) {
      return ReplayOutcome::Seen;
    }
    let owner_entries = maybe_seen.map_or(0, HashSet::len);
    let Some(tier) = self.room_for(owner_entries, limits) else {
      return if owner_entries < limits.reserved_per_owner {
        ReplayOutcome::Full
      } else {
        ReplayOutcome::OwnerFull
      };
    };
    match tier {
      ReplayTier::Reserved => self.reserved_in_use += 1,
      ReplayTier::Shared => self.shared_in_use += 1,
    }
    let owner = self
      .seen_per_owner
      .get_key_value(owner)
      .map_or_else(|| Arc::from(owner), |(known, _)| Arc::clone(known));
    self
      .seen_per_owner
      .entry(Arc::clone(&owner))
      .or_default()
      .insert(id);
    let retention = i64::try_from(limits.retention_secs).unwrap_or(i64::MAX);
    self.by_expiry.push_back(ReplayEntry {
      id,
      last_fresh_unix: now.saturating_add(retention),
      owner,
      tier,
    });
    ReplayOutcome::New
  }

  /// Where a new entry of an account with `owner_entries` goes, or `None` if there is no room.
  fn room_for(&self, owner_entries: usize, limits: ReplayLimits) -> Option<ReplayTier> {
    if owner_entries >= limits.max_entries_per_owner {
      return None;
    }
    if owner_entries < limits.reserved_per_owner {
      return (self.reserved_in_use < limits.max_reserved).then_some(ReplayTier::Reserved);
    }
    (self.shared_in_use < limits.max_shared).then_some(ReplayTier::Shared)
  }

  fn forget_expired(&mut self, now: i64) {
    while self
      .by_expiry
      .front()
      .is_some_and(|entry| entry.last_fresh_unix < now)
    {
      let Some(entry) = self.by_expiry.pop_front() else {
        return;
      };
      match entry.tier {
        ReplayTier::Reserved => self.reserved_in_use = self.reserved_in_use.saturating_sub(1),
        ReplayTier::Shared => self.shared_in_use = self.shared_in_use.saturating_sub(1),
      }
      if let Some(seen) = self.seen_per_owner.get_mut(&entry.owner) {
        seen.remove(&entry.id);
        if seen.is_empty() {
          self.seen_per_owner.remove(&entry.owner);
        }
      }
    }
  }

  #[cfg(test)]
  fn len(&self) -> usize {
    self.by_expiry.len()
  }
}

/// Applies the policy to a request with a valid token.
pub fn enforce(
  state: &AppState,
  presented: &PresentedToken<'_>,
  subject: &str,
  binding: &mut TokenBinding,
) -> Result<(), AuthError> {
  let settings = state.settings.get();
  let rules = &settings.token_binding;
  if rules.dpop_policy == DpopPolicy::Off || *binding == TokenBinding::ServerIssued {
    return Ok(());
  }
  let origins = if presented.proof == PresentedProof::Absent {
    Vec::new()
  } else {
    public_origins(state)
  };
  if presented.proof != PresentedProof::Absent && origins.is_empty() {
    state.dpop.warn_once_about_missing_origins();
  }
  let result = state
    .dpop
    .check(rules, &origins, presented, subject, binding, now_unix());
  match &result {
    Ok(Some(outcome)) => state.metrics.note_dpop(*outcome),
    Ok(None) => {},
    Err(AuthError::Dpop(DpopRefusal::UseNonce)) => {},
    Err(error) => {
      state.metrics.note_dpop(DpopOutcome::Refused);
      debug!(%error, "request refused by DPoP");
    },
  }
  result.map(|_| ())
}

/// Tells a client that sent a proof which nonce to use next, on every answer (RFC 9449 §8.2), so
/// that a rotation normally costs no round trip.
pub fn note_nonce_header(state: &AppState, headers: &mut HeaderMap) {
  let settings = state.settings.get();
  if settings.token_binding.dpop_policy == DpopPolicy::Off {
    return;
  }
  let nonce = state.dpop.current_nonce(&settings.token_binding);
  if let Ok(value) = HeaderValue::from_str(&nonce) {
    headers.insert(DPOP_NONCE_HEADER, value);
  }
}

/// The origins a proof may name. Only what the operator configured: never `Host` or a forwarded
/// header, which whoever sends the request controls. A few short lists, read only for requests with
/// a proof.
pub fn public_origins(state: &AppState) -> Vec<PublicOrigin> {
  let settings = state.settings.get();
  let registry = state.brands.current();
  let hosts = registry
    .served_hostnames()
    .into_iter()
    .map(|(host, _)| crate::brand::origin_of(&host));
  let meetings = state
    .maybe_meetings
    .iter()
    .map(|meetings| meetings.base_url().to_owned());
  let mut origins: Vec<PublicOrigin> = settings
    .security
    .public_origins
    .iter()
    .cloned()
    .chain(hosts)
    .chain(meetings)
    .chain([
      settings.mail.public_base_url.clone(),
      settings.shares.public_base_url.clone(),
    ])
    .filter_map(|origin| PublicOrigin::parse(&origin).ok())
    .collect();
  origins.dedup();
  origins
}

fn now_unix() -> i64 {
  time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
  use std::sync::LazyLock;

  use axum::http::header::WWW_AUTHENTICATE;
  use axum::response::IntoResponse;
  use teamcraft_crypto::dpop::{DpopProofRequest, DpopSigningKey};

  use super::*;
  use crate::error::{ApiError, DpopNonceChallenge, DpopProofRefusal};

  const NOW: i64 = 1_800_000_000;
  const TOKEN: &str = "an.access.token";
  const ORIGIN: &str = "https://chat.example.com";
  const PATH: &str = "/api/v1/me";
  const SUBJECT: &str = "user-1";

  static ORIGINS: LazyLock<Vec<PublicOrigin>> = LazyLock::new(|| vec![PublicOrigin::parse(ORIGIN).unwrap()]);

  mod policy {
    use super::*;

    #[test]
    fn off_checks_nothing_and_verifies_nothing() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let mut binding = TokenBinding::Bound(key.thumbprint());
      let outcome = guard.check(
        &rules(DpopPolicy::Off),
        &ORIGINS,
        &presented(TokenScheme::Bearer, PresentedProof::Absent),
        SUBJECT,
        &mut binding,
        NOW,
      );
      assert!(matches!(outcome, Ok(None)));
      assert!(!binding.is_verified());
    }

    #[test]
    fn optional_takes_old_clients_and_required_refuses_them() {
      let guard = guard();
      let request = presented(TokenScheme::Bearer, PresentedProof::Absent);
      let mut binding = TokenBinding::Unbound;
      assert!(matches!(
        guard.check(
          &rules(DpopPolicy::Optional),
          &ORIGINS,
          &request,
          SUBJECT,
          &mut binding,
          NOW
        ),
        Ok(Some(DpopOutcome::Unbound))
      ));
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Required),
          &ORIGINS,
          &request,
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::TokenNotBound)
      );
    }

    #[test]
    fn a_bound_token_without_proof_is_refused_under_every_checking_policy() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      for policy in [DpopPolicy::Optional, DpopPolicy::Required] {
        for scheme in [TokenScheme::Bearer, TokenScheme::Dpop, TokenScheme::Frame] {
          let mut binding = TokenBinding::Bound(key.thumbprint());
          assert_eq!(
            refusal(guard.check(
              &rules(policy.clone()),
              &ORIGINS,
              &presented(scheme, PresentedProof::Absent),
              SUBJECT,
              &mut binding,
              NOW
            )),
            Some(DpopRefusal::ProofMissing)
          );
          assert!(!binding.is_verified());
        }
      }
    }

    #[test]
    fn a_bound_token_with_the_bearer_scheme_is_refused_even_with_a_proof() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Optional),
          &ORIGINS,
          &presented(TokenScheme::Bearer, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::BearerScheme)
      );
    }

    #[test]
    fn a_valid_proof_verifies_a_bound_token_under_required() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Bound(key.thumbprint());
      let outcome = guard.check(
        &rules(DpopPolicy::Required),
        &ORIGINS,
        &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
        SUBJECT,
        &mut binding,
        NOW,
      );
      assert!(matches!(outcome, Ok(Some(DpopOutcome::Bound))));
      assert!(binding.is_verified());
    }

    #[test]
    fn a_proof_with_an_unbound_token_does_not_satisfy_required() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Unbound;
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Required),
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::TokenNotBound)
      );
    }

    #[test]
    fn a_wrong_proof_with_an_unbound_token_is_refused_under_optional() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &key, "POST", ORIGIN, NOW);
      let mut binding = TokenBinding::Unbound;
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Optional),
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::InvalidProof(DpopError::WrongMethod))
      );
    }

    /// A row from a newer version: bound tokens stay protected and nobody is locked out.
    #[test]
    fn an_unknown_policy_checks_like_optional() {
      let guard = guard();
      let settings = rules(DpopPolicy::Unknown("stricter".to_owned()));
      let key = DpopSigningKey::generate();
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &settings,
          &ORIGINS,
          &presented(TokenScheme::Bearer, PresentedProof::Absent),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::ProofMissing)
      );
      let mut binding = TokenBinding::Unbound;
      assert!(matches!(
        guard.check(
          &settings,
          &ORIGINS,
          &presented(TokenScheme::Bearer, PresentedProof::Absent),
          SUBJECT,
          &mut binding,
          NOW
        ),
        Ok(Some(DpopOutcome::Unbound))
      ));
    }

    #[test]
    fn tokens_of_this_server_are_not_part_of_it() {
      let guard = guard();
      let mut binding = TokenBinding::ServerIssued;
      assert!(matches!(
        guard.check(
          &rules(DpopPolicy::Required),
          &ORIGINS,
          &presented(TokenScheme::Bearer, PresentedProof::Absent),
          SUBJECT,
          &mut binding,
          NOW
        ),
        Ok(None)
      ));
    }

    #[test]
    fn several_proof_headers_are_refused_with_a_bound_token() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Optional),
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::Unusable),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::InvalidProof(DpopError::Malformed))
      );
    }

    #[test]
    fn the_proof_header_must_be_there_once_and_be_text() {
      let mut headers = HeaderMap::new();
      assert_eq!(PresentedProof::from_headers(&headers), PresentedProof::Absent);
      headers.insert(DPOP_HEADER, HeaderValue::from_static("proof"));
      assert_eq!(
        PresentedProof::from_headers(&headers),
        PresentedProof::One("proof")
      );
      headers.append(DPOP_HEADER, HeaderValue::from_static("other"));
      assert_eq!(PresentedProof::from_headers(&headers), PresentedProof::Unusable);
      let mut headers = HeaderMap::new();
      headers.insert(DPOP_HEADER, HeaderValue::from_bytes(b"caf\xe9").unwrap());
      assert_eq!(PresentedProof::from_headers(&headers), PresentedProof::Unusable);
    }
  }

  mod secrets {
    use super::*;

    #[test]
    fn debug_shows_neither_the_token_nor_the_proof() {
      let request = presented(TokenScheme::Dpop, PresentedProof::One("the.secret.proof"));
      let printed = format!("{request:?}");
      assert!(!printed.contains(TOKEN), "{printed}");
      assert!(!printed.contains("the.secret.proof"), "{printed}");
    }
  }

  mod theft_and_relay {
    use super::*;

    /// A thief with the token but not the key signs with a key of its own.
    #[test]
    fn replay_from_another_device_is_refused() {
      let guard = guard();
      let device = DpopSigningKey::generate();
      let thief = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &thief, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Bound(device.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Optional),
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::InvalidProof(DpopError::WrongKey))
      );
    }

    /// The device signed for the server it thought it talked to, and the look-alike passes it on.
    #[test]
    fn a_proof_made_for_a_look_alike_is_refused() {
      let guard = guard();
      let device = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &device, "GET", "https://chat.example.net", NOW);
      let mut binding = TokenBinding::Bound(device.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Required),
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::InvalidProof(DpopError::WrongUri))
      );
    }

    /// With no configured origin nothing can match, whatever the request's headers say.
    #[test]
    fn without_a_configured_origin_a_bound_token_is_refused() {
      let guard = guard();
      let device = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &device, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Bound(device.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &rules(DpopPolicy::Optional),
          &[],
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        )),
        Some(DpopRefusal::InvalidProof(DpopError::WrongUri))
      );
    }

    /// Without a configured origin an unbound token works as it would without the proof, so new
    /// clients aren't locked out of a server that was never told its origin.
    #[test]
    fn without_a_configured_origin_a_proof_with_an_unbound_token_is_ignored() {
      let guard = guard();
      let device = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &device, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Unbound;
      assert!(matches!(
        guard.check(
          &rules(DpopPolicy::Optional),
          &[],
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW
        ),
        Ok(Some(DpopOutcome::Unbound))
      ));
    }
  }

  mod nonce_and_replay {
    use super::*;

    #[test]
    fn a_proof_without_or_with_a_wrong_nonce_gets_a_challenge_and_isnt_used_up() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      for maybe_nonce in [None, Some("not-the-nonce")] {
        let proof = sign(&key, "GET", ORIGIN, NOW, maybe_nonce);
        let mut binding = TokenBinding::Bound(key.thumbprint());
        assert_eq!(
          refusal(guard.check(
            &rules(DpopPolicy::Optional),
            &ORIGINS,
            &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
            SUBJECT,
            &mut binding,
            NOW
          )),
          Some(DpopRefusal::UseNonce)
        );
      }
      assert_eq!(guard.replays.lock().unwrap().len(), 0);
    }

    #[test]
    fn a_replayed_proof_is_refused() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
      let request = presented(TokenScheme::Dpop, PresentedProof::One(&proof));
      let settings = rules(DpopPolicy::Optional);
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert!(
        guard
          .check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW)
          .is_ok()
      );
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW + 1)),
        Some(DpopRefusal::Replayed)
      );
    }

    /// A restart loses the replay cache and the nonces with it. The proof seen before the restart
    /// is not accepted again: it gets a challenge, for which only the key's holder can sign anew.
    #[test]
    fn after_a_restart_a_proof_made_before_it_gets_a_challenge() {
      let before = guard();
      let key = DpopSigningKey::generate();
      let proof = proof_with_nonce(&before, &key, "GET", ORIGIN, NOW);
      let request = presented(TokenScheme::Dpop, PresentedProof::One(&proof));
      let settings = rules(DpopPolicy::Optional);
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert!(
        before
          .check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW)
          .is_ok()
      );

      let after = guard();
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(after.check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW + 1)),
        Some(DpopRefusal::UseNonce)
      );
    }

    #[test]
    fn a_proof_is_remembered_for_as_long_as_it_is_fresh() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let settings = rules(DpopPolicy::Optional);
      let leeway = i64::from(settings.proof_leeway_secs);
      let max_age = i64::from(settings.proof_max_age_secs);
      // Dated as far ahead as allowed, so it is fresh for the longest possible time.
      let nonce = guard.current_nonce_at(&settings, NOW);
      let proof = sign(&key, "GET", ORIGIN, NOW + leeway, Some(&nonce));
      let request = presented(TokenScheme::Dpop, PresentedProof::One(&proof));
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert!(
        guard
          .check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW)
          .is_ok()
      );

      // The very last moment `iat` still lets it through.
      let last_fresh_moment = NOW + leeway + max_age + leeway;
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &settings,
          &ORIGINS,
          &request,
          SUBJECT,
          &mut binding,
          last_fresh_moment
        )),
        Some(DpopRefusal::Replayed)
      );
      // One second later it isn't fresh any more, whatever the cache knows.
      let mut binding = TokenBinding::Bound(key.thumbprint());
      assert_eq!(
        refusal(guard.check(
          &settings,
          &ORIGINS,
          &request,
          SUBJECT,
          &mut binding,
          last_fresh_moment + 1
        )),
        Some(DpopRefusal::InvalidProof(DpopError::NotFresh))
      );
    }

    #[test]
    fn full_reserves_fail_closed_until_entries_expire() {
      let limits = ReplayLimits {
        retention_secs: 10,
        max_shared: 0,
        max_reserved: 2,
        reserved_per_owner: 5,
        max_entries_per_owner: 100,
      };
      let mut cache = ReplayCache::default();
      assert_eq!(cache.record([1; 32], "a", NOW, limits), ReplayOutcome::New);
      assert_eq!(cache.record([2; 32], "b", NOW, limits), ReplayOutcome::New);
      assert_eq!(cache.record([1; 32], "a", NOW + 1, limits), ReplayOutcome::Seen);
      assert_eq!(cache.record([3; 32], "c", NOW + 1, limits), ReplayOutcome::Full);
      assert_eq!(cache.len(), 2);
      // Still there at the last moment, gone right after it.
      assert_eq!(cache.record([1; 32], "a", NOW + 10, limits), ReplayOutcome::Seen);
      assert_eq!(cache.record([3; 32], "c", NOW + 11, limits), ReplayOutcome::New);
      assert_eq!(cache.len(), 1);
      assert_eq!(cache.reserved_in_use, 1);
      assert!(!cache.seen_per_owner.contains_key("a"));
    }

    #[test]
    fn full_reserves_answer_unavailable_not_success() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let mut settings = rules(DpopPolicy::Optional);
      settings.replay_cache_max_entries = 1;
      let mut limits = ReplayLimits::from_rules(&settings);
      limits.reserved_per_owner = 1;
      guard
        .replays
        .lock()
        .unwrap()
        .record([9; 32], "somebody-else", NOW, limits);
      let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
      let mut binding = TokenBinding::Bound(key.thumbprint());
      let result = guard.check(
        &settings,
        &ORIGINS,
        &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
        SUBJECT,
        &mut binding,
        NOW,
      );
      assert!(matches!(result, Err(AuthError::DpopReplayCacheFull)));
      assert!(!binding.is_verified());
      assert_eq!(guard.full_cache_logged_at_unix.load(Ordering::Relaxed), NOW);
    }

    #[test]
    fn a_full_cache_is_logged_once_per_interval() {
      let guard = guard();
      guard.log_full_cache(NOW);
      guard.log_full_cache(NOW + FULL_CACHE_LOG_INTERVAL_SECS - 1);
      assert_eq!(guard.full_cache_logged_at_unix.load(Ordering::Relaxed), NOW);
      guard.log_full_cache(NOW + FULL_CACHE_LOG_INTERVAL_SECS);
      assert_eq!(
        guard.full_cache_logged_at_unix.load(Ordering::Relaxed),
        NOW + FULL_CACHE_LOG_INTERVAL_SECS
      );
    }

    #[test]
    fn a_clock_that_steps_back_keeps_entries_longer_not_shorter() {
      let limits = ReplayLimits {
        retention_secs: 10,
        max_shared: 10,
        max_reserved: 10,
        reserved_per_owner: 1,
        max_entries_per_owner: 10,
      };
      let mut cache = ReplayCache::default();
      assert_eq!(cache.record([1; 32], "a", NOW, limits), ReplayOutcome::New);
      assert_eq!(
        cache.record([1; 32], "a", NOW - 1000, limits),
        ReplayOutcome::Seen
      );
    }

    /// A member with any token and keys of their own making could otherwise fill the cache for
    /// everybody. Such a proof can be left out altogether, so remembering it protects nothing.
    #[test]
    fn proofs_with_unbound_tokens_are_not_remembered() {
      let guard = guard();
      let settings = rules(DpopPolicy::Optional);
      for _ in 0..3 {
        let own_key = DpopSigningKey::generate();
        let proof = proof_with_nonce(&guard, &own_key, "GET", ORIGIN, NOW);
        let request = presented(TokenScheme::Dpop, PresentedProof::One(&proof));
        for _ in 0..2 {
          let mut binding = TokenBinding::Unbound;
          assert!(matches!(
            guard.check(&settings, &ORIGINS, &request, SUBJECT, &mut binding, NOW),
            Ok(Some(DpopOutcome::Unbound))
          ));
        }
      }
      assert_eq!(guard.replays.lock().unwrap().len(), 0);
    }

    #[test]
    fn one_user_at_the_limit_waits_and_others_go_on() {
      let guard = guard();
      let mut settings = rules(DpopPolicy::Optional);
      settings.replay_cache_max_entries_per_user = 1;
      settings.replay_cache_reserved_per_user = 1;
      let check = |key: &DpopSigningKey, subject: &str, now: i64| {
        let proof = proof_with_nonce(&guard, key, "GET", ORIGIN, now);
        let mut binding = TokenBinding::Bound(key.thumbprint());
        guard.check(
          &settings,
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          subject,
          &mut binding,
          now,
        )
      };
      let first_key = DpopSigningKey::generate();
      let second_key = DpopSigningKey::generate();
      assert!(check(&first_key, "greedy", NOW).is_ok());
      // Another key doesn't help: the limit is per account.
      assert!(matches!(
        check(&second_key, "greedy", NOW),
        Err(AuthError::DpopUserLimit)
      ));
      assert!(check(&second_key, "patient", NOW).is_ok());
      // Room again once the entry expired, and the count is gone with it.
      let later = NOW
        + i64::try_from(replay_retention_secs(
          u64::from(settings.proof_max_age_secs),
          u64::from(settings.proof_leeway_secs),
        ))
        .unwrap()
        + 1;
      assert!(check(&first_key, "greedy", later).is_ok());
      assert!(
        !guard
          .replays
          .lock()
          .unwrap()
          .seen_per_owner
          .contains_key("patient")
      );
    }
  }

  /// Several accounts with bound tokens working together try to fill the cache, so that everybody
  /// else's requests are refused (#120, review of #1184).
  mod flood {
    use super::*;

    /// At the defaults, 20 accounts at their own limit used to fill the whole cache and get 503 for
    /// everybody bound. Now 25 fill the shared pool and only get 429 themselves.
    #[test]
    fn accounts_at_their_limit_leave_everybody_else_untouched() {
      let limits = ReplayLimits::from_rules(&TokenBindingSettings::default());
      let mut cache = ReplayCache::default();
      let mut next_id = 0_u64;
      let mut flood_until_refused = |cache: &mut ReplayCache, owner: &str| loop {
        next_id += 1;
        match cache.record(id(next_id), owner, NOW, limits) {
          ReplayOutcome::New => {},
          outcome => return outcome,
        }
      };
      for flooder in 0..25 {
        assert_eq!(
          flood_until_refused(&mut cache, &format!("flooder-{flooder}")),
          ReplayOutcome::OwnerFull
        );
      }
      assert_eq!(cache.shared_in_use, limits.max_shared);

      // Everybody else still has a reserve of their own, and the flooders don't get more.
      for honest in 0..100 {
        for request in 0..limits.reserved_per_owner {
          let unique = u64::try_from(honest * 1_000_000 + request).unwrap() + u64::MAX / 2;
          assert_eq!(
            cache.record(id(unique), &format!("member-{honest}"), NOW + 1, limits),
            ReplayOutcome::New
          );
        }
      }
      assert_eq!(
        flood_until_refused(&mut cache, "flooder-0"),
        ReplayOutcome::OwnerFull
      );
    }

    /// More accounts than the default ever let fill the cache: each takes only its reserve and the
    /// pool, and an account that has sent little gets in.
    #[test]
    fn many_flooding_accounts_still_leave_room_for_a_quiet_one() {
      let limits = ReplayLimits {
        retention_secs: 180,
        max_shared: 1000,
        max_reserved: 1000,
        reserved_per_owner: 10,
        max_entries_per_owner: 500,
      };
      let mut cache = ReplayCache::default();
      let mut next_id = 0_u64;
      for flooder in 0..90 {
        for _ in 0..1000 {
          next_id += 1;
          cache.record(id(next_id), &format!("flooder-{flooder}"), NOW, limits);
        }
      }
      assert_eq!(cache.shared_in_use, 1000);
      assert_eq!(cache.reserved_in_use, 900);
      assert_eq!(cache.len(), 1900);
      next_id += 1;
      assert_eq!(
        cache.record(id(next_id), "quiet", NOW, limits),
        ReplayOutcome::New
      );
    }

    /// Only when the reserves of all accounts together are full does the cache fail closed. At the
    /// defaults that takes some 780 accounts, each sending more than one request a second for
    /// three minutes.
    #[test]
    fn only_full_reserves_of_very_many_accounts_fail_closed() {
      let limits = ReplayLimits::from_rules(&TokenBindingSettings::default());
      assert_eq!(limits.max_reserved / limits.reserved_per_owner, 781);
      let small = ReplayLimits {
        max_reserved: 30,
        reserved_per_owner: 10,
        ..limits
      };
      let mut cache = ReplayCache::default();
      let mut next_id = 0_u64;
      for account in 0..3 {
        for _ in 0..10 {
          next_id += 1;
          assert_eq!(
            cache.record(id(next_id), &format!("a-{account}"), NOW, small),
            ReplayOutcome::New
          );
        }
      }
      next_id += 1;
      assert_eq!(cache.record(id(next_id), "a-3", NOW, small), ReplayOutcome::Full);
    }

    /// Through the guard, with real proofs: the flooding account gets 429, another gets in.
    #[test]
    fn the_flooding_account_waits_and_another_goes_on() {
      let guard = guard();
      let mut settings = rules(DpopPolicy::Required);
      settings.replay_cache_max_entries = 1000;
      settings.replay_cache_max_entries_per_user = 2000;
      settings.replay_cache_reserved_per_user = 2;
      let limits = ReplayLimits::from_rules(&settings);
      {
        let mut cache = guard.replays.lock().unwrap();
        for n in 0..1100 {
          cache.record(id(n), "flooder", NOW, limits);
        }
        // Reserve and the whole pool: 2 + 1000 entries, one more is refused.
        assert_eq!(cache.len(), 1002);
      }
      let check = |subject: &str| {
        let key = DpopSigningKey::generate();
        let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW);
        let mut binding = TokenBinding::Bound(key.thumbprint());
        guard.check(
          &settings,
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          subject,
          &mut binding,
          NOW,
        )
      };
      assert!(matches!(check("flooder"), Err(AuthError::DpopUserLimit)));
      assert!(matches!(check("member"), Ok(Some(DpopOutcome::Bound))));
      assert!(matches!(check("member"), Ok(Some(DpopOutcome::Bound))));
      // Past its reserve, while the pool is full, the member waits too: the pool is for bursts.
      assert!(matches!(check("member"), Err(AuthError::DpopUserLimit)));
      assert_eq!(guard.full_cache_logged_at_unix.load(Ordering::Relaxed), i64::MIN);
    }

    /// A proof is only compared with the proofs of the same account; a proof of one account counts
    /// as a replay for that account only.
    #[test]
    fn a_replay_is_found_in_the_account_it_belongs_to() {
      let limits = ReplayLimits::from_rules(&TokenBindingSettings::default());
      let mut cache = ReplayCache::default();
      assert_eq!(cache.record(id(1), "a", NOW, limits), ReplayOutcome::New);
      assert_eq!(cache.record(id(1), "a", NOW, limits), ReplayOutcome::Seen);
    }

    /// The limits are settings and can change while entries live: each entry gives back the room it
    /// took, so the counts never drift.
    #[test]
    fn changed_limits_give_back_the_room_each_entry_took() {
      let first = ReplayLimits {
        retention_secs: 10,
        max_shared: 100,
        max_reserved: 100,
        reserved_per_owner: 1,
        max_entries_per_owner: 100,
      };
      let second = ReplayLimits {
        reserved_per_owner: 50,
        ..first
      };
      let mut cache = ReplayCache::default();
      for n in 0..5 {
        cache.record(id(n), "a", NOW, first);
      }
      for n in 5..10 {
        cache.record(id(n), "a", NOW + 1, second);
      }
      // One in the first reserve, four in the pool, then five in the raised reserve.
      assert_eq!((cache.reserved_in_use, cache.shared_in_use), (6, 4));
      cache.forget_expired(NOW + 100);
      assert_eq!((cache.reserved_in_use, cache.shared_in_use), (0, 0));
      assert!(cache.seen_per_owner.is_empty());
    }

    fn id(n: u64) -> [u8; 32] {
      let mut id = [0; 32];
      id[..8].copy_from_slice(&n.to_be_bytes());
      id
    }
  }

  mod clock_skew {
    use super::*;

    #[test]
    fn a_client_clock_within_the_leeway_is_fine_and_beyond_it_is_refused() {
      let guard = guard();
      let key = DpopSigningKey::generate();
      let settings = rules(DpopPolicy::Optional);
      let leeway = i64::from(settings.proof_leeway_secs);
      let max_age = i64::from(settings.proof_max_age_secs);
      for (skew, is_accepted) in [
        (leeway, true),
        (leeway + 1, false),
        (-(max_age + leeway), true),
        (-(max_age + leeway + 1), false),
      ] {
        let proof = proof_with_nonce(&guard, &key, "GET", ORIGIN, NOW + skew);
        let mut binding = TokenBinding::Bound(key.thumbprint());
        let result = guard.check(
          &settings,
          &ORIGINS,
          &presented(TokenScheme::Dpop, PresentedProof::One(&proof)),
          SUBJECT,
          &mut binding,
          NOW,
        );
        if is_accepted {
          assert!(result.is_ok(), "{skew}");
        } else {
          assert_eq!(
            refusal(result),
            Some(DpopRefusal::InvalidProof(DpopError::NotFresh)),
            "{skew}"
          );
        }
      }
    }
  }

  mod answers {
    use super::*;

    #[test]
    fn every_refusal_is_a_401_with_a_dpop_challenge() {
      for (refusal, code, error) in [
        (DpopRefusal::UseNonce, "use_dpop_nonce", "use_dpop_nonce"),
        (
          DpopRefusal::InvalidProof(DpopError::WrongUri),
          "invalid_dpop_proof",
          "invalid_dpop_proof",
        ),
        (DpopRefusal::Replayed, "invalid_dpop_proof", "invalid_dpop_proof"),
        (DpopRefusal::ProofMissing, "dpop_required", "invalid_token"),
        (DpopRefusal::BearerScheme, "dpop_required", "invalid_token"),
        (DpopRefusal::TokenNotBound, "dpop_required", "invalid_token"),
      ] {
        let api_error = ApiError::from(AuthError::Dpop(refusal));
        assert_eq!(api_error.error_code().as_str(), code);
        let response = api_error.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
        let challenge = response.headers()[WWW_AUTHENTICATE].to_str().unwrap().to_owned();
        assert!(
          challenge.starts_with(&format!(r#"DPoP error="{error}""#)),
          "{challenge}"
        );
        assert!(challenge.contains(r#"algs="ES256""#), "{challenge}");
      }
    }

    #[test]
    fn the_nonce_challenge_and_refused_proofs_are_marked_as_not_a_failure() {
      let challenge = ApiError::from(AuthError::Dpop(DpopRefusal::UseNonce)).into_response();
      assert!(challenge.extensions().get::<DpopNonceChallenge>().is_some());
      assert!(challenge.extensions().get::<DpopProofRefusal>().is_some());
      let refused = ApiError::from(AuthError::Dpop(DpopRefusal::Replayed)).into_response();
      assert!(refused.extensions().get::<DpopNonceChallenge>().is_none());
      assert!(refused.extensions().get::<DpopProofRefusal>().is_some());
      let guessed = ApiError::from(AuthError::MissingToken).into_response();
      assert!(guessed.extensions().get::<DpopProofRefusal>().is_none());
    }
  }

  fn guard() -> DpopGuard {
    DpopGuard {
      nonces: Mutex::new(DpopNonces::new(NOW)),
      replays: Mutex::default(),
      has_warned_about_origins: AtomicBool::new(false),
      full_cache_logged_at_unix: AtomicI64::new(i64::MIN),
    }
  }

  fn rules(policy: DpopPolicy) -> TokenBindingSettings {
    TokenBindingSettings {
      dpop_policy: policy,
      ..TokenBindingSettings::default()
    }
  }

  fn presented<'a>(scheme: TokenScheme, proof: PresentedProof<'a>) -> PresentedToken<'a> {
    PresentedToken {
      token: TOKEN,
      scheme,
      proof,
      method: "GET",
      path: PATH,
    }
  }

  fn refusal(result: Result<Option<DpopOutcome>, AuthError>) -> Option<DpopRefusal> {
    match result {
      Err(AuthError::Dpop(refusal)) => Some(refusal),
      _ => None,
    }
  }

  fn proof_with_nonce(
    guard: &DpopGuard,
    key: &DpopSigningKey,
    method: &str,
    origin: &str,
    iat: i64,
  ) -> String {
    let nonce = guard.current_nonce_at(&TokenBindingSettings::default(), NOW);
    sign(key, method, origin, iat, Some(&nonce))
  }

  fn sign(key: &DpopSigningKey, method: &str, origin: &str, iat: i64, maybe_nonce: Option<&str>) -> String {
    key
      .sign_proof(&DpopProofRequest {
        method,
        target_uri: &format!("{origin}{PATH}"),
        issued_at_unix: iat,
        maybe_access_token: Some(TOKEN),
        maybe_nonce,
      })
      .unwrap()
      .as_str()
      .to_owned()
  }
}
