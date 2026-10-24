//! Tauri commands that expose the core to the webview. Errors cross as plain text.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use teamcraft_client_api::model::{Message, ThreadId};
use teamcraft_client_api::remote::{
  CORE_CHANGED_EVENT, ChatAction, ChatAnswer, ChatQuery, ChatSnapshot, CoreConnection, CoreStatus,
  EnrollRequest, LinkStatus, MessageWindow,
};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_media::effects::normalize_background_file;
use teamcraft_proto::{ConversationId, UserId};

use super::runtime::{CoreConfig, CoreRuntime, KeyChoice, TokenCache};
use crate::auth::commands::Service;
use crate::auth::{AuthService, TokenStore};
use crate::calls::CallHost;

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
      if !matches!(status, CoreStatus::Failed { .. }) {
        return Ok(status);
      }
      tracing::info!("the core failed to open before; starting it again");
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
#[tauri::command]
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
  let data_dir = base_dir(&app)?.join("core").join(server.storage_key()?);
  let status = host
    .start(&**service, &server.server_url, user, |tokens| {
      let config = CoreConfig {
        server_url: server.server_url.clone(),
        data_dir,
        keystore_scope: server.server_url.clone(),
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
      CoreRuntime::spawn(config, user, key, tokens, on_change, calls.gateway_handler())
    })
    .await?;
  Ok(status)
}

#[tauri::command]
pub async fn core_status(host: State<'_, CoreHost>) -> Result<CoreStatus, CommandError> {
  Ok(host.current()?.status().await)
}

/// Where the gateway connection stands, for the banner; answered without waiting for the core.
#[tauri::command]
pub fn core_connection(host: State<'_, CoreHost>) -> Result<CoreConnection, CommandError> {
  Ok(host.current()?.connection_state().into())
}

/// The webview saw the network come back or the window become visible again: the gateway and a
/// running call check their connections at once.
#[tauri::command]
pub fn core_network_changed(host: State<'_, CoreHost>, calls: State<'_, Arc<CallHost>>) {
  host.network_changed();
  calls.network_changed();
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
  use teamcraft_proto::{Id, Snowflake};
  use tokio::io::{AsyncReadExt, AsyncWriteExt};
  use tokio::net::TcpListener;

  use super::*;
  use crate::auth::{DesktopConfig, MemoryStore};

  const TOKEN: &str = "local-token-a";

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
    // A damaged database: opening fails after the store lock was taken.
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

    assert!(matches!(first, CoreStatus::Failed { .. }), "{first:?}");
    assert!(matches!(second, CoreStatus::NeedsEnrollment { .. }), "{second:?}");
    drop(still_held);
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
      argon2: Argon2Settings::default(),
    };
    CoreRuntime::spawn(
      config,
      user(1),
      KeyChoice::Fixed(vec![7; 32]),
      tokens,
      Arc::new(|| {}),
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
