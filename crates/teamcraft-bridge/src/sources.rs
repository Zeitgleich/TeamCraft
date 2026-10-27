//! The sources the endpoints accept, shared between the endpoints and the worker.
//!
//! Two kinds live side by side:
//!
//! - **Managed** sources come from the app (docs/bridge.md, "Sources"): a workspace admin announces
//!   one with an encrypted control message in the channel, the worker reads it from the bot's store
//!   and replaces the managed set when something changed. Only the token's SHA-256 is known.
//! - **Static** sources are the optional `[[sources]]` of the configuration, for a bridge whose
//!   operator prefers files. Their tokens are hashed at start and looked up the same way.
//!
//! A request's token is hashed and compared with every known hash in constant time.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use teamcraft_core::integration_sources::ActiveSource;
use teamcraft_crypto::source_token::{find_source_token, source_token_sha256};
use teamcraft_proto::{ConversationId, SourceId, SourceKind};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::batch::Mentions;
use crate::config::{BridgeConfig, SourceConfig};
use crate::limiter::RateLimiter;
use crate::render::LogRules;

/// A producer as the endpoints and the worker see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
  pub key: SourceKey,
  /// Shown in logs and in the "dropped" notices.
  pub name: String,
  pub conversation: ConversationId,
  pub accepts: Accepts,
  pub rules: LogRules,
  pub danger_is_alert: bool,
  pub rate_limit_per_minute: u32,
  /// Files one webhook message may carry; `None`: the bridge's default (`files.max_files_per_message`).
  pub maybe_max_files_per_message: Option<u32>,
  pub mentions: Mentions,
  token_sha256: [u8; 32],
}

/// Names a source across replacements of the managed set, so its rate limit carries over.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceKey {
  /// A `[[sources]]` block, by its name (unique in the file).
  Static(String),
  Managed(SourceId),
}

/// The endpoints a source's token opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepts {
  /// Static sources: both, as before managed sources.
  Both,
  Webhooks,
  Logs,
}

/// Which endpoint a request came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
  Webhook,
  Ingest,
}

/// The shared table; cloning shares it.
#[derive(Clone)]
pub struct SourceRegistry {
  inner: Arc<RwLock<Registry>>,
}

struct Registry {
  statics: Vec<Arc<Source>>,
  managed: Vec<Arc<Source>>,
  limiters: HashMap<SourceKey, RateLimiter>,
  /// All sources together (`listen.total_rate_limit_per_minute`), so many sources can't add up
  /// to more than the bridge and the channels should take.
  total: RateLimiter,
  /// Bytes of webhooks with files, all sources together (`files.total_bytes_per_minute`).
  file_bytes: RateLimiter,
  /// Per source: its bytes of webhooks with files and its requests with files running.
  source_files: HashMap<SourceKey, SourceFiles>,
  source_bytes_per_minute: u64,
  max_requests_per_source: usize,
  max_file_request_bytes: u64,
}

/// A source's share of the bridge for webhooks with files, so one token can't take it all.
struct SourceFiles {
  /// `files.source_bytes_per_minute`.
  bytes: RateLimiter,
  /// `files.max_concurrent_requests_per_source`.
  requests: Arc<Semaphore>,
}

/// Whether the byte budgets for files had room ([`SourceRegistry::try_take_file_bytes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileBytes {
  Taken,
  /// Not now; there is room again after this long.
  Wait(Duration),
}

/// Bytes a request with files took from the budgets. They are given back when it is dropped before
/// [`Self::keep`]: a request that was refused, or whose producer hung up before its upload began,
/// costs nothing.
pub struct FileCharge {
  registry: SourceRegistry,
  source: Arc<Source>,
  bytes: u64,
  is_kept: bool,
}

/// What [`SourceRegistry::replace_managed`] changed, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ManagedChange {
  pub added: Vec<String>,
  pub removed: Vec<String>,
}

impl Source {
  pub fn from_config(config: &SourceConfig) -> Self {
    Self {
      key: SourceKey::Static(config.name.clone()),
      name: config.name.clone(),
      conversation: config.conversation_id,
      accepts: Accepts::Both,
      rules: config.log_rules(),
      danger_is_alert: config.danger_is_alert,
      rate_limit_per_minute: config.rate_limit_per_minute,
      maybe_max_files_per_message: config.maybe_max_files_per_message,
      mentions: config.mentions(),
      token_sha256: source_token_sha256(config.token.expose_secret()),
    }
  }

  /// A source announced in the app; `None` for a kind this version doesn't serve.
  pub fn from_managed(source: &ActiveSource) -> Option<Self> {
    let accepts = match source.kind {
      SourceKind::Webhook => Accepts::Webhooks,
      SourceKind::Log => Accepts::Logs,
      SourceKind::Unknown(_) => return None,
    };
    Some(Self {
      key: SourceKey::Managed(source.id),
      name: source.name.clone(),
      conversation: source.conversation,
      accepts,
      rules: LogRules {
        alert_levels: source.options.alert_levels.clone(),
      },
      danger_is_alert: source.options.danger_is_alert,
      rate_limit_per_minute: source.options.rate_limit_per_minute,
      maybe_max_files_per_message: source.options.maybe_max_files_per_message,
      mentions: Mentions::default(),
      token_sha256: *source.token_sha256.as_bytes(),
    })
  }

  fn opens(&self, endpoint: Endpoint) -> bool {
    matches!(
      (self.accepts, endpoint),
      (Accepts::Both, _) | (Accepts::Webhooks, Endpoint::Webhook) | (Accepts::Logs, Endpoint::Ingest)
    )
  }
}

impl SourceRegistry {
  /// The static sources of the configuration; managed ones arrive from the worker.
  pub fn new(config: &BridgeConfig) -> Self {
    let registry = Registry {
      statics: config
        .sources
        .iter()
        .map(|source| Arc::new(Source::from_config(source)))
        .collect(),
      managed: Vec::new(),
      limiters: HashMap::new(),
      total: RateLimiter::new(config.listen.total_rate_limit_per_minute, Instant::now()),
      file_bytes: RateLimiter::per_minute(config.files.total_bytes_per_minute, Instant::now()),
      source_files: HashMap::new(),
      source_bytes_per_minute: config.files.source_bytes_per_minute,
      max_requests_per_source: usize::try_from(config.files.max_concurrent_requests_per_source)
        .unwrap_or(usize::MAX),
      max_file_request_bytes: config
        .files
        .source_bytes_per_minute
        .min(config.files.total_bytes_per_minute),
    };
    Self {
      inner: Arc::new(RwLock::new(registry)),
    }
  }

  /// The source whose token matches and that serves `endpoint`. A token of another kind's source
  /// gets the same answer as an unknown one.
  pub fn find(&self, token: &str, endpoint: Endpoint) -> Option<Arc<Source>> {
    let inner = self.read();
    let all = inner.statics.iter().chain(&inner.managed);
    find_source_token(token, all.map(|source| (&source.token_sha256, source)))
      .filter(|source| source.opens(endpoint))
      .cloned()
  }

  /// Replaces the managed sources. Rate limits of sources that stay carry over; those of removed
  /// ones are dropped.
  pub fn replace_managed(&self, sources: Vec<Source>) -> ManagedChange {
    let mut inner = self.write();
    let mut change = ManagedChange::default();
    for old in &inner.managed {
      if !sources.iter().any(|new| new.key == old.key) {
        change.removed.push(old.name.clone());
      }
    }
    for new in &sources {
      if !inner.managed.iter().any(|old| old.key == new.key) {
        change.added.push(new.name.clone());
      }
    }
    inner.managed = sources.into_iter().map(Arc::new).collect();
    let keys: Vec<SourceKey> = inner
      .statics
      .iter()
      .chain(&inner.managed)
      .map(|source| source.key.clone())
      .collect();
    inner.limiters.retain(|key, _| keys.contains(key));
    inner.source_files.retain(|key, _| keys.contains(key));
    change
  }

  /// Whether the managed set already is `sources` (same order).
  pub fn has_managed(&self, sources: &[Source]) -> bool {
    let inner = self.read();
    inner.managed.len() == sources.len() && inner.managed.iter().zip(sources).all(|(old, new)| **old == *new)
  }

  pub fn len(&self) -> usize {
    let inner = self.read();
    inner.statics.len() + inner.managed.len()
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Takes `count` events from the source's bucket and the bridge's total, all or nothing.
  pub fn try_take(&self, source: &Source, count: u32, now: Instant) -> bool {
    self.with_limiters(source, now, |limiter, total| {
      // The total is asked first, so a refusal there costs the source nothing.
      if !total.try_take(count, now) {
        return false;
      }
      limiter.try_take(count, now)
    })
  }

  /// Takes as many of `count` events as both buckets allow.
  pub fn take_up_to(&self, source: &Source, count: u32, now: Instant) -> u32 {
    self.with_limiters(source, now, |limiter, total| {
      let allowed = total.take_up_to(count, now);
      limiter.take_up_to(allowed, now)
    })
  }

  /// How long until both buckets have room again.
  pub fn retry_after(&self, source: &Source, now: Instant) -> Duration {
    self.with_limiters(source, now, |limiter, total| {
      limiter.retry_after().max(total.retry_after())
    })
  }

  /// Takes `bytes` of a webhook with files from the source's and the bridge's byte budgets, both
  /// or neither.
  pub fn try_take_file_bytes(&self, source: &Source, bytes: u64, now: Instant) -> FileBytes {
    self.with_source_files(source, now, |files, total| {
      let source_bytes = &mut files.bytes;
      // The total is asked first, so a refusal there costs the source nothing.
      if !total.try_take_amount(bytes, now) {
        return FileBytes::Wait(total.retry_after_for(bytes));
      }
      if !source_bytes.try_take_amount(bytes, now) {
        total.refund(bytes, now);
        return FileBytes::Wait(source_bytes.retry_after_for(bytes));
      }
      FileBytes::Taken
    })
  }

  /// Gives back bytes taken for a request that didn't go through.
  pub fn refund_file_bytes(&self, source: &Source, bytes: u64, now: Instant) {
    self.with_source_files(source, now, |files, total| {
      files.bytes.refund(bytes, now);
      total.refund(bytes, now);
    });
  }

  /// The most bytes one request with files can carry: what a full minute of both budgets holds.
  pub fn max_file_request_bytes(&self) -> u64 {
    self.read().max_file_request_bytes
  }

  /// A place for one more request with files of `source`; `None` while it has as many running as
  /// it may (`files.max_concurrent_requests_per_source`). The place is freed when dropped.
  pub fn file_request_slot(&self, source: &Source) -> Option<OwnedSemaphorePermit> {
    let requests = self.with_source_files(source, Instant::now(), |files, _| files.requests.clone());
    requests.try_acquire_owned().ok()
  }

  fn with_source_files<T>(
    &self,
    source: &Source,
    now: Instant,
    run: impl FnOnce(&mut SourceFiles, &mut RateLimiter) -> T,
  ) -> T {
    let mut inner = self.write();
    let Registry {
      source_files,
      file_bytes,
      source_bytes_per_minute,
      max_requests_per_source,
      ..
    } = &mut *inner;
    let files = source_files
      .entry(source.key.clone())
      .or_insert_with(|| SourceFiles {
        bytes: RateLimiter::per_minute(*source_bytes_per_minute, now),
        requests: Arc::new(Semaphore::new(*max_requests_per_source)),
      });
    run(files, file_bytes)
  }

  fn with_limiters<T>(
    &self,
    source: &Source,
    now: Instant,
    run: impl FnOnce(&mut RateLimiter, &mut RateLimiter) -> T,
  ) -> T {
    let mut inner = self.write();
    let Registry { limiters, total, .. } = &mut *inner;
    let limiter = limiters
      .entry(source.key.clone())
      .or_insert_with(|| RateLimiter::new(source.rate_limit_per_minute, now));
    run(limiter, total)
  }

  fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
    // Every write leaves the table whole (plain assignments), so a poisoned lock is still consistent.
    self
      .inner
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
    self
      .inner
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl FileCharge {
  /// Nothing taken yet.
  pub fn new(registry: SourceRegistry, source: Arc<Source>) -> Self {
    Self {
      registry,
      source,
      bytes: 0,
      is_kept: false,
    }
  }

  /// Takes `bytes` more from the budgets.
  pub fn take(&mut self, bytes: u64, now: Instant) -> FileBytes {
    let taken = self.registry.try_take_file_bytes(&self.source, bytes, now);
    if taken == FileBytes::Taken {
      self.bytes += bytes;
    }
    taken
  }

  /// What the request took so far.
  pub fn bytes(&self) -> u64 {
    self.bytes
  }

  /// The upload began: the bytes stay taken.
  pub fn keep(&mut self) {
    self.is_kept = true;
  }
}

impl Drop for FileCharge {
  fn drop(&mut self) {
    if !self.is_kept && self.bytes > 0 {
      self
        .registry
        .refund_file_bytes(&self.source, self.bytes, Instant::now());
    }
  }
}

impl fmt::Debug for FileCharge {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FileCharge")
      .field("bytes", &self.bytes)
      .field("is_kept", &self.is_kept)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake, SourceOptions, SourceTokenHash, UserId};

  use super::*;

  const STATIC_TOKEN: &str = "static-token-0123456789";

  #[test]
  fn tokens_find_their_source_only_on_its_endpoint() {
    let registry = registry_with_static();
    registry.replace_managed(vec![managed(1, "ci", SourceKind::Webhook, "tcs_hook")]);
    registry.replace_managed(vec![
      managed(1, "ci", SourceKind::Webhook, "tcs_hook"),
      managed(2, "nas", SourceKind::Log, "tcs_logs"),
    ]);
    assert_eq!(registry.find("tcs_hook", Endpoint::Webhook).unwrap().name, "ci");
    assert!(registry.find("tcs_hook", Endpoint::Ingest).is_none());
    assert_eq!(registry.find("tcs_logs", Endpoint::Ingest).unwrap().name, "nas");
    assert!(registry.find("tcs_logs", Endpoint::Webhook).is_none());
    for endpoint in [Endpoint::Webhook, Endpoint::Ingest] {
      assert_eq!(registry.find(STATIC_TOKEN, endpoint).unwrap().name, "static");
    }
    assert!(registry.find("unknown", Endpoint::Webhook).is_none());
  }

  #[test]
  fn replacing_the_managed_set_reports_changes_and_drops_removed_limits() {
    let registry = registry_with_static();
    let change = registry.replace_managed(vec![managed(1, "ci", SourceKind::Webhook, "tcs_a")]);
    assert_eq!(change.added, ["ci"]);
    let source = registry.find("tcs_a", Endpoint::Webhook).unwrap();
    let now = Instant::now();
    assert!(registry.try_take(&source, 60, now));
    assert!(!registry.try_take(&source, 1, now), "the bucket is empty");

    // Unchanged source: its bucket carries over.
    let same = vec![managed(1, "ci", SourceKind::Webhook, "tcs_a")];
    assert!(registry.has_managed(&same));
    registry.replace_managed(same);
    assert!(!registry.try_take(&source, 1, now));

    let change = registry.replace_managed(Vec::new());
    assert_eq!(change.removed, ["ci"]);
    assert!(
      registry.find("tcs_a", Endpoint::Webhook).is_none(),
      "a revoked token stops at once"
    );
    assert_eq!(registry.len(), 1);
  }

  #[test]
  fn all_sources_together_stay_under_the_bridge_total() {
    let registry = registry_with_total(100);
    registry.replace_managed(vec![
      managed(1, "a", SourceKind::Webhook, "tcs_a"),
      managed(2, "b", SourceKind::Webhook, "tcs_b"),
    ]);
    let (a, b) = (
      registry.find("tcs_a", Endpoint::Webhook).unwrap(),
      registry.find("tcs_b", Endpoint::Webhook).unwrap(),
    );
    let now = Instant::now();
    assert_eq!(registry.take_up_to(&a, 60, now), 60);
    assert_eq!(registry.take_up_to(&b, 60, now), 40, "the total has 40 left");
    assert!(!registry.try_take(&b, 1, now));
    assert!(registry.retry_after(&b, now) >= Duration::from_millis(500));
  }

  #[test]
  fn one_source_cannot_take_the_file_bytes_of_all() {
    // 1000 file bytes a minute in all, 500 per source.
    let registry = registry_with_total(100);
    registry.replace_managed(vec![
      managed(1, "a", SourceKind::Webhook, "tcs_a"),
      managed(2, "b", SourceKind::Webhook, "tcs_b"),
    ]);
    let (a, b) = (
      registry.find("tcs_a", Endpoint::Webhook).unwrap(),
      registry.find("tcs_b", Endpoint::Webhook).unwrap(),
    );
    let now = Instant::now();
    assert_eq!(registry.max_file_request_bytes(), 500);
    assert_eq!(registry.try_take_file_bytes(&a, 500, now), FileBytes::Taken);
    assert!(matches!(
      registry.try_take_file_bytes(&a, 1, now),
      FileBytes::Wait(retry_after) if retry_after >= Duration::from_secs(1)
    ));
    assert_eq!(
      registry.try_take_file_bytes(&b, 500, now),
      FileBytes::Taken,
      "the other source still has its share"
    );
    let static_source = registry.find(STATIC_TOKEN, Endpoint::Webhook).unwrap();
    assert!(
      matches!(
        registry.try_take_file_bytes(&static_source, 1, now),
        FileBytes::Wait(_)
      ),
      "the total is used up"
    );
  }

  #[test]
  fn a_charge_dropped_before_the_upload_gives_its_bytes_back() {
    let registry = registry_with_total(100);
    let source = registry.find(STATIC_TOKEN, Endpoint::Webhook).unwrap();
    let now = Instant::now();
    let mut refused = FileCharge::new(registry.clone(), source.clone());
    assert_eq!(refused.take(400, now), FileBytes::Taken);
    assert!(matches!(refused.take(200, now), FileBytes::Wait(_)));
    assert_eq!(refused.bytes(), 400, "a refused piece is not counted");
    drop(refused);
    let mut kept = FileCharge::new(registry.clone(), source.clone());
    assert_eq!(kept.take(500, now), FileBytes::Taken, "everything was given back");
    kept.keep();
    drop(kept);
    assert!(matches!(
      registry.try_take_file_bytes(&source, 1, now),
      FileBytes::Wait(_)
    ));
  }

  #[test]
  fn each_source_runs_only_so_many_requests_with_files_at_once() {
    let registry = registry_with_static();
    let source = registry.find(STATIC_TOKEN, Endpoint::Webhook).unwrap();
    let first = registry.file_request_slot(&source).unwrap();
    let _second = registry.file_request_slot(&source).unwrap();
    assert!(registry.file_request_slot(&source).is_none(), "two by default");
    drop(first);
    assert!(registry.file_request_slot(&source).is_some());
  }

  #[test]
  fn sources_carry_their_own_file_limit() {
    let unset = managed(1, "ci", SourceKind::Webhook, "tcs_a");
    assert_eq!(unset.maybe_max_files_per_message, None, "the bridge's default");
    let limited = Source::from_managed(&ActiveSource {
      options: SourceOptions {
        maybe_max_files_per_message: Some(2),
        ..SourceOptions::default()
      },
      ..active(1, "ci", SourceKind::Webhook, "tcs_a")
    })
    .unwrap();
    assert_eq!(limited.maybe_max_files_per_message, Some(2));
  }

  fn registry_with_static() -> SourceRegistry {
    registry_with_total(100_000)
  }

  /// Requests and lines: `total` a minute; file bytes: ten times that, half of it per source.
  fn registry_with_total(total: u32) -> SourceRegistry {
    let config = BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [listen]
      total_rate_limit_per_minute = {total}
      [files]
      max_file_bytes = {source_bytes}
      source_bytes_per_minute = {source_bytes}
      total_bytes_per_minute = {file_bytes}
      [[sources]]
      name = "static"
      token = "{STATIC_TOKEN}"
      conversation_id = "1"
      "#,
      file_bytes = u64::from(total) * 10,
      source_bytes = u64::from(total) * 5
    ))
    .unwrap();
    SourceRegistry::new(&config)
  }

  fn managed(id: u8, name: &str, kind: SourceKind, token: &str) -> Source {
    Source::from_managed(&active(id, name, kind, token)).unwrap()
  }

  fn active(id: u8, name: &str, kind: SourceKind, token: &str) -> ActiveSource {
    ActiveSource {
      id: SourceId::from_bytes([id; 16]),
      conversation: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      name: name.into(),
      kind,
      token_sha256: SourceTokenHash::from_bytes(source_token_sha256(token)),
      options: SourceOptions {
        rate_limit_per_minute: 60,
        ..SourceOptions::default()
      },
      created_by: UserId::from_snowflake(Snowflake::new(1).unwrap()),
      created_at_ms: 0,
    }
  }
}
