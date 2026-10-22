//! Drives a [`Client`] for an app: keeps the gateway connected (with backoff), sends heartbeats,
//! flushes the outbox, commits waiting membership changes and runs the UI's commands.
//!
//! The runner is a plain future without `Send` bounds: native apps poll it on a tokio task, the
//! browser with `wasm_bindgen_futures::spawn_local` (ARCHITECTURE.md §7, one engine for all shells).

use std::time::Duration;

use futures_channel::mpsc;
use futures_util::StreamExt;
use futures_util::future::{Either, select};
use teamcraft_proto::{GatewayClientMessage, GatewayServerMessage};

use crate::api::{ApiError, GatewayConnection, ServerApi};
use crate::client::Client;
use crate::error::CoreError;
use crate::platform;
use crate::store::LocalStore;
#[cfg(feature = "ui")]
use crate::ui_backend::CoreCommand;

/// Runs a client until it is stopped; see the module docs.
pub struct SyncRunner<A: ServerApi, S> {
  client: Client<A, S>,
  settings: RunnerSettings,
  commands: mpsc::UnboundedReceiver<RunnerCommand>,
  on_change: Box<dyn Fn()>,
  maybe_gateway: Option<A::Gateway>,
  backoff: Duration,
  reconnect_at_ms: i64,
  last_heartbeat_ms: i64,
  last_account_sync_ms: i64,
}

/// Timing of the runner. Configurable; the defaults suit desktop and browser alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerSettings {
  /// How often the runner wakes up without other events (outbox retry, staggered commits).
  pub tick: Duration,
  /// Gateway heartbeat interval (the server's `hello` suggests one; it allows some slack).
  pub heartbeat: Duration,
  pub reconnect_min: Duration,
  pub reconnect_max: Duration,
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
  Tick,
}

impl Default for RunnerSettings {
  fn default() -> Self {
    Self {
      tick: Duration::from_secs(5),
      heartbeat: Duration::from_secs(25),
      reconnect_min: Duration::from_secs(1),
      reconnect_max: Duration::from_secs(30),
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
    let backoff = settings.reconnect_min;
    let runner = Self {
      client,
      settings,
      commands,
      on_change,
      maybe_gateway: None,
      backoff,
      reconnect_at_ms: i64::MIN,
      last_heartbeat_ms: i64::MIN,
      last_account_sync_ms: i64::MIN,
    };
    (runner, RunnerHandle { sender })
  }

  /// Runs until [`RunnerCommand::Stop`] or until every handle is dropped; returns the client.
  pub async fn run(mut self) -> Client<A, S> {
    self.flush().await;
    loop {
      if self.maybe_gateway.is_none() && self.client.now_ms() >= self.reconnect_at_ms {
        self.connect().await;
      }
      match self.next_wake().await {
        Wake::Command(None | Some(RunnerCommand::Stop)) => return self.client,
        Wake::Command(Some(command)) => self.on_command(command).await,
        Wake::Frame(maybe_frame) => self.on_frame(maybe_frame).await,
        Wake::Tick => self.on_tick().await,
      }
    }
  }

  async fn next_wake(&mut self) -> Wake {
    let Self {
      commands,
      maybe_gateway,
      settings,
      ..
    } = self;
    let frame = Box::pin(async {
      match maybe_gateway {
        Some(connection) => connection.receive().await,
        None => std::future::pending().await,
      }
    });
    let tick = Box::pin(platform::sleep(settings.tick));
    match select(commands.next(), select(frame, tick)).await {
      Either::Left((maybe_command, _)) => Wake::Command(maybe_command),
      Either::Right((Either::Left((maybe_frame, _)), _)) => Wake::Frame(maybe_frame),
      Either::Right((Either::Right(((), _)), _)) => Wake::Tick,
    }
  }

  async fn connect(&mut self) {
    match self.client.connect_gateway().await {
      Ok((connection, outcome)) => {
        tracing::info!(?outcome, "gateway connected");
        self.maybe_gateway = Some(connection);
        self.backoff = self.settings.reconnect_min;
        self.last_heartbeat_ms = self.client.now_ms();
        self.changed();
      },
      Err(error) => {
        tracing::warn!(%error, "gateway connection failed, retrying");
        self.schedule_reconnect();
      },
    }
  }

  async fn on_command(&mut self, command: RunnerCommand) {
    match command {
      RunnerCommand::Flush => {},
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
    let outcome = match maybe_frame {
      Some(Ok(frame)) => self.client.handle_frame(frame).await,
      Some(Err(error)) => Err(CoreError::Api(error)),
      None => Err(CoreError::Gateway("connection closed".into())),
    };
    match outcome {
      Ok(()) => self.changed(),
      Err(error) => {
        tracing::warn!(%error, "gateway ended");
        self.schedule_reconnect();
      },
    }
  }

  async fn on_tick(&mut self) {
    let now = self.client.now_ms();
    if let Some(connection) = self.maybe_gateway.as_mut()
      && now.saturating_sub(self.last_heartbeat_ms) >= millis(self.settings.heartbeat)
    {
      self.last_heartbeat_ms = now;
      if let Err(error) = connection.send(&GatewayClientMessage::Heartbeat).await {
        tracing::warn!(%error, "heartbeat failed");
        self.schedule_reconnect();
      }
    }
    if self.client.is_key_bag_dirty()
      || now.saturating_sub(self.last_account_sync_ms) >= millis(self.settings.account_sync)
    {
      self.last_account_sync_ms = now;
      match self.client.sync_account_data().await {
        Ok(()) => self.changed(),
        Err(error) => tracing::debug!(%error, "account data not synced yet"),
      }
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

  fn schedule_reconnect(&mut self) {
    self.maybe_gateway = None;
    self.reconnect_at_ms = self.client.now_ms().saturating_add(millis(self.backoff));
    self.backoff = (self.backoff * 2).min(self.settings.reconnect_max);
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
