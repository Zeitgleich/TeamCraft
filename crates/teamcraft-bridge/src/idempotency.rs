//! Idempotency keys of webhooks (docs/bridge.md, "Retries"): a producer that lost the answer to a
//! request sends it again with the same `Idempotency-Key` header, and the bridge answers the repeat
//! as it answered the original, without posting the message twice.
//!
//! - Keys are per source and live in memory: from the answer that accepted their request on, for
//!   `idempotency.window_secs`. A restart forgets them. `idempotency.max_keys` and `max_bytes`
//!   bound them; beyond, the source that holds the most loses its oldest key first, so one source
//!   sending many keys pushes out its own.
//! - Only accepted requests are remembered. A refused one posted nothing, so its retry runs again.
//! - A repeat is recognised by the message it would post: text, alert, and each file's name, type
//!   and bytes ([`MessageFingerprint`]), not the raw body, so a form sent again with a new boundary
//!   is the same request. The same key with another message is refused.
//! - A repeat that arrives while the original still runs waits for it: accepted, the repeat gets
//!   its answer; refused or given up, the repeat runs itself.
//! - Keys are never logged: [`IdempotencyKey`] has no `Display` and a redacted `Debug`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use teamcraft_crypto::fingerprint::{Fingerprint, FingerprintHasher};
use tokio::sync::watch;

use crate::config::IdempotencySection;
use crate::sources::SourceKey;

/// The request header (IETF draft `draft-ietf-httpapi-idempotency-key-header`, as Stripe and others
/// take it). Slack's, Mattermost's and Discord's webhooks have no equivalent.
pub const IDEMPOTENCY_KEY_HEADER: &str = "idempotency-key";
/// Set to `true` on an answer given from memory, without posting.
pub const IDEMPOTENT_REPLAYED_HEADER: &str = "idempotent-replayed";
/// Longest key taken, in bytes.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 255;
/// What a remembered key costs besides its own bytes: its entry, its place in the queue, the
/// fingerprint and the time.
pub const ENTRY_OVERHEAD_BYTES: usize = 128;

/// The keys of all sources; cloning shares them.
#[derive(Clone)]
pub struct IdempotencyStore {
  inner: Arc<Mutex<Keys>>,
}

/// An `Idempotency-Key` as the producer sent it (without the quotes of the draft's form).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(Arc<str>);

/// What a request with a key does ([`IdempotencyStore::claim`]).
#[derive(Debug)]
pub enum KeyUse {
  /// The key is free: the request goes ahead, and accepts the guard once its message is posted.
  Run(KeyGuard),
  /// A request with this key and the same message was accepted: answer as then, post nothing.
  Replay,
  /// The key was used for another message.
  Mismatch,
}

/// The key of a request that goes ahead. [`Self::accept`] once its message is posted; dropped
/// before that, the key is free again and a request waiting for it runs.
pub struct KeyGuard {
  store: IdempotencyStore,
  source: SourceKey,
  key: IdempotencyKey,
  id: u64,
  is_accepted: bool,
}

/// The fingerprint of the message a request would post, built while its body arrives. Every
/// variable piece is length-prefixed, so different messages never feed the same bytes.
#[derive(Debug, Default)]
pub struct MessageFingerprint {
  files: FingerprintHasher,
  maybe_file: Option<FingerprintHasher>,
}

/// The header is there but is no key: more than one, empty, longer than
/// [`MAX_IDEMPOTENCY_KEY_BYTES`], or not printable ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the Idempotency-Key header must be one value of 1 to 255 printable ASCII characters")]
pub struct InvalidKey;

struct Keys {
  window: Duration,
  max_keys: usize,
  max_bytes: usize,
  sources: HashMap<SourceKey, SourceKeys>,
  count: usize,
  bytes: usize,
  next_id: u64,
}

/// One source's keys.
#[derive(Default)]
struct SourceKeys {
  entries: HashMap<IdempotencyKey, Entry>,
  /// The accepted keys and when they were accepted, oldest first: they expire, and are evicted,
  /// from the front. Running keys aren't in it.
  accepted: VecDeque<(IdempotencyKey, Instant)>,
  bytes: usize,
}

enum Entry {
  /// Its request runs. Requests waiting for it subscribe to `settled`; the sender is dropped with
  /// the entry when the request settles, which wakes them.
  Running {
    id: u64,
    fingerprint: Fingerprint,
    settled: watch::Sender<()>,
  },
  Accepted {
    fingerprint: Fingerprint,
  },
}

impl IdempotencyStore {
  pub fn new(settings: &IdempotencySection) -> Self {
    Self {
      inner: Arc::new(Mutex::new(Keys {
        window: settings.window(),
        max_keys: settings.max_keys,
        max_bytes: settings.max_bytes,
        sources: HashMap::new(),
        count: 0,
        bytes: 0,
        next_id: 0,
      })),
    }
  }

  /// What a request with `key` that would post the message `fingerprint` does. While a request
  /// with the same key and message runs, waits for it to settle first.
  pub async fn claim(&self, source: &SourceKey, key: &IdempotencyKey, fingerprint: Fingerprint) -> KeyUse {
    loop {
      match self.try_claim(source, key, fingerprint, Instant::now()) {
        Ok(key_use) => return key_use,
        // Closed when the running request settles; nothing is ever sent.
        Err(mut settled) => {
          let _ = settled.changed().await;
        },
      }
    }
  }

  /// [`Self::claim`] without waiting: `Err` while a request with the same key and message runs,
  /// with what to wait on.
  pub fn try_claim(
    &self,
    source: &SourceKey,
    key: &IdempotencyKey,
    fingerprint: Fingerprint,
    now: Instant,
  ) -> Result<KeyUse, watch::Receiver<()>> {
    let mut keys = self.lock();
    keys.expire(now);
    match keys.sources.get(source).and_then(|keys| keys.entries.get(key)) {
      Some(Entry::Accepted { fingerprint: kept }) if *kept == fingerprint => Ok(KeyUse::Replay),
      Some(Entry::Running {
        fingerprint: running,
        settled,
        ..
      }) if *running == fingerprint => Err(settled.subscribe()),
      Some(_) => Ok(KeyUse::Mismatch),
      None => {
        let id = keys.insert_running(source, key, fingerprint);
        Ok(KeyUse::Run(KeyGuard {
          store: self.clone(),
          source: source.clone(),
          key: key.clone(),
          id,
          is_accepted: false,
        }))
      },
    }
  }

  /// Forgets the keys whose window ended by `now`.
  pub fn expire(&self, now: Instant) {
    self.lock().expire(now);
  }

  /// Keys remembered or running, and their bytes as counted against `idempotency.max_bytes`.
  pub fn usage(&self) -> (usize, usize) {
    let keys = self.lock();
    (keys.count, keys.bytes)
  }

  /// Requests waiting for a running request with `key`, of any source.
  #[cfg(test)]
  pub(crate) fn waiting_for(&self, key: &str) -> usize {
    let key = IdempotencyKey(Arc::from(key));
    self
      .lock()
      .sources
      .values()
      .filter_map(|keys| keys.entries.get(&key))
      .map(|entry| match entry {
        Entry::Running { settled, .. } => settled.receiver_count(),
        Entry::Accepted { .. } => 0,
      })
      .sum()
  }

  fn lock(&self) -> MutexGuard<'_, Keys> {
    // Every change keeps the map, the queues and the counts in step before anything can fail, so
    // a poisoned lock is still consistent.
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl IdempotencyKey {
  /// The request's key, `None` without the header.
  pub fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, InvalidKey> {
    let mut values = headers.get_all(IDEMPOTENCY_KEY_HEADER).iter();
    let Some(value) = values.next() else {
      return Ok(None);
    };
    if values.next().is_some() {
      return Err(InvalidKey);
    }
    let text = value.to_str().map_err(|_| InvalidKey)?;
    // The draft writes the key as a structured-field string, "..."; the quotes aren't part of it.
    let text = text
      .strip_prefix('"')
      .and_then(|inner| inner.strip_suffix('"'))
      .unwrap_or(text);
    let is_key = !text.is_empty()
      && text.len() <= MAX_IDEMPOTENCY_KEY_BYTES
      && text.bytes().all(|byte| (b' '..=b'~').contains(&byte));
    if !is_key {
      return Err(InvalidKey);
    }
    Ok(Some(Self(Arc::from(text))))
  }

  fn cost_bytes(&self) -> usize {
    self.0.len() + ENTRY_OVERHEAD_BYTES
  }
}

impl KeyGuard {
  /// The request's message is posted (queued for sending): repeats get its answer for the window.
  pub fn accept(mut self, now: Instant) {
    self.is_accepted = true;
    self.store.lock().accept(&self.source, &self.key, self.id, now);
  }
}

impl MessageFingerprint {
  /// A file of the form begins; its bytes follow.
  pub fn start_file(&mut self, file_name: &str, media_type: &str) {
    self.files.update(b"F");
    update_framed(&mut self.files, file_name.as_bytes());
    update_framed(&mut self.files, media_type.as_bytes());
    self.maybe_file = Some(FingerprintHasher::new());
  }

  pub fn file_data(&mut self, bytes: &[u8]) {
    if let Some(file) = self.maybe_file.as_mut() {
      file.update(bytes);
    }
  }

  /// The current file is complete.
  pub fn end_file(&mut self, size_bytes: u64) {
    if let Some(file) = self.maybe_file.take() {
      self.files.update(&size_bytes.to_be_bytes());
      self.files.update(file.finish().as_bytes());
    }
  }

  /// The fingerprint of the message with `text`, `is_alert` and the files fed before.
  pub fn finish(self, text: &str, is_alert: bool) -> Fingerprint {
    let mut message = self.files;
    message.update(b"M");
    update_framed(&mut message, text.as_bytes());
    message.update(&[u8::from(is_alert)]);
    message.finish()
  }
}

impl Keys {
  /// A running entry for a free key; evicts accepted keys if it doesn't fit. Returns its ID.
  fn insert_running(&mut self, source: &SourceKey, key: &IdempotencyKey, fingerprint: Fingerprint) -> u64 {
    let id = self.next_id;
    self.next_id += 1;
    let (settled, _) = watch::channel(());
    let cost = key.cost_bytes();
    let keys = self.sources.entry(source.clone()).or_default();
    keys.entries.insert(
      key.clone(),
      Entry::Running {
        id,
        fingerprint,
        settled,
      },
    );
    keys.bytes += cost;
    self.count += 1;
    self.bytes += cost;
    self.evict_over_bounds();
    id
  }

  /// Turns the guard's running entry into an accepted one; waiting requests wake up.
  fn accept(&mut self, source: &SourceKey, key: &IdempotencyKey, id: u64, now: Instant) {
    let Some(keys) = self.sources.get_mut(source) else {
      return;
    };
    let Some(entry) = keys.entries.get_mut(key) else {
      return;
    };
    let Entry::Running {
      id: running,
      fingerprint,
      ..
    } = entry
    else {
      return;
    };
    if *running != id {
      return;
    }
    let fingerprint = *fingerprint;
    *entry = Entry::Accepted { fingerprint };
    keys.accepted.push_back((key.clone(), now));
  }

  /// Frees the key of a guard dropped before it was accepted; waiting requests wake up.
  fn release(&mut self, source: &SourceKey, key: &IdempotencyKey, id: u64) {
    let Some(keys) = self.sources.get_mut(source) else {
      return;
    };
    if !matches!(keys.entries.get(key), Some(Entry::Running { id: running, .. }) if *running == id) {
      return;
    }
    keys.entries.remove(key);
    let cost = key.cost_bytes();
    keys.bytes -= cost;
    self.count -= 1;
    self.bytes -= cost;
    if keys.entries.is_empty() {
      self.sources.remove(source);
    }
  }

  fn expire(&mut self, now: Instant) {
    let window = self.window;
    for keys in self.sources.values_mut() {
      while keys
        .accepted
        .front()
        .is_some_and(|(_, accepted_at)| now.saturating_duration_since(*accepted_at) >= window)
      {
        if let Some(freed) = keys.pop_oldest() {
          self.count -= 1;
          self.bytes -= freed;
        }
      }
    }
    self.sources.retain(|_, keys| !keys.entries.is_empty());
  }

  /// Over `max_keys` or `max_bytes`: the source that holds the most bytes loses its oldest accepted
  /// key until the rest fit. Running keys stay; they are bounded by the requests that may run.
  fn evict_over_bounds(&mut self) {
    while self.count > self.max_keys || self.bytes > self.max_bytes {
      let Some(keys) = self
        .sources
        .values_mut()
        .filter(|keys| !keys.accepted.is_empty())
        .max_by_key(|keys| keys.bytes)
      else {
        break;
      };
      match keys.pop_oldest() {
        Some(freed) => {
          self.count -= 1;
          self.bytes -= freed;
        },
        None => break,
      }
    }
    self.sources.retain(|_, keys| !keys.entries.is_empty());
  }
}

impl SourceKeys {
  /// Forgets the oldest accepted key; returns its bytes.
  fn pop_oldest(&mut self) -> Option<usize> {
    let (key, _) = self.accepted.pop_front()?;
    self.entries.remove(&key);
    let cost = key.cost_bytes();
    self.bytes -= cost;
    Some(cost)
  }
}

impl Drop for KeyGuard {
  fn drop(&mut self) {
    if !self.is_accepted {
      self.store.lock().release(&self.source, &self.key, self.id);
    }
  }
}

impl fmt::Debug for IdempotencyKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("IdempotencyKey([redacted])")
  }
}

impl fmt::Debug for KeyGuard {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("KeyGuard")
      .field("source", &self.source)
      .field("is_accepted", &self.is_accepted)
      .finish_non_exhaustive()
  }
}

fn update_framed(hasher: &mut FingerprintHasher, bytes: &[u8]) {
  hasher.update(&(bytes.len() as u64).to_be_bytes());
  hasher.update(bytes);
}

#[cfg(test)]
mod tests {
  use axum::http::HeaderValue;

  use super::*;

  const WINDOW: Duration = Duration::from_secs(600);

  mod claims {
    use super::*;

    #[test]
    fn a_repeat_of_an_accepted_message_is_replayed_and_another_message_refused() {
      let store = store(100, 1 << 20);
      let now = Instant::now();
      accept(
        store.try_claim(&source("a"), &key("k1"), message("deploy"), now),
        now,
      );
      assert!(matches!(
        store.try_claim(&source("a"), &key("k1"), message("deploy"), now),
        Ok(KeyUse::Replay)
      ));
      assert!(matches!(
        store.try_claim(&source("a"), &key("k1"), message("other"), now),
        Ok(KeyUse::Mismatch)
      ));
      assert_eq!(store.usage().0, 1, "a repeat stores nothing");
    }

    #[test]
    fn a_key_whose_request_was_refused_is_free_again() {
      let store = store(100, 1 << 20);
      let now = Instant::now();
      let guard = run(store.try_claim(&source("a"), &key("k1"), message("deploy"), now));
      drop(guard);
      assert_eq!(store.usage(), (0, 0));
      assert!(
        matches!(
          store.try_claim(&source("a"), &key("k1"), message("another"), now),
          Ok(KeyUse::Run(_))
        ),
        "nothing was posted, so even another message may take the key"
      );
    }

    #[test]
    fn keys_are_per_source() {
      let store = store(100, 1 << 20);
      let now = Instant::now();
      accept(
        store.try_claim(&source("a"), &key("k1"), message("deploy"), now),
        now,
      );
      assert!(matches!(
        store.try_claim(&source("b"), &key("k1"), message("deploy"), now),
        Ok(KeyUse::Run(_))
      ));
    }

    #[test]
    fn another_message_under_a_running_key_is_refused_at_once() {
      let store = store(100, 1 << 20);
      let now = Instant::now();
      let _running = run(store.try_claim(&source("a"), &key("k1"), message("deploy"), now));
      assert!(matches!(
        store.try_claim(&source("a"), &key("k1"), message("other"), now),
        Ok(KeyUse::Mismatch)
      ));
      assert!(
        store
          .try_claim(&source("a"), &key("k1"), message("deploy"), now)
          .is_err(),
        "the same message waits"
      );
    }
  }

  mod waiting {
    use futures_util::future::Either;

    use super::*;

    #[tokio::test]
    async fn repeats_wait_for_the_running_request_and_get_its_answer() {
      let store = store(100, 1 << 20);
      let guard = run(store.try_claim(&source("a"), &key("k1"), message("x"), Instant::now()));
      let repeats: Vec<_> = (0..3).map(|_| spawn_claim(&store)).collect();
      wait_for_waiters(&store, 3).await;
      guard.accept(Instant::now());
      for repeat in repeats {
        assert!(matches!(repeat.await.unwrap(), KeyUse::Replay));
      }
    }

    #[tokio::test]
    async fn when_the_running_request_fails_one_repeat_runs_and_the_other_waits_for_it() {
      let store = store(100, 1 << 20);
      let guard = run(store.try_claim(&source("a"), &key("k1"), message("x"), Instant::now()));
      let (first, second) = (spawn_claim(&store), spawn_claim(&store));
      wait_for_waiters(&store, 2).await;
      drop(guard);
      // One of them takes the key over; the other waits for that one.
      let (taken_over, other) = match futures_util::future::select(first, second).await {
        Either::Left((result, other)) | Either::Right((result, other)) => (result.unwrap(), other),
      };
      let KeyUse::Run(guard) = taken_over else {
        panic!("the first to wake runs");
      };
      wait_for_waiters(&store, 1).await;
      guard.accept(Instant::now());
      assert!(matches!(other.await.unwrap(), KeyUse::Replay));
    }

    fn spawn_claim(store: &IdempotencyStore) -> tokio::task::JoinHandle<KeyUse> {
      let store = store.clone();
      tokio::spawn(async move { store.claim(&source("a"), &key("k1"), message("x")).await })
    }

    /// Until `count` requests wait for the running `k1`.
    async fn wait_for_waiters(store: &IdempotencyStore, count: usize) {
      for _ in 0..10_000 {
        if store.waiting_for("k1") >= count {
          return;
        }
        tokio::task::yield_now().await;
      }
      panic!("{count} requests never waited");
    }
  }

  mod bounds {
    use super::*;

    #[test]
    fn keys_are_forgotten_once_their_window_ends() {
      let store = store(100, 1 << 20);
      let start = Instant::now();
      accept(
        store.try_claim(&source("a"), &key("k1"), message("x"), start),
        start,
      );
      let almost = start + WINDOW - Duration::from_secs(1);
      assert!(matches!(
        store.try_claim(&source("a"), &key("k1"), message("x"), almost),
        Ok(KeyUse::Replay)
      ));
      assert!(
        matches!(
          store.try_claim(&source("a"), &key("k1"), message("x"), start + WINDOW),
          Ok(KeyUse::Run(_))
        ),
        "after the window the request runs again"
      );
      store.expire(start + 2 * WINDOW);
      assert_eq!(store.usage(), (0, 0));
    }

    #[test]
    fn a_running_request_never_expires_and_its_window_starts_with_its_answer() {
      let store = store(100, 1 << 20);
      let start = Instant::now();
      let running = store.try_claim(&source("a"), &key("k1"), message("x"), start);
      let answered = start + 2 * WINDOW;
      assert!(
        store
          .try_claim(&source("a"), &key("k1"), message("x"), answered)
          .is_err()
      );
      accept(running, answered);
      assert!(matches!(
        store.try_claim(&source("a"), &key("k1"), message("x"), answered + WINDOW / 2),
        Ok(KeyUse::Replay)
      ));
    }

    #[test]
    fn over_the_count_the_oldest_key_of_the_source_holding_most_goes_first() {
      let store = store(3, 1 << 20);
      let now = Instant::now();
      accept(
        store.try_claim(&source("quiet"), &key("q1"), message("x"), now),
        now,
      );
      for name in ["n1", "n2", "n3"] {
        accept(
          store.try_claim(&source("noisy"), &key(name), message("x"), now),
          now,
        );
      }
      assert_eq!(store.usage().0, 3);
      assert!(
        matches!(
          store.try_claim(&source("noisy"), &key("n1"), message("x"), now),
          Ok(KeyUse::Run(_))
        ),
        "the noisy source's oldest key was forgotten"
      );
      assert!(matches!(
        store.try_claim(&source("quiet"), &key("q1"), message("x"), now),
        Ok(KeyUse::Replay)
      ));
    }

    #[test]
    fn over_the_bytes_long_keys_push_out_old_ones() {
      // Room for two keys of 255 bytes, or more short ones.
      let max_bytes = 2 * (MAX_IDEMPOTENCY_KEY_BYTES + ENTRY_OVERHEAD_BYTES);
      let store = store(100, max_bytes);
      let now = Instant::now();
      for name in ["s1", "s2", "s3"] {
        accept(store.try_claim(&source("a"), &key(name), message("x"), now), now);
      }
      let long = "k".repeat(MAX_IDEMPOTENCY_KEY_BYTES);
      accept(store.try_claim(&source("a"), &key(&long), message("x"), now), now);
      let (count, bytes) = store.usage();
      assert!(bytes <= max_bytes, "{bytes}");
      assert_eq!(count, 3, "the oldest short key went");
      for kept in [long.as_str(), "s2", "s3"] {
        assert!(matches!(
          store.try_claim(&source("a"), &key(kept), message("x"), now),
          Ok(KeyUse::Replay)
        ));
      }
      assert!(matches!(
        store.try_claim(&source("a"), &key("s1"), message("x"), now),
        Ok(KeyUse::Run(_))
      ));
    }

    #[test]
    fn running_requests_are_never_evicted() {
      let store = store(1, 1 << 20);
      let now = Instant::now();
      let first = store.try_claim(&source("a"), &key("k1"), message("x"), now);
      let second = store.try_claim(&source("a"), &key("k2"), message("x"), now);
      assert_eq!(store.usage().0, 2, "both run: the bound waits for them");
      accept(first, now);
      accept(second, now);
      accept(store.try_claim(&source("a"), &key("k3"), message("x"), now), now);
      assert_eq!(store.usage().0, 1, "accepted keys are evicted down to the bound");
    }
  }

  mod keys {
    use super::*;

    #[test]
    fn the_header_is_one_printable_value_of_at_most_255_bytes() {
      assert_eq!(parse(&[]), Ok(None));
      assert_eq!(parse(&["3f2a-77"]), Ok(Some(key("3f2a-77"))));
      assert_eq!(
        parse(&["\"3f2a-77\""]),
        Ok(Some(key("3f2a-77"))),
        "the draft's quoted form is the same key"
      );
      assert!(parse(&[&"k".repeat(255)]).unwrap().is_some());
      let too_long = "k".repeat(256);
      for (what, values) in [
        ("empty", vec![""]),
        ("empty quotes", vec!["\"\""]),
        ("too long", vec![too_long.as_str()]),
        ("two of them", vec!["a", "b"]),
        ("a tab", vec!["a\tb"]),
      ] {
        assert_eq!(parse(&values), Err(InvalidKey), "{what}");
      }
      let mut headers = HeaderMap::new();
      headers.insert(
        IDEMPOTENCY_KEY_HEADER,
        HeaderValue::from_bytes(b"caf\xc3\xa9").unwrap(),
      );
      assert_eq!(
        IdempotencyKey::from_headers(&headers),
        Err(InvalidKey),
        "not ASCII"
      );
    }

    #[test]
    fn keys_stay_out_of_debug_output() {
      let key = key("secret-ish-key");
      assert_eq!(format!("{key:?}"), "IdempotencyKey([redacted])");
      let store = store(10, 1 << 20);
      let guard = run(store.try_claim(&source("a"), &key, message("x"), Instant::now()));
      assert!(!format!("{guard:?}").contains("secret-ish-key"));
    }

    fn parse(values: &[&str]) -> Result<Option<IdempotencyKey>, InvalidKey> {
      let mut headers = HeaderMap::new();
      for value in values {
        headers.append(IDEMPOTENCY_KEY_HEADER, HeaderValue::from_str(value).unwrap());
      }
      IdempotencyKey::from_headers(&headers)
    }
  }

  mod fingerprints {
    use super::*;

    const REPORT: (&str, &str, &[u8]) = ("report.txt", "text/plain", b"all green");

    #[test]
    fn the_same_message_has_the_same_fingerprint_and_any_change_shows() {
      let report = with_files(&[REPORT], "Build 7", false);
      assert_eq!(report, with_files(&[REPORT], "Build 7", false));
      for (what, other) in [
        ("text", with_files(&[REPORT], "Build 8", false)),
        ("alert", with_files(&[REPORT], "Build 7", true)),
        (
          "file name",
          with_files(&[("other.txt", "text/plain", b"all green")], "Build 7", false),
        ),
        (
          "media type",
          with_files(&[("report.txt", "text/html", b"all green")], "Build 7", false),
        ),
        (
          "bytes",
          with_files(&[("report.txt", "text/plain", b"all red")], "Build 7", false),
        ),
        ("no file", with_files(&[], "Build 7", false)),
        (
          "one more file",
          with_files(&[REPORT, ("empty", "text/plain", b"")], "Build 7", false),
        ),
      ] {
        assert_ne!(report, other, "{what}");
      }
    }

    #[test]
    fn pieces_cannot_move_between_fields() {
      assert_ne!(
        with_files(&[("ab", "c", b"")], "", false),
        with_files(&[("a", "bc", b"")], "", false)
      );
      assert_ne!(
        with_files(&[("a", "b", b"c")], "", false),
        with_files(&[("a", "b", b"")], "c", false)
      );
    }

    #[test]
    fn file_bytes_count_however_they_arrive() {
      let mut split = MessageFingerprint::default();
      split.start_file("a.bin", "application/octet-stream");
      split.file_data(b"12");
      split.file_data(b"345");
      split.end_file(5);
      assert_eq!(
        split.finish("x", false),
        with_files(&[("a.bin", "application/octet-stream", b"12345")], "x", false)
      );
    }

    fn with_files(files: &[(&str, &str, &[u8])], text: &str, is_alert: bool) -> Fingerprint {
      let mut fingerprint = MessageFingerprint::default();
      for (name, media_type, bytes) in files {
        fingerprint.start_file(name, media_type);
        fingerprint.file_data(bytes);
        fingerprint.end_file(bytes.len() as u64);
      }
      fingerprint.finish(text, is_alert)
    }
  }

  fn store(max_keys: usize, max_bytes: usize) -> IdempotencyStore {
    IdempotencyStore::new(&IdempotencySection {
      window_secs: WINDOW.as_secs(),
      max_keys,
      max_bytes,
    })
  }

  fn run(claim: Result<KeyUse, watch::Receiver<()>>) -> KeyGuard {
    match claim {
      Ok(KeyUse::Run(guard)) => guard,
      other => panic!("not a free key: {other:?}"),
    }
  }

  fn accept(claim: Result<KeyUse, watch::Receiver<()>>, now: Instant) {
    run(claim).accept(now);
  }

  fn source(name: &str) -> SourceKey {
    SourceKey::Static(name.into())
  }

  fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey(Arc::from(text))
  }

  fn message(text: &str) -> Fingerprint {
    MessageFingerprint::default().finish(text, false)
  }
}
