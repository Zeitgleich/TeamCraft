//! Tauri commands that expose the core to the webview. Errors cross as plain text.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::FilePath;
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{AttachmentPriority, LinkRequestView};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, CORE_FAILED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreConnection,
  CoreStatus, EnrollRequest, LinkStatus, MessageWindow, PhoneLinkStatus,
};
use teamcraft_client_api::snapshot_changes::{SnapshotChanges, SnapshotCursor};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_media::effects::normalize_background_file;
use teamcraft_proto::{ConversationId, MessageId, UserId};
use tokio::sync::oneshot::error::RecvError;

use super::runtime::{CoreConfig, CoreRuntime, FailureHandler, KeyChoice, SaveTo, TokenCache};
use crate::auth::commands::Service;
use crate::auth::{AuthService, TokenStore};
use crate::calls::CallHost;

/// Headers of [`core_stage_file`]; the webview's `large_files` sends the same names.
const STAGE_ID_HEADER: &str = "x-stage-id";
const STAGE_OFFSET_HEADER: &str = "x-stage-offset";

/// Holds the running core, if a user is signed in.
pub struct CoreHost {
  tokens: TokenCache,
  /// The server (origin), the user and their core.
  running: Mutex<Option<(String, UserId, Arc<CoreRuntime>)>>,
}

impl CoreHost {
  pub fn new(tokens: TokenCache) -> Self {
    Self {
      tokens,
      running: Mutex::new(None),
    }
  }

  /// The network changed or the computer woke up: the running core checks its connection now.
  pub fn network_changed(&self) {
    if let Ok(runtime) = self.current() {
      runtime.network_changed();
    }
  }

  /// Where the running core's gateway connection stands; `None` without a running core.
  pub fn connection_state(&self) -> Option<teamcraft_core::connection::ConnectionState> {
    self.current().ok().map(|runtime| runtime.connection_state())
  }

  /// Starts the core of `user` on `server_url` with `spawn`, or reuses the running one. A core
  /// that failed to open is replaced, so trying again really tries again.
  pub(crate) async fn start<S: TokenStore>(
    &self,
    service: &AuthService<S>,
    server_url: &str,
    user: UserId,
    spawn: impl FnOnce(TokenCache) -> CoreRuntime,
  ) -> Result<CoreStatus, String> {
    // Refreshes a token that is about to run out before the core asks for it; without one the
    // core waits for it on its own.
    if let Err(error) = service.server_bearer_token().await {
      tracing::debug!(%error, "no fresh access token before starting the core");
    }
    if let Some(runtime) = self.running_for(server_url, user)? {
      let status = runtime.status().await;
      if !matches!(
        status,
        CoreStatus::Failed { .. } | CoreStatus::StoreUnreadable { .. } | CoreStatus::Stopped
      ) {
        return Ok(status);
      }
      tracing::info!(
        ?status,
        "the core failed to open or stopped before; starting it again"
      );
      // Stopped here, not when the last handle goes: the new core needs the store lock now.
      runtime.shutdown();
      drop(runtime);
      self.stop()?;
    }
    let runtime = Arc::new(spawn(self.tokens.clone()));
    // Replacing a running core (another account signed in) drops, and so stops, the old one.
    let previous = self
      .running
      .lock()
      .map_err(|_| "core state is poisoned".to_owned())?
      .replace((server_url.to_owned(), user, runtime.clone()));
    // Dropping joins the thread; do it outside the lock.
    drop(previous);
    Ok(runtime.status().await)
  }

  /// Stops the core. The encrypted database stays on disk.
  pub(crate) fn stop(&self) -> Result<(), String> {
    let stopped = self
      .running
      .lock()
      .map_err(|_| "core state is poisoned".to_owned())?
      .take();
    // Dropping joins the thread; do it outside the lock.
    drop(stopped);
    Ok(())
  }

  pub(crate) fn current(&self) -> Result<Arc<CoreRuntime>, String> {
    self
      .running
      .lock()
      .map_err(|_| "core state is poisoned".to_owned())?
      .as_ref()
      .map(|(_, _, runtime)| runtime.clone())
      .ok_or_else(|| "the core has not been started".to_owned())
  }

  fn running_for(&self, server_url: &str, user: UserId) -> Result<Option<Arc<CoreRuntime>>, String> {
    Ok(
      self
        .running
        .lock()
        .map_err(|_| "core state is poisoned".to_owned())?
        .as_ref()
        .filter(|(current_server, current, _)| current_server == server_url && *current == user)
        .map(|(_, _, runtime)| runtime.clone()),
    )
  }
}

#[derive(Debug, Serialize)]
pub struct CommandError(String);

impl From<String> for CommandError {
  fn from(message: String) -> Self {
    Self(message)
  }
}

/// Starts (or reuses) the core for `user` of the active server and reports where it stands.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_start(
  app: AppHandle,
  host: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
  service: State<'_, Arc<Service>>,
  user: UserId,
) -> Result<CoreStatus, CommandError> {
  let server = service
    .active_server()
    .ok_or_else(|| "no server is chosen".to_owned())?;
  // Everything local is per server: the store (with the workspace root it pinned) and its key.
  let core_dir = base_dir(&app)?.join("core");
  super::media::finish_pending_clear(&core_dir);
  let data_dir = core_dir.join(server.storage_key()?);
  let status = host
    .start(&**service, &server.server_url, user, |tokens| {
      let config = CoreConfig {
        server_url: server.server_url.clone(),
        data_dir,
        keystore_scope: server.server_url.clone(),
        maybe_download_dir: app.path().download_dir().ok(),
        argon2: Argon2Settings::default(),
      };
      let key = KeyChoice::Keyring {
        service: "de.zeitgleich.teamcraft.db".to_owned(),
        account: format!("{}|{user}", config.keystore_scope),
      };
      let emitter = app.clone();
      let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Err(error) = emitter.emit(CORE_CHANGED_EVENT, ()) {
          tracing::debug!(%error, "could not notify the webview");
        }
      });
      let emitter = app.clone();
      let on_failure: FailureHandler = Arc::new(move |failure| {
        if let Err(error) = emitter.emit(CORE_FAILED_EVENT, failure) {
          tracing::debug!(%error, "could not tell the webview about a failure");
        }
      });
      CoreRuntime::spawn(
        config,
        user,
        key,
        tokens,
        on_change,
        on_failure,
        calls.gateway_handler(),
      )
    })
    .await?;
  Ok(status)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_status(host: State<'_, CoreHost>) -> Result<CoreStatus, CommandError> {
  Ok(host.current()?.status().await)
}

/// Where the gateway connection stands, for the banner; answered without waiting for the core.
#[tauri::command(rename_all = "snake_case")]
pub fn core_connection(host: State<'_, CoreHost>) -> Result<CoreConnection, CommandError> {
  Ok(host.current()?.connection_state().into())
}

/// The webview saw the network come back or the window become visible again: the gateway checks
/// its connection at once. A running call is left alone: it notices a new route to the SFU itself
/// (and wake-up tells it in `main.rs`), while moving it to a new socket on every focus restarted
/// its media path each time the window was focused.
#[tauri::command(rename_all = "snake_case")]
pub fn core_network_changed(host: State<'_, CoreHost>) {
  host.network_changed();
}

/// The user opened a conversation (issue #247: a running catch-up takes it next).
#[tauri::command(rename_all = "snake_case")]
pub fn core_focus(host: State<'_, CoreHost>, conversation: ConversationId) {
  // No running core: nothing is catching up that the focus could steer.
  if let Ok(runtime) = host.current() {
    runtime.focus(conversation);
  }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_enroll(host: State<'_, CoreHost>, request: EnrollRequest) -> Result<(), CommandError> {
  Ok(host.current()?.enroll(request).await?)
}

// The web core and the dev bridge take the same snake_case argument names.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_start(host: State<'_, CoreHost>, device_name: String) -> Result<(), CommandError> {
  Ok(host.current()?.link_start(device_name).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_status(host: State<'_, CoreHost>) -> Result<LinkStatus, CommandError> {
  Ok(host.current()?.link_status().await)
}

#[tauri::command(rename_all = "snake_case")]
pub fn core_link_cancel(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  host.current()?.link_cancel();
  Ok(())
}

/// Signed-in device: shows a code a new phone scans (issue #998).
#[tauri::command(rename_all = "snake_case")]
pub async fn core_phone_link_start(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  Ok(host.current()?.phone_link_start().await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_phone_link_status(host: State<'_, CoreHost>) -> Result<PhoneLinkStatus, CommandError> {
  Ok(host.current()?.phone_link_status().await)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_approval_start(host: State<'_, CoreHost>, code: String) -> Result<(), CommandError> {
  Ok(host.current()?.approval_start(code).await?)
}

// The web core and the dev bridge take the same snake_case argument names.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_approval_confirm(
  host: State<'_, CoreHost>,
  short_code: String,
) -> Result<(), CommandError> {
  Ok(host.current()?.approval_confirm(short_code).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub fn core_approval_cancel(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  host.current()?.approval_cancel();
  Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_announce(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  Ok(host.current()?.link_announce().await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_requests(host: State<'_, CoreHost>) -> Result<Vec<LinkRequestView>, CommandError> {
  Ok(host.current()?.link_requests().await)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_request_review(host: State<'_, CoreHost>, id: String) -> Result<(), CommandError> {
  Ok(host.current()?.link_request_review(id).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_link_request_reject(host: State<'_, CoreHost>, id: String) -> Result<(), CommandError> {
  Ok(host.current()?.link_request_reject(id).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_snapshot(host: State<'_, CoreHost>) -> Result<ChatSnapshot, CommandError> {
  Ok(host.current()?.snapshot().await?)
}

/// What changed in the snapshot since the webview's cursor (issue #246).
#[tauri::command(rename_all = "snake_case")]
pub async fn core_snapshot_changes(
  host: State<'_, CoreHost>,
  since: Option<SnapshotCursor>,
) -> Result<SnapshotChanges, CommandError> {
  Ok(host.current()?.snapshot_changes(since).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_messages(
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  start: usize,
  len: usize,
) -> Result<MessageWindow, CommandError> {
  Ok(host.current()?.messages(conversation, start, len).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_thread(
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  thread: ThreadId,
) -> Result<Vec<Message>, CommandError> {
  Ok(host.current()?.thread(conversation, thread).await?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn core_action(host: State<'_, CoreHost>, action: ChatAction) -> Result<(), CommandError> {
  let action = match action {
    // A picture of the user's own: re-encoded here (decoded, scaled to Full HD, JPEG without
    // metadata such as the photo's location) before it is encrypted and uploaded.
    ChatAction::AddBackgroundImage { name, picture } => {
      let picture =
        tauri::async_runtime::spawn_blocking(move || normalize_background_file(picture.as_slice()))
          .await
          .map_err(|error| error.to_string())?
          .map_err(|error| error.to_string())?;
      ChatAction::AddBackgroundImage {
        name,
        picture: picture.into(),
      }
    },
    ChatAction::AddOrgBackground { name, picture } => {
      let picture =
        tauri::async_runtime::spawn_blocking(move || normalize_background_file(picture.as_slice()))
          .await
          .map_err(|error| error.to_string())?
          .map_err(|error| error.to_string())?;
      ChatAction::AddOrgBackground {
        name,
        picture: picture.into(),
      }
    },
    other => other,
  };
  Ok(host.current()?.action(action).await?)
}

/// Search, saved items, files, message extras and the other reads of client-api's `ChatQuery`.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_query(host: State<'_, CoreHost>, query: ChatQuery) -> Result<ChatAnswer, CommandError> {
  Ok(host.current()?.query(query).await?)
}

/// One file of a message, decrypted, as raw bytes (no JSON) for the image viewer. The bytes stay in
/// memory; the webview turns them into a `blob:` URL it revokes when the viewer moves on.
// The web core and the dev bridge take the same snake_case argument names.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_attachment(
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  // A webview from before priorities sends none: a preview.
  priority: Option<AttachmentPriority>,
) -> Result<tauri::ipc::Response, CommandError> {
  let plaintext = host
    .current()?
    .attachment(conversation, message, file_id, priority.unwrap_or_default())
    .await?;
  Ok(tauri::ipc::Response::new(plaintext.expose_secret().to_vec()))
}

/// "Save…" and "Download" on a file (#719): asks where to put it in the system's save dialog, then
/// the core downloads and decrypts it to exactly that path beside its loop (the file's transfer
/// state shows the progress and the result, a failure with its reason). Answers `false` when the
/// user cancelled the dialog; a dialog that couldn't open is an error, not a cancel (#807).
#[tauri::command(rename_all = "snake_case")]
pub async fn core_save_attachment(
  window: tauri::WebviewWindow,
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  file_name: String,
) -> Result<bool, CommandError> {
  let runtime = host.current()?;
  let maybe_path = ask_save_path(&window, &file_name).await.map_err(|error| {
    tracing::warn!(%error, conversation_id = %conversation, "no file saved");
    CommandError(error.to_string())
  })?;
  let Some(path) = maybe_path else {
    tracing::debug!(conversation_id = %conversation, "the save dialog was cancelled");
    return Ok(false);
  };
  // Never the path: it names the file.
  tracing::info!(conversation_id = %conversation, message_id = %message, "saving a file to the picked place");
  runtime
    .save_attachment(conversation, message, file_id, SaveTo::File(path))
    .await?;
  Ok(true)
}

/// "Download all" under several files (#1014): asks once for a folder, then the core downloads and
/// decrypts every file into it under its own name (a taken name gets a number), a few at a time
/// beside its loop; each file's card shows its progress and result. Answers how many files are on
/// their way, `0` when the user cancelled the dialog. One dialog for all files: a save dialog per
/// file competed for the window's one sheet, and only the first file was saved.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_save_attachments(
  window: tauri::WebviewWindow,
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  message: MessageId,
  file_ids: Vec<String>,
) -> Result<usize, CommandError> {
  let runtime = host.current()?;
  let maybe_folder = ask_folder(&window).await.map_err(|error| {
    tracing::warn!(%error, conversation_id = %conversation, "no files saved");
    CommandError(error.to_string())
  })?;
  let Some(folder) = maybe_folder else {
    tracing::debug!(conversation_id = %conversation, "the folder dialog was cancelled");
    return Ok(0);
  };
  tracing::info!(
    conversation_id = %conversation,
    message_id = %message,
    files = file_ids.len(),
    "saving files to the picked folder"
  );
  let mut queued = 0;
  let mut maybe_failure = None;
  for file_id in file_ids {
    match runtime
      .save_attachment(conversation, message, file_id, SaveTo::Folder(folder.clone()))
      .await
    {
      Ok(()) => queued += 1,
      Err(error) => maybe_failure = Some(error),
    }
  }
  match maybe_failure {
    // A file that couldn't even be queued has no card state to show it: the UI says so.
    Some(error) => Err(CommandError(error)),
    None => Ok(queued),
  }
}

/// Why the save dialog gave no place to save to, though the user didn't cancel it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum SaveDialogError {
  /// The dialog never answered: it couldn't open, or it failed while open.
  #[error("the save dialog could not be opened")]
  NotOpened,
  /// The dialog answered with something that is not a file on this device.
  #[error("the save dialog answered with a place that is not a file on this device")]
  NotLocal,
}

/// The path the user picks in the system's save dialog, starting in the downloads folder with
/// `file_name`; `None` when cancelled.
async fn ask_save_path(
  window: &tauri::WebviewWindow,
  file_name: &str,
) -> Result<Option<PathBuf>, SaveDialogError> {
  use tauri_plugin_dialog::DialogExt;

  let (sender, receiver) = tokio::sync::oneshot::channel();
  let mut dialog = window
    .dialog()
    .file()
    .set_parent(window)
    .set_file_name(save_name(file_name));
  if let Ok(downloads) = window.path().download_dir() {
    dialog = dialog.set_directory(downloads);
  }
  dialog.save_file(move |maybe_path| drop(sender.send(maybe_path)));
  picked_path(receiver.await)
}

/// The folder the user picks in the system's folder dialog, starting in the downloads folder;
/// `None` when cancelled.
async fn ask_folder(window: &tauri::WebviewWindow) -> Result<Option<PathBuf>, SaveDialogError> {
  use tauri_plugin_dialog::DialogExt;

  let (sender, receiver) = tokio::sync::oneshot::channel();
  let mut dialog = window.dialog().file().set_parent(window);
  if let Ok(downloads) = window.path().download_dir() {
    dialog = dialog.set_directory(downloads);
  }
  dialog.pick_folder(move |maybe_path| drop(sender.send(maybe_path)));
  picked_path(receiver.await)
}

/// What the save dialog's answer means: `None` when the user cancelled. A dropped answer (the
/// dialog couldn't open on the main thread, or failed) is an error the user is told about, not a
/// cancel that does nothing (#807).
fn picked_path(answer: Result<Option<FilePath>, RecvError>) -> Result<Option<PathBuf>, SaveDialogError> {
  match answer {
    Err(_) => Err(SaveDialogError::NotOpened),
    Ok(None) => Ok(None),
    Ok(Some(picked)) => picked
      .into_path()
      .map(Some)
      .map_err(|_| SaveDialogError::NotLocal),
  }
}

/// The name the save dialog suggests: the sender's file name without path separators or control
/// characters, so it can't point the dialog elsewhere.
pub(crate) fn save_name(file_name: &str) -> String {
  let cleaned: String = file_name
    .chars()
    .map(|character| match character {
      '/' | '\\' | ':' => '_',
      character if character.is_control() => '_',
      character => character,
    })
    .collect();
  let trimmed = cleaned.trim().trim_start_matches('.');
  match trimmed.is_empty() {
    true => "file".to_owned(),
    false => trimmed.to_owned(),
  }
}

/// One chunk of a large file the webview stages for its upload (issue #153), as a raw body with
/// the copy's ID and the chunk's offset in headers; answers the staged copy's path, which the
/// message's file then names.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_stage_file(
  host: State<'_, CoreHost>,
  request: tauri::ipc::Request<'_>,
) -> Result<String, CommandError> {
  let tauri::ipc::InvokeBody::Raw(chunk) = request.body() else {
    return Err(CommandError("a staged chunk is sent as raw bytes".into()));
  };
  let header = |name: &str| {
    request
      .headers()
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
      .ok_or_else(|| CommandError(format!("the {name} header is missing")))
  };
  let id = header(STAGE_ID_HEADER)?;
  let offset: u64 = header(STAGE_OFFSET_HEADER)?
    .parse()
    .map_err(|_| CommandError("the offset is not a number".into()))?;
  let directory = host.current()?.staging_dir().to_path_buf();
  let path = super::staging::append_chunk(&directory, &id, offset, chunk)
    .map_err(|error| CommandError(error.to_string()))?;
  Ok(path.to_string_lossy().into_owned())
}

/// Sets an unreadable local database aside and opens a new one (issue #160).
#[tauri::command(rename_all = "snake_case")]
pub async fn core_reset_store(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  Ok(host.current()?.reset_store().await?)
}

/// Stops the core, e.g. on sign-out. The encrypted database stays on disk.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_stop(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  // Signing out of a device that isn't set up yet leaves no keys of the attempt behind (#168).
  if let Ok(runtime) = host.current() {
    runtime.forget_unfinished_setup().await;
  }
  Ok(host.stop()?)
}

/// The app's data directory; `TEAMCRAFT_DATA_DIR` overrides the platform's.
pub fn base_dir(app: &AppHandle) -> Result<PathBuf, String> {
  match std::env::var("TEAMCRAFT_DATA_DIR") {
    Ok(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
    _ => app.path().app_data_dir().map_err(|error| error.to_string()),
  }
}

#[cfg(test)]
mod tests {
  // Test-only module: a failing step should abort the test with a clear message.
  #![allow(clippy::expect_used, clippy::unwrap_used)]

  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::{SystemTime, UNIX_EPOCH};

  use secrecy::SecretString;
  use teamcraft_client_api::remote::StoreProblem;
  use teamcraft_proto::{Id, Snowflake};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  use super::*;
  use crate::auth::{DesktopConfig, MemoryStore};

  const TOKEN: &str = "local-token-a";

  /// The save dialog's answer: a cancel does nothing; a dialog that never answered, or a place that
  /// isn't a local file, is an error the UI shows (#807).
  #[test]
  fn a_save_dialog_that_never_answers_is_an_error_not_a_cancel() {
    let (sender, receiver) = tokio::sync::oneshot::channel::<Option<FilePath>>();
    drop(sender);
    assert_eq!(
      picked_path(receiver.blocking_recv()),
      Err(SaveDialogError::NotOpened)
    );
    assert_eq!(picked_path(Ok(None)), Ok(None));
    let picked = std::env::temp_dir().join("picked.wav");
    assert_eq!(
      picked_path(Ok(Some(FilePath::Path(picked.clone())))),
      Ok(Some(picked))
    );
    let remote = FilePath::Url(url::Url::parse("https://example.com/picked.wav").unwrap());
    assert_eq!(picked_path(Ok(Some(remote))), Err(SaveDialogError::NotLocal));
  }

  #[test]
  fn the_suggested_save_name_stays_a_file_name() {
    assert_eq!(save_name("beach photo.jpg"), "beach photo.jpg");
    assert_eq!(save_name("../../.bashrc"), "_.._.bashrc");
    assert_eq!(save_name("C:\\Users\\a.txt"), "C__Users_a.txt");
    assert_eq!(save_name("line\nbreak.txt"), "line_break.txt");
    assert_eq!(save_name("..."), "file");
    assert_eq!(save_name(""), "file");
  }

  /// A new device signs in and the UI starts the core at once, before any refresh loop copied the
  /// token: the core reads the session itself and offers enrollment (the server has no identity).
  #[tokio::test]
  async fn a_core_started_right_after_sign_in_offers_enrollment() {
    let (server_url, authorized) = spawn_not_found_server().await;
    let service = signed_in_service(&server_url).await;
    let host = CoreHost::new(TokenCache::live(service.clone()));
    let directory = scratch_dir("enroll");

    let status = host
      .start(&service, &server_url, user(1), |tokens| {
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("start");

    assert_eq!(
      status,
      CoreStatus::NeedsEnrollment {
        has_existing_identity: false,
        was_data_lost: false,
        can_restore_on_device: false,
      }
    );
    assert!(
      authorized.load(Ordering::SeqCst) > 0,
      "the token reached the server"
    );
    host.stop().expect("stop");
    drop(std::fs::remove_dir_all(&directory));
  }

  #[tokio::test]
  async fn a_failed_core_is_replaced_on_the_next_start() {
    let (server_url, _) = spawn_not_found_server().await;
    let service = signed_in_service(&server_url).await;
    let host = CoreHost::new(TokenCache::live(service.clone()));
    let directory = scratch_dir("replace");
    std::fs::create_dir_all(&directory).expect("dir");
    // A damaged database: opening fails after the store lock was taken, and the user is asked what
    // to do with it (issue #160).
    let database = directory.join(format!("{}.db", user(1)));
    std::fs::write(&database, b"not a database").expect("file");

    let first = host
      .start(&service, &server_url, user(1), |tokens| {
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("first start");
    // Another handle to the failed core (a command still running) must not keep the lock.
    let still_held = host.current().expect("failed core");
    let second = host
      .start(&service, &server_url, user(1), |tokens| {
        std::fs::remove_file(&database).expect("remove the damaged database");
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("second start");

    assert!(
      matches!(
        first,
        CoreStatus::StoreUnreadable {
          problem: StoreProblem::Damaged
        }
      ),
      "{first:?}"
    );
    assert!(matches!(second, CoreStatus::NeedsEnrollment { .. }), "{second:?}");
    drop(still_held);
    host.stop().expect("stop");
    drop(std::fs::remove_dir_all(&directory));
  }

  #[tokio::test]
  async fn an_unreadable_database_is_set_aside_and_the_device_set_up_again() {
    let (server_url, _) = spawn_not_found_server().await;
    let service = signed_in_service(&server_url).await;
    let host = CoreHost::new(TokenCache::live(service.clone()));
    let directory = scratch_dir("reset");
    std::fs::create_dir_all(&directory).expect("dir");
    let database = directory.join(format!("{}.db", user(1)));
    std::fs::write(&database, b"not a database").expect("file");
    let status = host
      .start(&service, &server_url, user(1), |tokens| {
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("start");
    assert!(matches!(status, CoreStatus::StoreUnreadable { .. }), "{status:?}");

    let core = host.current().expect("core");
    core.reset_store().await.expect("reset");
    let after = core.status().await;
    assert!(
      matches!(
        after,
        CoreStatus::NeedsEnrollment {
          was_data_lost: true,
          ..
        }
      ),
      "set up again, and told why: {after:?}"
    );
    // The old file is kept beside the new database, untouched.
    let kept: Vec<_> = std::fs::read_dir(&directory)
      .expect("list")
      .filter_map(|entry| entry.ok())
      .filter(|entry| entry.file_name().to_string_lossy().contains(".db.unreadable-"))
      .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read(kept[0].path()).expect("read"), b"not a database");
    // A readable database is never reset.
    assert!(core.reset_store().await.is_err());
    drop(core);
    host.stop().expect("stop");
    drop(std::fs::remove_dir_all(&directory));
  }

  #[tokio::test]
  async fn a_stopped_core_reports_stopped_and_is_replaced_on_the_next_start() {
    let (server_url, _) = spawn_not_found_server().await;
    let service = signed_in_service(&server_url).await;
    let host = CoreHost::new(TokenCache::live(service.clone()));
    let directory = scratch_dir("stopped");
    host
      .start(&service, &server_url, user(1), |tokens| {
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("first start");
    // Its thread gone, as after a panic: an internal error, not a keystore problem.
    let gone = host.current().expect("running core");
    gone.shutdown();
    let stopped = gone.status().await;
    drop(gone);

    let restarted = host
      .start(&service, &server_url, user(1), |tokens| {
        runtime(&server_url, directory.clone(), tokens)
      })
      .await
      .expect("second start");

    assert_eq!(stopped, CoreStatus::Stopped);
    assert!(
      matches!(restarted, CoreStatus::NeedsEnrollment { .. }),
      "{restarted:?}"
    );
    host.stop().expect("stop");
    drop(std::fs::remove_dir_all(&directory));
  }

  #[tokio::test]
  async fn a_core_without_a_token_waits_instead_of_failing() {
    let (server_url, _) = spawn_not_found_server().await;
    let directory = scratch_dir("no-token");
    let core = runtime(&server_url, directory.clone(), TokenCache::default());

    let status = core.status().await;

    assert_eq!(status, CoreStatus::NotStarted);
    drop(core);
    drop(std::fs::remove_dir_all(&directory));
  }

  /// Answers every request with `404`, counting those that carried the session's bearer token.
  async fn spawn_not_found_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let authorized = Arc::new(AtomicUsize::new(0));
    let counter = authorized.clone();
    tokio::spawn(async move {
      while let Ok((mut stream, _)) = listener.accept().await {
        let counter = counter.clone();
        tokio::spawn(async move {
          let mut request = Vec::new();
          let mut buffer = [0u8; 1024];
          while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer).await {
              Ok(0) | Err(_) => return,
              Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
          }
          let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
          if text.contains(&format!("authorization: bearer {TOKEN}")) {
            counter.fetch_add(1, Ordering::SeqCst);
          }
          let answer = "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
          drop(stream.write_all(answer.as_bytes()).await);
        });
      }
    });
    (format!("http://{address}"), authorized)
  }

  /// Signed in with a stored local session, as after a local-account sign-in.
  async fn signed_in_service(server_url: &str) -> Arc<AuthService<MemoryStore>> {
    let config = DesktopConfig {
      server_url: server_url.to_owned(),
      issuer: "https://idp.example.com".into(),
      client_id: "teamcraft".into(),
      scopes: Vec::new(),
    };
    let store = MemoryStore::default();
    let now_ms = i64::try_from(
      SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis(),
    )
    .expect("millis");
    let session = format!(r#"{{"token":"{TOKEN}","expires_at_ms":{}}}"#, now_ms + 3_600_000);
    store
      .save_local_session(server_url, &SecretString::from(session))
      .await
      .expect("save");
    let service = Arc::new(AuthService::new(Some(config), store).expect("service"));
    assert!(service.restore().await.expect("restore"), "signed in");
    service
  }

  fn runtime(server_url: &str, data_dir: PathBuf, tokens: TokenCache) -> CoreRuntime {
    let config = CoreConfig {
      server_url: server_url.to_owned(),
      data_dir,
      keystore_scope: "test".into(),
      maybe_download_dir: None,
      argon2: Argon2Settings::default(),
    };
    CoreRuntime::spawn(
      config,
      user(1),
      KeyChoice::Fixed(vec![7; 32]),
      tokens,
      Arc::new(|| {}),
      Arc::new(|_| {}),
      Arc::new(|_| {}),
    )
  }

  fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("teamcraft-core-start-{}-{name}", std::process::id()))
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
