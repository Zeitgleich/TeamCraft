//! The sources the endpoints accept, shared between the endpoints and the worker.
//!
//! Two kinds live side by side:
//!
//! - **Managed** sources come from the app (docs/bridge.md, "Sources"): a channel admin announces
//!   one with an encrypted control message in the channel, the worker reads it from the bot's store
//!   and replaces the managed set when something changed. Only the token's SHA-256 is known.
//! - **Static** sources are the optional `[[sources]]` of the configuration, for a bridge whose
//!   operator prefers files. Their tokens are hashed at start and looked up the same way.
//!
//! A request's token is hashed and compared with every known hash in constant time.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use secrecy::ExposeSecret;
use teamcraft_core::integration_sources::ActiveSource;
use teamcraft_crypto::source_token::{find_source_token, source_token_sha256};
use teamcraft_proto::{ConversationId, SourceId, SourceKind};

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
#[derive(Clone, Default)]
pub struct SourceRegistry {
  inner: Arc<RwLock<Registry>>,
}

#[derive(Default)]
struct Registry {
  statics: Vec<Arc<Source>>,
  managed: Vec<Arc<Source>>,
  limiters: HashMap<SourceKey, RateLimiter>,
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
    let registry = Self::default();
    {
      let mut inner = registry.write();
      inner.statics = config
        .sources
        .iter()
        .map(|source| Arc::new(Source::from_config(source)))
        .collect();
    }
    registry
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

  /// Takes `count` events from the source's bucket, all or nothing.
  pub fn try_take(&self, source: &Source, count: u32, now: Instant) -> bool {
    self.with_limiter(source, now, |limiter| limiter.try_take(count, now))
  }

  /// Takes as many of `count` events as the bucket allows.
  pub fn take_up_to(&self, source: &Source, count: u32, now: Instant) -> u32 {
    self.with_limiter(source, now, |limiter| limiter.take_up_to(count, now))
  }

  pub fn retry_after(&self, source: &Source, now: Instant) -> Duration {
    self.with_limiter(source, now, |limiter| limiter.retry_after())
  }

  fn with_limiter<T>(&self, source: &Source, now: Instant, run: impl FnOnce(&mut RateLimiter) -> T) -> T {
    let mut inner = self.write();
    let limiter = inner
      .limiters
      .entry(source.key.clone())
      .or_insert_with(|| RateLimiter::new(source.rate_limit_per_minute, now));
    run(limiter)
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

  fn registry_with_static() -> SourceRegistry {
    let config = BridgeConfig::from_toml(&format!(
      r#"
      [server]
      url = "http://localhost"
      [store]
      path = "x.db"
      [[sources]]
      name = "static"
      token = "{STATIC_TOKEN}"
      conversation_id = "1"
      "#
    ))
    .unwrap();
    SourceRegistry::new(&config)
  }

  fn managed(id: u8, name: &str, kind: SourceKind, token: &str) -> Source {
    Source::from_managed(&ActiveSource {
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
    })
    .unwrap()
  }
}
