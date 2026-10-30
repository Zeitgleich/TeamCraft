//! The app as a test sees it: a client on the desktop's store (SQLCipher on a real disk behind the
//! `TrackedStore` with its message cache), driven by the runner and connection supervisor the apps
//! use, so reconnects, heartbeats, backoff and frames held during a sync behave as in the app.
//!
//! The runner is not `Send`: tests run it on a `tokio::task::LocalSet`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::api::HttpServerApi;
use teamcraft_core::clock::SystemClock;
use teamcraft_core::connection::{ConnectionState, ConnectionStatus, NetworkChanges};
use teamcraft_core::runner::{RunnerCommand, RunnerHandle, RunnerSettings, SyncRunner};
use teamcraft_core::store::{
  LocalStore, MessageContent, MessageFilter, SealedSqliteStore, SqlCipherStore, StaticKeySource, TrackedStore,
};
use teamcraft_core::{Client, ClientSettings};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::ConversationId;

use crate::process::binary_dir;
use crate::tokens::UserTokens;

/// Where tests keep stores on a disk (default: beside the build output). Not `/tmp`: in memory, a
/// sync to disk costs nothing and the tests would miss what it costs the app.
pub const DISK_DIR_ENV: &str = "E2E_DISK_DIR";
/// Messages the desktop's message cache holds (`MESSAGE_CACHE_MESSAGES` in the desktop core).
const MESSAGE_CACHE_MESSAGES: usize = 60_000;

/// The desktop's store.
pub type DiskStore = TrackedStore<Arc<SqlCipherStore>>;
/// The mobile apps' store: sealed records in plain SQLite, all of them in memory.
pub type MobileStore = TrackedStore<Arc<SealedSqliteStore>>;

/// A running app on store `S`.
pub struct App<S = DiskStore> {
  pub store: S,
  /// Reports a network change or a wake-up, as the platform does.
  pub network: NetworkChanges,
  status: ConnectionStatus,
  handle: RunnerHandle,
  task: tokio::task::JoinHandle<()>,
}

/// A fresh directory on a disk, removed when dropped.
pub struct DiskDir(PathBuf);

impl<S: LocalStore + Clone + 'static> App<S> {
  /// Opens the enrolled `store` and starts the runner against `base_url`.
  pub fn start(base_url: &str, tokens: UserTokens, store: S) -> Self {
    Self::start_focused(base_url, tokens, store, None)
  }

  /// Like [`Self::start`], with `maybe_focus` the conversation the user looks at (caught up first).
  pub fn start_focused(
    base_url: &str,
    tokens: UserTokens,
    store: S,
    maybe_focus: Option<ConversationId>,
  ) -> Self {
    init_log();
    let api = HttpServerApi::new(base_url, tokens);
    let client = Client::open(
      api,
      store.clone(),
      Box::new(SystemClock),
      ClientSettings::default(),
    )
    .unwrap()
    .expect("an enrolled store");
    if let Some(focus) = maybe_focus {
      client.catch_up_focus().set(focus);
    }
    let (mut runner, handle) = SyncRunner::new(client, RunnerSettings::default(), Box::new(|| {}));
    let network = NetworkChanges::new();
    runner.watch_network(&network);
    let status = runner.connection_status();
    let task = tokio::task::spawn_local(async move {
      runner.run().await;
    });
    Self {
      store,
      network,
      status,
      handle,
      task,
    }
  }

  pub fn state(&self) -> ConnectionState {
    self.status.get()
  }

  pub fn is_connected(&self) -> bool {
    self.state() == ConnectionState::Connected
  }

  /// How long until the connection was up, counted from now.
  pub async fn wait_connected(&self, timeout: Duration) -> Duration {
    let started = Instant::now();
    while !self.is_connected() {
      assert!(
        started.elapsed() < timeout,
        "the app did not connect within {timeout:?}: {:?}",
        self.state()
      );
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
    started.elapsed()
  }

  /// How long `text` took to show in `channel`, counted from now.
  pub async fn wait_for(&self, channel: &ConversationId, text: &str, timeout: Duration) -> Duration {
    let started = Instant::now();
    let mut next_report = Duration::from_secs(10);
    loop {
      if self.texts(*channel).iter().any(|known| known == text) {
        return started.elapsed();
      }
      if started.elapsed() >= next_report {
        let parked = self
          .store
          .list(teamcraft_core::store::Table::Undecrypted)
          .map_or(0, |parked| parked.len());
        eprintln!(
          "app: still waiting for {text:?} after {} s; connection: {:?}; {} texts in the conversation, {parked} messages parked undecryptable",
          next_report.as_secs(),
          self.state(),
          self.texts(*channel).len()
        );
        next_report += Duration::from_secs(10);
      }
      assert!(
        started.elapsed() < timeout,
        "{text:?} did not arrive within {timeout:?}; connection: {:?}",
        self.state()
      );
      tokio::time::sleep(Duration::from_millis(20)).await;
    }
  }

  /// The texts of a conversation, replies included, in display order.
  pub fn texts(&self, channel: ConversationId) -> Vec<String> {
    self
      .store
      .messages(channel, MessageFilter::All)
      .unwrap()
      .into_iter()
      .filter_map(|message| match message.content {
        MessageContent::Text { body, .. } => Some(body),
        _ => None,
      })
      .collect()
  }

  /// Every expected message is there, and no text twice.
  pub fn assert_each_once(&self, channels: &[ConversationId], expected: &[(ConversationId, String)]) {
    let mut seen = BTreeSet::new();
    for channel in channels {
      for text in self.texts(*channel) {
        assert!(seen.insert((*channel, text.clone())), "{text:?} arrived twice");
      }
    }
    for message in expected {
      assert!(seen.contains(message), "{:?} is missing", message.1);
    }
  }

  pub async fn stop(self) {
    self.handle.send(RunnerCommand::Stop);
    drop(self.task.await);
  }
}

impl DiskDir {
  pub fn new(prefix: &str) -> Self {
    let base = std::env::var_os(DISK_DIR_ENV)
      .map(PathBuf::from)
      .unwrap_or_else(|| binary_dir().join("e2e-disk"));
    let directory = base.join(format!(
      "{prefix}-{}",
      rand_core::RngCore::next_u64(&mut rand_core::OsRng)
    ));
    std::fs::create_dir_all(&directory).expect("creating the store directory");
    Self(directory)
  }

  pub fn path(&self) -> &Path {
    &self.0
  }
}

impl Drop for DiskDir {
  fn drop(&mut self) {
    // A leftover directory only costs disk space.
    drop(std::fs::remove_dir_all(&self.0));
  }
}

/// The desktop's store in `directory`, with a new random key.
pub fn disk_store(directory: &Path) -> DiskStore {
  let key = StaticKeySource::new(SymmetricKey::generate(&mut rand_core::OsRng));
  let store = SqlCipherStore::open(&directory.join("store.db"), &key).expect("opening the store");
  TrackedStore::with_message_cache(Arc::new(store), MESSAGE_CACHE_MESSAGES)
}

/// The mobile apps' store in `directory`, with a new random key.
pub fn mobile_store(directory: &Path) -> MobileStore {
  let key = StaticKeySource::new(SymmetricKey::generate(&mut rand_core::OsRng));
  let store = SealedSqliteStore::open(&directory.join("store.db"), &key).expect("opening the store");
  TrackedStore::new(Arc::new(store))
}

/// The client core's log on stderr when `E2E_LOG` names a filter (`E2E_LOG=teamcraft_core=debug`).
fn init_log() {
  if let Ok(filter) = std::env::var("E2E_LOG") {
    // A second test in the process finds the subscriber set; that is fine.
    drop(
      tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_writer(std::io::stderr)
        .try_init(),
    );
  }
}
