//! Videos that play while they stream (#200, e2ee.md §4.5): the player loads a URL of the app's own
//! and reads it in ranges; the core reads each range from the encrypted blob and decrypts only the
//! chunks it covers, so a video of any size plays and seeks without the whole file in memory.
//!
//! - Desktop: `http://127.0.0.1:<port>/<token>`, answered by the shell's loopback server
//!   (`core::media_stream`).
//! - Browser: `/media-stream/<token>`, answered by the service worker with what the core in this
//!   page reads (`sw.js`, `web_core::open_media_stream`).
//!
//! Where neither works (the demo data, a page without the worker, a tab that doesn't run the core)
//! [`open`] fails and the caller opens the file whole as before.

use serde::Deserialize;
use wasm_bindgen::JsValue;

use crate::model::{ConversationId, MessageId};

/// A stream the player loads; closed when dropped, so the core forgets the file's key for it.
#[derive(Debug, PartialEq, Eq)]
pub struct StreamUrl {
  url: String,
  token: String,
}

/// The desktop shell's answer to `core_open_media_stream`.
#[derive(Deserialize)]
struct ShellStream {
  url: String,
  token: String,
}

/// Opens a video of a message to stream.
pub async fn open(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<StreamUrl, String> {
  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    return Err("the demo data doesn't stream".to_owned());
  }
  if crate::auth::tauri::is_available() {
    return open_in_shell(conversation, message, file_id).await;
  }
  open_in_page(conversation, message, file_id).await
}

impl StreamUrl {
  pub fn as_str(&self) -> &str {
    &self.url
  }
}

impl Drop for StreamUrl {
  fn drop(&mut self) {
    close(std::mem::take(&mut self.token));
  }
}

async fn open_in_shell(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<StreamUrl, String> {
  let args = serde_json::json!({
    "conversation": conversation,
    "message": message,
    "file_id": file_id,
  });
  let js_args =
    js_sys::JSON::parse(&args.to_string()).map_err(|_| "could not encode the arguments".to_owned())?;
  let answer = crate::auth::tauri::invoke("core_open_media_stream", js_args)
    .await
    .map_err(|error| error.to_string())?;
  let json = js_sys::JSON::stringify(&answer)
    .ok()
    .and_then(|json| json.as_string())
    .ok_or_else(|| "unreadable answer of core_open_media_stream".to_owned())?;
  let stream: ShellStream = serde_json::from_str(&json)
    .map_err(|error| format!("unreadable answer of core_open_media_stream: {error}"))?;
  Ok(StreamUrl {
    url: stream.url,
    token: stream.token,
  })
}

#[cfg(target_arch = "wasm32")]
async fn open_in_page(
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
) -> Result<StreamUrl, String> {
  let token = crate::web_core::open_media_stream(conversation, message, file_id)
    .await?
    .as_str()
    .to_owned();
  Ok(StreamUrl {
    url: format!("/media-stream/{token}"),
    token,
  })
}

#[cfg(not(target_arch = "wasm32"))]
async fn open_in_page(
  _conversation: ConversationId,
  _message: MessageId,
  _file_id: String,
) -> Result<StreamUrl, String> {
  Err("streams need the desktop app or a browser".to_owned())
}

fn close(token: String) {
  if token.is_empty() {
    return;
  }
  if crate::auth::tauri::is_available() {
    let args = js_sys::Object::new();
    // Deliberately ignored: setting a field of a fresh plain object can't fail.
    let _ = js_sys::Reflect::set(&args, &"token".into(), &JsValue::from_str(&token));
    wasm_bindgen_futures::spawn_local(async move {
      if let Err(error) = crate::auth::tauri::invoke("core_close_media_stream", args.into()).await {
        tracing::debug!(%error, "could not close a stream");
      }
    });
    return;
  }
  close_in_page(token);
}

#[cfg(target_arch = "wasm32")]
fn close_in_page(token: String) {
  if let Some(token) = teamcraft_core::media_stream::MediaStreamToken::parse(&token) {
    crate::web_core::close_media_stream(token);
  }
}

#[cfg(not(target_arch = "wasm32"))]
fn close_in_page(_token: String) {}
