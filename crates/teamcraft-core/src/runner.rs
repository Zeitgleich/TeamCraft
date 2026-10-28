//! Drives a [`Client`] for an app: keeps the gateway connected (through the
//! [`ConnectionSupervisor`]), flushes the outbox, commits waiting membership changes and runs the
//! UI's commands. File uploads and downloads run beside the loop, so a large file holds up neither
//! sends nor the gateway. It reports the device's activity on every new connection, so a resumed
//! session the server timed out comes back online (docs/gateway.md, "Presence").
//!
//! The runner is a plain future without `Send` bounds: native apps poll it on a tokio task, the
//! browser with `wasm_bindgen_futures::spawn_local` (ARCHITECTURE.md §7, one engine for all shells).

use std::time::Duration;

use futures_channel::mpsc;
use futures_util::StreamExt;
use futures_util::future::{Either, select};
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayServerMessage};

use crate::api::{ApiError, ServerApi};
use crate::client::Client;
use crate::client::files::BlobUpload;
use crate::connection::{ConnectionSettings, ConnectionStatus, ConnectionSupervisor, NetworkChanges};
use crate::downloads::{Downloads, Finished};
use crate::platform;
use crate::store::LocalStore;
#[cfg(feature = "ui")]
use crate::typing::TypingAnnouncer;
#[cfg(feature = "ui")]
use crate::ui_backend::CoreCommand;
use crate::upkeep::{self, Upkeep, UploadMode};

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
  upkeep: Upkeep,
  /// The user's own typing, sent at most once per [`crate::typing::TYPING_INTERVAL_MS`].
  #[cfg(feature = "ui")]
  typing: TypingAnnouncer,
  /// The file going up beside the loop.
  maybe_upload: Option<BlobUpload>,
  /// A file may be waiting for its upload: one was queued, a retry came due, or one finished.
  is_upload_due: bool,
  /// Files on their way down beside the loop.
  downloads: Downloads,
  /// The bytes the transfers in flight had moved when the app was last told.
  maybe_reported_bytes: Option<u64>,
}

/// How often the app is told to read again while a file goes up or down, for its progress.
const TRANSFER_PROGRESS: Duration = Duration::from_millis(500);

/// Timing of the runner. Configurable; the defaults suit desktop and browser alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerSettings {
  /// How often the runner wakes up without other events (outbox retry, staggered commits).
  pub tick: Duration,
  /// Heartbeats, backoff and timeouts of the gateway connection.
  pub connection: ConnectionSettings,
  /// How long a failed account data sync waits before it is retried, and how often the roster's
  /// age is checked locally ([`Upkeep`]). Account data is read after each gateway connection and on
  /// `key_bag_update`, local changes are pushed on the next tick.
  pub upkeep_interval: Duration,
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
  /// The device became active or idle (the app came to the foreground or went to the background,
  /// or the user touched it): told to the server now if connected, and on every new connection.
  SetActivity(DeviceActivity),
  /// Ends [`SyncRunner::run`], which hands the client back.
  Stop,
}

/// What woke the runner.
enum Wake {
  Command(Option<RunnerCommand>),
  Frame(Option<Result<GatewayServerMessage, ApiError>>),
  NetworkChanged,
  /// Storage answered the running blob upload.
  Uploaded(Result<(), ApiError>),
  Downloaded(Finished),
  Tick,
}

impl Default for RunnerSettings {
  fn default() -> Self {
    Self {
      tick: Duration::from_secs(5),
      connection: ConnectionSettings::default(),
      upkeep_interval: Duration::from_secs(60),
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
    let upkeep = Upkeep::new(settings.upkeep_interval).with_uploads(UploadMode::Host);
    let runner = Self {
      client,
      settings,
      commands,
      on_change,
      connection,
      maybe_network: None,
      next_tick_ms: i64::MIN,
      upkeep,
      #[cfg(feature = "ui")]
      typing: TypingAnnouncer::default(),
      maybe_upload: None,
      is_upload_due: true,
      downloads: Downloads::default(),
      maybe_reported_bytes: None,
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
        // A resumed session keeps what the server last knew, which may be an away timeout.
        self.report_activity().await;
        // Back online: what waited goes out now, not on the next tick.
        self.is_upload_due = true;
        self.flush().await;
        self.changed();
      }
      self.start_upload().await;
      self.start_downloads().await;
      match self.next_wake().await {
        Wake::Command(None | Some(RunnerCommand::Stop)) => return self.client,
        Wake::Command(Some(command)) => self.on_command(command).await,
        Wake::Frame(maybe_frame) => self.on_frame(maybe_frame).await,
        Wake::NetworkChanged => {
          let now = self.client.now_ms();
          self.connection.network_changed(now).await;
        },
        Wake::Uploaded(sent) => self.on_uploaded(sent).await,
        Wake::Downloaded(finished) => {
          if Downloads::finish(&mut self.client, finished) {
            self.changed();
          }
        },
        Wake::Tick => self.on_tick().await,
      }
      self.report_progress();
    }
  }

  async fn next_wake(&mut self) -> Wake {
    let now = self.client.now_ms();
    let deadline = self
      .connection
      .next_deadline_ms()
      .map_or(self.next_tick_ms, |deadline| deadline.min(self.next_tick_ms));
    // While a file goes up or down, the app reads its progress again every so often.
    let deadline = match self.maybe_reported_bytes {
      Some(_) => deadline.min(now.saturating_add(millis(TRANSFER_PROGRESS))),
      None => deadline,
    };
    // Never longer than a tick: after the wall clock went back, deadlines lie in the "future" by
    // the jump, and the tick notices the jump and moves them (robustness.md, "Time").
    let until_ms = deadline.saturating_sub(now).clamp(0, millis(self.settings.tick));
    let Self {
      commands,
      connection,
      maybe_network,
      maybe_upload,
      downloads,
      ..
    } = self;
    let frame = Box::pin(connection.receive());
    let network = Box::pin(async {
      match maybe_network {
        Some(changes) => changes.next().await,
        None => std::future::pending().await,
      }
    });
    let uploaded = Box::pin(async {
      match maybe_upload {
        Some(upload) => upload.sent().await,
        None => std::future::pending().await,
      }
    });
    let downloaded = Box::pin(downloads.next_finished());
    let tick = Box::pin(platform::sleep(Duration::from_millis(until_ms.unsigned_abs())));
    let transfers = select(uploaded, downloaded);
    match select(
      commands.next(),
      select(select(frame, network), select(transfers, tick)),
    )
    .await
    {
      Either::Left((maybe_command, _)) => Wake::Command(maybe_command),
      Either::Right((Either::Left((Either::Left((maybe_frame, _)), _)), _)) => Wake::Frame(maybe_frame),
      Either::Right((Either::Left((Either::Right((Some(()), _)), _)), _)) => Wake::NetworkChanged,
      // Whoever reported network changes is gone; carry on without them.
      Either::Right((Either::Left((Either::Right((None, _)), _)), _)) => {
        *maybe_network = None;
        Wake::Tick
      },
      Either::Right((Either::Right((Either::Left((Either::Left((sent, _)), _)), _)), _)) => {
        Wake::Uploaded(sent)
      },
      Either::Right((Either::Right((Either::Left((Either::Right((finished, _)), _)), _)), _)) => {
        Wake::Downloaded(finished)
      },
      Either::Right((Either::Right((Either::Right(((), _)), _)), _)) => Wake::Tick,
    }
  }

  async fn on_command(&mut self, command: RunnerCommand) {
    match command {
      RunnerCommand::Flush => {},
      RunnerCommand::SetActivity(activity) => {
        self.client.set_device_activity(activity);
        self.report_activity().await;
        // Nothing to flush: the activity changes no stored state.
        return;
      },
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(CoreCommand::Typing {
        conversation,
        maybe_thread,
      }) => {
        self.announce_typing(conversation, maybe_thread).await;
        // Nothing to flush: typing changes no stored state.
        return;
      },
      // Uploads and saves run beside the loop.
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(CoreCommand::UploadFiles) => self.is_upload_due = true,
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(CoreCommand::DownloadFile {
        conversation,
        attachment,
      }) => self.downloads.save(conversation, attachment),
      #[cfg(feature = "ui")]
      RunnerCommand::Ui(command) => {
        let now = self.client.now_ms();
        // A command can take long (an upload, a big history page); heartbeats go on meanwhile.
        let result = self
          .connection
          .keep_alive(now, self.client.run_ui_command(command))
          .await;
        if let Err(error) = result {
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
    if self.next_tick_ms.saturating_sub(now) > millis(self.settings.tick) {
      // The clock went back: the upkeep runs now, not after the jump.
      self.next_tick_ms = now;
    }
    if now < self.next_tick_ms {
      // Woken for the connection only.
      return;
    }
    self.next_tick_ms = now.saturating_add(millis(self.settings.tick));
    // Retries of failed uploads come due with time.
    self.is_upload_due = true;
    // The upkeep may resync many conversations; heartbeats go on meanwhile.
    let is_changed = self
      .connection
      .keep_alive(now, self.upkeep.tick(&mut self.client))
      .await;
    if is_changed {
      self.changed();
    }
  }

  async fn flush(&mut self) {
    let now = self.client.now_ms();
    let is_changed = self
      .connection
      .keep_alive(now, upkeep::flush_sends(&mut self.client))
      .await;
    if is_changed {
      self.changed();
    }
  }

  /// Tells the other members that the user types (throttled; offline, it is dropped: a late
  /// indicator would be wrong).
  #[cfg(feature = "ui")]
  async fn announce_typing(
    &mut self,
    conversation: teamcraft_proto::ConversationId,
    maybe_thread: Option<teamcraft_proto::ThreadId>,
  ) {
    let now = self.client.now_ms();
    self
      .typing
      .announce(&mut self.connection, conversation, maybe_thread, now)
      .await;
  }

  /// Tells the server what the device last said about its activity; offline, the next connection
  /// does. A failed send replaces the connection, which then reports it again.
  async fn report_activity(&mut self) {
    if !self.connection.is_connected() {
      return;
    }
    let message = GatewayClientMessage::SetActivity {
      activity: self.client.device_activity().clone(),
    };
    let now = self.client.now_ms();
    if !self.connection.send(now, &message).await {
      tracing::debug!("device activity not reported");
    }
  }

  /// Starts the next due upload, if none runs; drops a running one whose file was cancelled (or its
  /// message deleted), which stops it.
  async fn start_upload(&mut self) {
    if !self.is_upload_due {
      return;
    }
    if let Some(upload) = &self.maybe_upload
      && !self.client.is_upload_current(upload)
    {
      self.maybe_upload = None;
      self.changed();
    }
    if self.maybe_upload.is_some() {
      return;
    }
    self.is_upload_due = false;
    let now = self.client.now_ms();
    match self.connection.keep_alive(now, self.client.start_upload()).await {
      Ok(Some(upload)) => {
        self.maybe_upload = Some(upload);
        // The pending message shows the file's thumbnail from now on.
        self.changed();
      },
      Ok(None) => {},
      Err(error) => tracing::debug!(%error, "upload not started"),
    }
  }

  /// The running upload's blob is stored (or failed): the file goes into its message, which is sent
  /// once all its files are up.
  async fn on_uploaded(&mut self, sent: Result<(), ApiError>) {
    let Some(upload) = self.maybe_upload.take() else {
      return;
    };
    let now = self.client.now_ms();
    if let Err(error) = self
      .connection
      .keep_alive(now, self.client.finish_upload(upload, sent))
      .await
    {
      tracing::debug!(%error, "upload not finished");
    }
    self.is_upload_due = true;
    self.flush().await;
    self.changed();
  }

  async fn start_downloads(&mut self) {
    let now = self.client.now_ms();
    if self
      .connection
      .keep_alive(now, self.downloads.start_due(&mut self.client))
      .await
    {
      self.changed();
    }
  }

  /// Tells the app to read again when the transfers in flight moved on.
  fn report_progress(&mut self) {
    let moved = self.client.transfers().moved_bytes();
    if moved != self.maybe_reported_bytes {
      self.maybe_reported_bytes = moved;
      self.changed();
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
