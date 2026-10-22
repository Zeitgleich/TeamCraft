//! Tauri commands that expose the core to the webview. Errors cross as plain text.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreStatus, EnrollRequest, LinkStatus,
  MessageWindow,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_media::effects::normalize_background_file;
use teamcraft_proto::{ConversationId, UserId};

use super::runtime::{CoreConfig, CoreRuntime, KeyChoice, TokenCache};
use crate::auth::DesktopConfig;
use crate::calls::CallHost;

/// Holds the running core, if a user is signed in.
pub struct CoreHost {
  config: DesktopConfig,
  tokens: TokenCache,
  running: Mutex<Option<(UserId, Arc<CoreRuntime>)>>,
}

impl CoreHost {
  pub fn new(config: DesktopConfig, tokens: TokenCache) -> Self {
    Self {
      config,
      tokens,
      running: Mutex::new(None),
    }
  }

  pub(crate) fn current(&self) -> Result<Arc<CoreRuntime>, String> {
    self
      .running
      .lock()
      .map_err(|_| "core state is poisoned".to_owned())?
      .as_ref()
      .map(|(_, runtime)| runtime.clone())
      .ok_or_else(|| "the core has not been started".to_owned())
  }
}

#[derive(Debug, Serialize)]
pub struct CommandError(String);

impl From<String> for CommandError {
  fn from(message: String) -> Self {
    Self(message)
  }
}

/// Starts (or reuses) the core for `user` and reports where it stands.
#[tauri::command]
pub async fn core_start(
  app: AppHandle,
  host: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
  user: UserId,
) -> Result<CoreStatus, CommandError> {
  let existing = {
    let running = host
      .running
      .lock()
      .map_err(|_| "core state is poisoned".to_owned())?;
    running
      .as_ref()
      .filter(|(current, _)| *current == user)
      .map(|(_, runtime)| runtime.clone())
  };
  let runtime = match existing {
    Some(runtime) => runtime,
    None => {
      let data_dir = data_dir(&app)?;
      let config = CoreConfig {
        server_url: host.config.server_url.clone(),
        data_dir,
        keystore_scope: host.config.issuer.clone(),
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
      let runtime = Arc::new(CoreRuntime::spawn(
        config,
        user,
        key,
        host.tokens.clone(),
        on_change,
        calls.gateway_handler(),
      ));
      // Replacing a running core (another account signed in) drops, and so stops, the old one.
      *host
        .running
        .lock()
        .map_err(|_| "core state is poisoned".to_owned())? = Some((user, runtime.clone()));
      runtime
    },
  };
  Ok(runtime.status().await)
}

#[tauri::command]
pub async fn core_status(host: State<'_, CoreHost>) -> Result<CoreStatus, CommandError> {
  Ok(host.current()?.status().await)
}

#[tauri::command]
pub async fn core_enroll(host: State<'_, CoreHost>, request: EnrollRequest) -> Result<(), CommandError> {
  Ok(host.current()?.enroll(request).await?)
}

#[tauri::command]
pub async fn core_link_start(host: State<'_, CoreHost>, device_name: String) -> Result<(), CommandError> {
  Ok(host.current()?.link_start(device_name).await?)
}

#[tauri::command]
pub async fn core_link_status(host: State<'_, CoreHost>) -> Result<LinkStatus, CommandError> {
  Ok(host.current()?.link_status().await)
}

#[tauri::command]
pub fn core_link_cancel(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  host.current()?.link_cancel();
  Ok(())
}

#[tauri::command]
pub async fn core_approval_start(host: State<'_, CoreHost>, code: String) -> Result<(), CommandError> {
  Ok(host.current()?.approval_start(code).await?)
}

#[tauri::command]
pub async fn core_approval_confirm(
  host: State<'_, CoreHost>,
  short_code: String,
) -> Result<(), CommandError> {
  Ok(host.current()?.approval_confirm(short_code).await?)
}

#[tauri::command]
pub fn core_approval_cancel(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  host.current()?.approval_cancel();
  Ok(())
}

#[tauri::command]
pub async fn core_snapshot(host: State<'_, CoreHost>) -> Result<ChatSnapshot, CommandError> {
  Ok(host.current()?.snapshot().await?)
}

#[tauri::command]
pub async fn core_messages(
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  start: usize,
  len: usize,
) -> Result<MessageWindow, CommandError> {
  Ok(host.current()?.messages(conversation, start, len).await?)
}

#[tauri::command]
pub async fn core_thread(
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  thread: ThreadId,
) -> Result<Vec<Message>, CommandError> {
  Ok(host.current()?.thread(conversation, thread).await?)
}

#[tauri::command]
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
#[tauri::command]
pub async fn core_query(host: State<'_, CoreHost>, query: ChatQuery) -> Result<ChatAnswer, CommandError> {
  Ok(host.current()?.query(query).await?)
}

/// Stops the core, e.g. on sign-out. The encrypted database stays on disk.
#[tauri::command]
pub fn core_stop(host: State<'_, CoreHost>) -> Result<(), CommandError> {
  let stopped = host
    .running
    .lock()
    .map_err(|_| "core state is poisoned".to_owned())?
    .take();
  // Dropping joins the thread; do it outside the lock.
  drop(stopped);
  Ok(())
}

/// `TEAMCRAFT_DATA_DIR` overrides the platform's app data directory.
fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
  let base = match std::env::var("TEAMCRAFT_DATA_DIR") {
    Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
    _ => app.path().app_data_dir().map_err(|error| error.to_string())?,
  };
  Ok(base.join("core"))
}
