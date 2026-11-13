//! The unread state of the accounts that are not active (#214), for the badges of the switcher.
//!
//! Only the active account runs a client core (store, gateway, notifications). For each other
//! account the shell asks that account's server for its counts (`GET /api/v1/unread`,
//! docs/notifications.md) every [`POLL_INTERVAL`], with that account's own sign-in service: its
//! refresh token and DPoP key from the sealed file, its own HTTP client. The counts are metadata the
//! server keeps anyway; nothing is decrypted, and nothing of one account goes to another account's
//! server. The server counts muted conversations too (mutes live in the encrypted account data), so
//! a background badge shows mentions as a number and anything else only as a dot.
//!
//! A server's session is used by one sign-in service at a time, or a refresh could rotate the
//! refresh token under the other one: switching to an account first stops its watcher and waits for
//! a round in progress ([`BackgroundAccounts::stop`]); the previous account's watcher starts only
//! after the active service let go of it (`AuthService::set_server` waits for a refresh in progress).

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tauri::async_runtime::JoinHandle;
use teamcraft_core::store::SecretVault;
use teamcraft_proto::UnreadResponse;

use super::servers::ServerState;
use super::{AuthError, AuthService, DesktopConfig, VaultStore};

/// How often a background account's counts are read. A few requests an hour per account; the
/// active account gets its counts live from the gateway.
const POLL_INTERVAL: Duration = Duration::from_secs(120);
/// The first round waits a moment, so a start or a switch isn't slowed down by it.
const FIRST_POLL_DELAY: Duration = Duration::from_secs(5);
const UNREAD_PATH: &str = "/api/v1/unread";
/// Tells the webview that the account list or a background badge changed.
pub const ACCOUNTS_CHANGED_EVENT: &str = "accounts-changed";

/// What the switcher shows of an account that is not active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackgroundState {
  /// Not read yet, or the server didn't answer.
  #[default]
  Unknown,
  /// No session is stored, or the identity provider refused it: signing in again is up to the user.
  SignedOut,
  Unread(UnreadBadge),
}

/// An account's unread state as its server counts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UnreadBadge {
  /// Unread messages and thread replies that mention the user.
  pub mentions: u32,
  /// Conversations with unread messages, and followed threads with unread replies.
  pub unread: u32,
}

/// The watchers of the background accounts, by server origin.
pub struct BackgroundAccounts {
  vault: Arc<SecretVault>,
  on_change: Arc<dyn Fn() + Send + Sync>,
  timing: Timing,
  watchers: Mutex<HashMap<String, Watcher>>,
  states: Arc<Mutex<HashMap<String, BackgroundState>>>,
}

#[derive(Clone, Copy)]
struct Timing {
  first: Duration,
  every: Duration,
}

struct Watcher {
  /// Held for the length of a round; [`BackgroundAccounts::stop`] takes it to wait for one.
  busy: Arc<tokio::sync::Mutex<()>>,
  is_stopped: Arc<AtomicBool>,
  task: JoinHandle<()>,
}

/// One round of a watcher: the state found, or `None` when this round says nothing new (offline).
trait Poll: Send + Sync + 'static {
  fn poll(&self) -> impl Future<Output = Option<BackgroundState>> + Send;
}

/// A background account's own sign-in service.
struct ServicePoll {
  service: AuthService<VaultStore>,
  is_signed_in: AtomicBool,
}

impl BackgroundAccounts {
  /// Watchers that tell the webview of every change.
  pub fn new(vault: Arc<SecretVault>, app: tauri::AppHandle) -> Self {
    use tauri::Emitter;

    let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
      if let Err(error) = app.emit(ACCOUNTS_CHANGED_EVENT, ()) {
        tracing::debug!(%error, "could not tell the webview about the accounts");
      }
    });
    Self::with(
      vault,
      on_change,
      Timing {
        first: FIRST_POLL_DELAY,
        every: POLL_INTERVAL,
      },
    )
  }

  /// Watchers that tell nobody and never touch the keychain unless one is started (tests).
  #[cfg(test)]
  pub(super) fn new_for_tests() -> Self {
    Self::with(
      Arc::new(SecretVault::new(Arc::new(teamcraft_core::store::OsKeystore))),
      Arc::new(|| {}),
      Timing {
        first: FIRST_POLL_DELAY,
        every: POLL_INTERVAL,
      },
    )
  }

  fn with(vault: Arc<SecretVault>, on_change: Arc<dyn Fn() + Send + Sync>, timing: Timing) -> Self {
    Self {
      vault,
      on_change,
      timing,
      watchers: Mutex::new(HashMap::new()),
      states: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  /// Watches every account but the active one (at the start).
  pub fn watch_all(&self, servers: &ServerState) {
    let maybe_active = servers.directory.active().map(|profile| profile.origin);
    for profile in servers.directory.accounts() {
      if Some(&profile.origin) != maybe_active.as_ref() {
        self.watch(DesktopConfig::from_profile(&profile));
      }
    }
  }

  /// Starts watching the account on `config`'s server, unless it is watched already.
  pub fn watch(&self, config: DesktopConfig) {
    let origin = config.server_url.clone();
    let service = match AuthService::new(Some(config), VaultStore::new(Arc::clone(&self.vault))) {
      Ok(service) => service,
      Err(error) => {
        tracing::warn!(%error, "no unread badge for a background account");
        return;
      },
    };
    self.watch_with(
      origin,
      ServicePoll {
        service,
        is_signed_in: AtomicBool::new(false),
      },
    );
  }

  /// The last state found for the account of `origin`; unknown when it isn't watched.
  pub fn state(&self, origin: &str) -> BackgroundState {
    lock(&self.states).get(origin).copied().unwrap_or_default()
  }

  /// Stops watching the account of `origin`. Waits for a round in progress and for the watcher to
  /// end, so the account's session is not in use here any more when this returns, and no state the
  /// watcher found shows after it.
  pub async fn stop(&self, origin: &str) {
    let Some(watcher) = lock(&self.watchers).remove(origin) else {
      return;
    };
    let busy = watcher.busy.lock().await;
    watcher.is_stopped.store(true, Ordering::Release);
    watcher.task.abort();
    drop(busy);
    // Aborted: ends at its next wait, the lock it would need or its pause. The error says so.
    let _ = watcher.task.await;
    lock(&self.states).remove(origin);
  }

  fn watch_with(&self, origin: String, poll: impl Poll) {
    let mut watchers = lock(&self.watchers);
    if watchers.contains_key(&origin) {
      return;
    }
    let busy = Arc::new(tokio::sync::Mutex::new(()));
    let is_stopped = Arc::new(AtomicBool::new(false));
    let task = tauri::async_runtime::spawn(run(
      origin.clone(),
      poll,
      Arc::clone(&busy),
      Arc::clone(&is_stopped),
      Arc::clone(&self.states),
      Arc::clone(&self.on_change),
      self.timing,
    ));
    watchers.insert(
      origin,
      Watcher {
        busy,
        is_stopped,
        task,
      },
    );
  }
}

impl Drop for BackgroundAccounts {
  fn drop(&mut self) {
    for watcher in lock(&self.watchers).values() {
      watcher.is_stopped.store(true, Ordering::Release);
      watcher.task.abort();
    }
  }
}

/// A watcher's loop: a round, then the wait, until it is stopped.
async fn run(
  origin: String,
  poll: impl Poll,
  busy: Arc<tokio::sync::Mutex<()>>,
  is_stopped: Arc<AtomicBool>,
  states: Arc<Mutex<HashMap<String, BackgroundState>>>,
  on_change: Arc<dyn Fn() + Send + Sync>,
  timing: Timing,
) {
  tokio::time::sleep(timing.first).await;
  loop {
    let is_changed = {
      let _round = busy.lock().await;
      // Stopped while waiting for the lock: the session belongs to someone else now.
      if is_stopped.load(Ordering::Acquire) {
        return;
      }
      // Kept inside the round: a stop waits for it, so no state lands after the stop cleared it.
      poll.poll().await.is_some_and(|state| {
        let previous = lock(&states).insert(origin.clone(), state);
        previous != Some(state)
      })
    };
    if is_changed {
      on_change();
    }
    tokio::time::sleep(timing.every).await;
  }
}

impl Poll for ServicePoll {
  async fn poll(&self) -> Option<BackgroundState> {
    if !self.is_signed_in.load(Ordering::Acquire) {
      match self.service.restore().await {
        Ok(true) => self.is_signed_in.store(true, Ordering::Release),
        Ok(false) | Err(AuthError::Denied(_) | AuthError::NotSignedIn | AuthError::KeyLost) => {
          return Some(BackgroundState::SignedOut);
        },
        Err(error) => {
          tracing::debug!(%error, "a background account could not sign in now");
          return None;
        },
      }
    }
    match self.service.api_get(UNREAD_PATH).await {
      Ok(response) if response.status == 200 => {
        match serde_json::from_str::<UnreadResponse>(&response.body) {
          Ok(unread) => Some(BackgroundState::Unread(UnreadBadge::from_response(&unread))),
          Err(error) => {
            tracing::debug!(%error, "unreadable unread counts of a background account");
            None
          },
        }
      },
      Ok(response) => {
        tracing::debug!(
          status = response.status,
          "a background account's server refused the counts"
        );
        if response.status == 401 {
          // The next round signs in again from the stored session, or finds it gone.
          self.is_signed_in.store(false, Ordering::Release);
        }
        None
      },
      Err(error) => {
        tracing::debug!(%error, "no unread counts of a background account");
        if matches!(
          error,
          AuthError::NotSignedIn | AuthError::Denied(_) | AuthError::KeyLost
        ) {
          self.is_signed_in.store(false, Ordering::Release);
        }
        None
      },
    }
  }
}

impl UnreadBadge {
  /// The badge of a `GET /api/v1/unread` answer.
  pub fn from_response(response: &UnreadResponse) -> Self {
    let conversation_mentions = response.conversations.iter().fold(0_u32, |sum, conversation| {
      sum.saturating_add(conversation.mention_count)
    });
    let unread_conversations = response
      .conversations
      .iter()
      .filter(|conversation| conversation.unread_count > 0)
      .count();
    Self {
      mentions: conversation_mentions.saturating_add(response.thread_mention_count),
      unread: u32::try_from(unread_conversations)
        .unwrap_or(u32::MAX)
        .saturating_add(response.unread_thread_count),
    }
  }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::AtomicU32;

  use teamcraft_core::store::OsKeystore;
  use teamcraft_proto::{ConversationId, ConversationUnread};
  use tokio::sync::{Semaphore, mpsc};

  use super::*;

  const ORIGIN: &str = "https://b.example.com";

  /// A round that tells when it began and ends once the test lets it: no timing involved.
  struct GatedPoll {
    entered: mpsc::UnboundedSender<()>,
    release: Arc<Semaphore>,
    finished: Arc<AtomicU32>,
  }

  impl Poll for GatedPoll {
    async fn poll(&self) -> Option<BackgroundState> {
      let _ = self.entered.send(());
      self.release.acquire().await.unwrap().forget();
      self.finished.fetch_add(1, Ordering::SeqCst);
      Some(BackgroundState::Unread(UnreadBadge {
        mentions: 1,
        unread: 2,
      }))
    }
  }

  /// A [`GatedPoll`] whose rounds may run `released` times, the channel of their beginnings and the
  /// count of those that ended.
  fn gated(
    released: usize,
  ) -> (
    GatedPoll,
    mpsc::UnboundedReceiver<()>,
    Arc<Semaphore>,
    Arc<AtomicU32>,
  ) {
    let (entered, rounds) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(released));
    let finished = Arc::new(AtomicU32::new(0));
    let poll = GatedPoll {
      entered,
      release: Arc::clone(&release),
      finished: Arc::clone(&finished),
    };
    (poll, rounds, release, finished)
  }

  fn accounts(changes: Arc<AtomicU32>) -> BackgroundAccounts {
    BackgroundAccounts::with(
      Arc::new(SecretVault::new(Arc::new(OsKeystore))),
      Arc::new(move || {
        changes.fetch_add(1, Ordering::SeqCst);
      }),
      Timing {
        first: Duration::ZERO,
        every: Duration::from_millis(10),
      },
    )
  }

  #[test]
  fn the_badge_counts_mentions_and_unread_places() {
    let conversation = |unread_count, mention_count| ConversationUnread {
      conversation_id: serde_json::from_str::<ConversationId>("\"1\"").unwrap(),
      unread_count,
      mention_count,
    };
    let response = UnreadResponse {
      conversations: vec![conversation(3, 1), conversation(5, 0), conversation(0, 0)],
      unread_thread_count: 2,
      thread_mention_count: 4,
      maybe_max_counted: None,
      maybe_counted_through: None,
    };
    assert_eq!(
      UnreadBadge::from_response(&response),
      UnreadBadge {
        mentions: 5,
        unread: 4
      }
    );
    let json = serde_json::to_value(BackgroundState::Unread(UnreadBadge::from_response(&response))).unwrap();
    assert_eq!(
      json,
      serde_json::json!({ "state": "unread", "mentions": 5, "unread": 4 })
    );
    assert_eq!(
      serde_json::to_value(BackgroundState::SignedOut).unwrap(),
      serde_json::json!({ "state": "signed_out" })
    );
  }

  /// Switching to an account waits for its round: afterwards no round of it runs here, and its
  /// badge is gone (a stopped watcher once wrote it back after the stop).
  #[tokio::test]
  async fn stopping_waits_for_the_round_and_none_follows() {
    let accounts = accounts(Arc::new(AtomicU32::new(0)));
    let (poll, mut rounds, release, finished) = gated(0);
    accounts.watch_with(ORIGIN.to_owned(), poll);
    rounds.recv().await.unwrap();

    // The stop is waiting for the round when the round is let go.
    tokio::join!(accounts.stop(ORIGIN), async { release.add_permits(1) });

    assert_eq!(
      finished.load(Ordering::SeqCst),
      1,
      "the round in progress ended first"
    );
    assert_eq!(accounts.state(ORIGIN), BackgroundState::Unknown);
    // The watcher has ended: nothing holds the channel's sender any more, and no round began.
    assert!(rounds.recv().await.is_none(), "no round after the stop");
  }

  #[tokio::test]
  async fn a_changed_state_is_told_once() {
    let changes = Arc::new(AtomicU32::new(0));
    let accounts = accounts(Arc::clone(&changes));
    let (poll, mut rounds, _release, _finished) = gated(Semaphore::MAX_PERMITS);
    accounts.watch_with(ORIGIN.to_owned(), poll);
    // The third round began: the first two found the same counts and wrote them.
    for _ in 0..3 {
      rounds.recv().await.unwrap();
    }
    assert_eq!(
      changes.load(Ordering::SeqCst),
      1,
      "the same counts again change nothing"
    );
    assert_eq!(
      accounts.state(ORIGIN),
      BackgroundState::Unread(UnreadBadge {
        mentions: 1,
        unread: 2
      })
    );
    accounts.stop(ORIGIN).await;
  }
}
