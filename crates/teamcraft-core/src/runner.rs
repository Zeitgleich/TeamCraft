//! Drives a [`Client`] for an app: keeps the gateway connected (through the
//! [`ConnectionSupervisor`]), flushes the outbox, commits waiting membership changes and runs the
//! UI's commands.
//!
//! The runner is a plain future without `Send` bounds: native apps poll it on a tokio task, the
//! browser with `wasm_bindgen_futures::spawn_local` (ARCHITECTURE.md §7, one engine for all shells).

use std::time::Duration;

use futures_channel::mpsc;
use futures_util::StreamExt;
use futures_util::future::{Either, select};
use teamcraft_proto::GatewayServerMessage;

use crate::api::{ApiError, ServerApi};
use crate::client::Client;
use crate::connection::{ConnectionSettings, ConnectionStatus, ConnectionSupervisor, NetworkChanges};
use crate::platform;
use crate::store::LocalStore;
#[cfg(feature = "ui")]
use crate::ui_backend::CoreCommand;

/// Runs a client until it is stopped; see the module docs.
pub struct SyncRunner<A: ServerApi, S> {
  client: Client<A, S>,
  settings: RunnerSettings,
  commands: mpsc::UnboundedReceiver<RunnerCommand>,
  on_change: std::rc::Rc<dyn Fn()>,
  connection: ConnectionSupervisor<A::Gateway>,
  /// Network changes and wake-ups the app reported; each one reconnects at once.
  maybe_network: Option<mpsc::UnboundedReceiver<()>>,
  /// When the next tick is due. A deadline rather than a fresh sleep per wake-up, so a busy
  /// connection (a frame every few seconds) can't postpone heartbeats until the server drops it.
  next_tick_ms: i64,
  last_account_sync_ms: i64,
  /// When the user's typing was last announced, to send at most one per [`TYPING_INTERVAL_MS`].
  #[cfg(feature = "ui")]
  last_typing: Option<(teamcraft_proto::ConversationId, i64)>,
}

/// The server forwards one typing frame per 3 s and conversation; sending more is wasted.
#[cfg(feature = "ui")]
const TYPING_INTERVAL_MS: i64 = 3_000;

/// Timing of the runner. Configurable; the defaults suit desktop and browser alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerSettings {
  /// How often the runner wakes up without other events (outbox retry, staggered commits).
  pub tick: Duration,
  /// Heartbeats, backoff and timeouts of the gateway connection.
  pub connection: ConnectionSettings,
  /// How often account data (saved items, drafts, saved searches) is pulled from the user's other
  /// devices; local changes are pushed on the next tick regardless.
  pub account_sync: Duration,
}

/// Sends commands to a running [`SyncRunner`].
#[derive(Clone, Debug)]
pub struct RunnerHandle {
  sender: mpsc::UnboundedSender<RunnerCommand>,
}

/// What an app asks the runner to do.
#[derive(Debug)]
pub enum RunnerCommand {
  /// New messages are in the outbox.
  Flush,
  /// A request the UI adapter queued.
  #[cfg(feature = "ui")]
  Ui(CoreCommand),
  /// Ends [`SyncRunner::run`], which hands the client back.
  Stop,
}

/// What woke the runner.
enum Wake {
  Command(Option<RunnerCommand>),
  Frame(Option<Result<GatewayServerMessage, ApiError>>),
  NetworkChanged,
  Tick,
}

impl Default for RunnerSettings {
  fn default() -> Self {
    Self {
      tick: Duration::from_secs(5),
      connection: ConnectionSettings::default(),
      account_sync: Duration::from_secs(60),
    }
  }
}

impl<A: ServerApi, S: LocalStore> SyncRunner<A, S> {
  /// `on_change` runs after anything that may have changed the store (the UI re-reads it).
  pub fn new(
    client: Client<A, S>,
    settings: RunnerSettings,
    on_change: Box<dyn Fn()>,
  ) -> (Self, RunnerHandle) {
    let (sender, commands) = mpsc::unbounded();
    let on_change: std::rc::Rc<dyn Fn()> = on_change.into();
    let mut connection = ConnectionSupervisor::new(settings.connection.clone(), ConnectionStatus::new());
    let notify = on_change.clone();
    connection.set_observer(Box::new(move |_| notify()));
    let runner = Self {
      client,
      settings,
      commands,
      on_change,
      connection,
      maybe_network: None,
      next_tick_ms: i64::MIN,
      last_account_sync_ms: i64::MIN,
      #[cfg(feature = "ui")]
      last_typing: None,
    };
    (runner, RunnerHandle { sender })
  }

  /// Reconnects at once whenever `changes` reports a network change or a wake-up.
  pub fn watch_network(&mut self, changes: &NetworkChanges) {
    self.maybe_network = Some(changes.subscribe());
  }

  /// Reports the connection's state to `status` (kept by the app across runner restarts).
  pub fn share_connection_status(&mut self, status: ConnectionStatus) {
    self.connection.share_status(status);
  }

  /// The gateway connection's state, readable while the runner runs.
  pub fn connection_status(&self) -> ConnectionStatus {
    self.connection.status()
  }

  /// Runs until [`RunnerCommand::Stop`] or until every handle is dropped; returns the client.
  pub async fn run(mut self) -> Client<A, S> {
    self.flush().await;
    loop {
      if self.connection.connect_if_due(&mut self.client).await.is_some() {
        self.changed();
      }
      match self.next_wake().await {
        Wake::Command(None | Some(RunnerCommand::Stop)) => return self.client,
        Wake::Command(Some(command)) => self.on_command(command).await,
        Wake::Frame(maybe_frame) => self.on_frame(maybe_frame).await,
        Wake::NetworkChanged => {
          let now = self.client.now_ms();
          self.connection.network_changed(now).await;
        },
        Wake::Tick => self.on_tick().await,
      }
    }
  }

  async fn next_wake(&mut self) -> Wake {
    let now = self.client.now_ms();
    let deadline = self
      .connection
      .next_deadline_ms()
      .map_or(self.next_tick_ms, |deadline| deadline.min(self.next_tick_ms));
    let until_ms = deadline.saturating_sub(now).max(0);
    let Self {
      commands,
      connection,
      maybe_network,
      ..
    } = self;
    let frame = Box::pin(connection.receive());
    let network = Box::pin(async {
      match maybe_network {
        Some(changes) => changes.next().await,
        None => std::future::pending().await,
      }
    });
    let tick = Box::pin(platform::sleep(Duration::from_millis(until_ms.unsigned_abs())));
    match select(commands.next(), select(select(frame, network), tick)).await {
      Either::Left((maybe_command, _)) => Wake::Command(maybe_command),
      Either::Right((Either::Left((Either::Left((maybe_frame, _)), _)), _)) => Wake::Frame(maybe_frame),
      Either::Right((Either::Left((Either::Right((Some(()), _)), _)), _)) => Wake::NetworkChanged,
      // Whoever reported network changes is gone; carry on without them.
      Either::Right((Either::Left((Either::Right((None, _)), _)), _)) => {
        *maybe_network = None;
        Wake::Tick
      },
      Either::Right((Either::Right(((), _)), _)) => Wake::Tick,
    }
  }

  async fn on_command(&mut self, command: RunnerCommand) {
    match command {
      RunnerCommand::Flush => {},
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(CoreCommand::Typing { conversation }) => {
        self.announce_typing(conversation).await;
        // Nothing to flush: typing changes no stored state.
        return;
      },
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(command) => {
        if let Err(error) = self.client.run_ui_command(command).await {
          tracing::warn!(%error, "UI command failed");
        }
      },
      // Handled by `run`.
      RunnerCommand::Stop => return,
    }
    self.flush().await;
  }

  async fn on_frame(&mut self, maybe_frame: Option<Result<GatewayServerMessage, ApiError>>) {
    if self.connection.on_frame(&mut self.client, maybe_frame).await {
      self.changed();
    }
  }

  async fn on_tick(&mut self) {
    let now = self.client.now_ms();
    self.connection.on_tick(now).await;
    if now < self.next_tick_ms {
      // Woken for the connection only.
      return;
    }
    self.next_tick_ms = now.saturating_add(millis(self.settings.tick));
    if self.client.is_key_bag_dirty()
      || now.saturating_sub(self.last_account_sync_ms) >= millis(self.settings.account_sync)
    {
      self.last_account_sync_ms = now;
      match self.client.sync_account_data().await {
        Ok(()) => self.changed(),
        Err(error) => tracing::debug!(%error, "account data not synced yet"),
      }
      // Owners keep the signed roster fresh (a no-op for everyone else and while it is).
      if let Err(error) = self.client.refresh_workspace_signatures().await {
        tracing::debug!(%error, "roster not re-signed yet");
      }
      if let Err(error) = self.client.check_workspace_head().await {
        tracing::debug!(%error, "workspace head not checked yet");
      }
      // A rotated org recovery key: wrap the backup key to the new one (security review B4).
      if let Err(error) = self.client.rewrap_for_current_org_key().await {
        tracing::debug!(%error, "backup key not wrapped to the new org key yet");
      }
    }
    // Last seen of people who went offline since the last tick, in one request.
    match self.client.refresh_stale_last_seen().await {
      Ok(true) => self.changed(),
      Ok(false) => {},
      Err(error) => tracing::debug!(%error, "reading last seen failed"),
    }
    // Key packages expire; a device that runs for weeks renews them in time (e2ee.md §4.3).
    if let Err(error) = self.client.renew_key_packages_if_due().await {
      tracing::debug!(%error, "key packages not renewed yet");
    }
    // Staggered commits (a colleague's new device) come due without a gateway event.
    if let Err(error) = self.client.apply_pending_changes().await {
      tracing::debug!(%error, "pending membership changes not applied yet");
    }
    self.flush().await;
  }

  async fn flush(&mut self) {
    if let Err(error) = self.client.generate_pending_previews().await {
      tracing::debug!(%error, "link previews not made");
    }
    match self.client.upload_pending().await {
      Ok(uploaded) if uploaded > 0 => self.changed(),
      Ok(_) => {},
      Err(error) => tracing::debug!(%error, "uploads did not finish"),
    }
    match self.client.flush_outbox().await {
      Ok(report) if report.sent > 0 => self.changed(),
      Ok(_) => {},
      Err(error) => tracing::debug!(%error, "outbox flush did not finish"),
    }
  }

  /// Tells the other members that the user types, at most once per [`TYPING_INTERVAL_MS`] and
  /// conversation; offline, it is dropped (a late indicator would be wrong).
  #[cfg(feature = "ui")]
  async fn announce_typing(&mut self, conversation: teamcraft_proto::ConversationId) {
    let now = self.client.now_ms();
    if self
      .last_typing
      .is_some_and(|(last, at)| last == conversation && now.saturating_sub(at) < TYPING_INTERVAL_MS)
    {
      return;
    }
    if !self.connection.is_connected() {
      return;
    }
    self.last_typing = Some((conversation, now));
    if !self
      .connection
      .send(now, &Client::<A, S>::typing_frame(conversation))
      .await
    {
      tracing::debug!("typing not announced");
    }
  }

  fn changed(&self) {
    (self.on_change)();
  }
}

impl RunnerHandle {
  /// Queues a command; returns `false` if the runner has stopped.
  pub fn send(&self, command: RunnerCommand) -> bool {
    self.sender.unbounded_send(command).is_ok()
  }
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
