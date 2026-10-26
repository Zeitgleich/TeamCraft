//! The persistent media cache of the desktop app (issue #749, `teamcraft_core::media_cache`):
//! sealed files in the app's data directory, one folder per server and user beside the store,
//! encrypted with keys derived from the store's database key. The core opens it together with the
//! store; the webview reads and fills it through the commands here, which never wait for the core
//! thread, so pictures show while it connects or syncs.
//!
//! Pictures cross the bridge as raw bytes: `core_media_get` answers
//! `meta length (u32, big-endian) || meta JSON || bytes` (empty: not cached), `core_media_put` takes
//! the bytes as the body and the rest in headers.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tauri::{AppHandle, State};
use teamcraft_core::media_cache::{CachedMedia, FileMediaCache, MediaKey};
use teamcraft_core::store::{DatabaseKeySource, KeySourceError};
use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
use teamcraft_proto::UserId;

use super::commands::{CommandError, CoreHost, base_dir};

/// Folder of the caches inside a server's core directory.
const MEDIA_CACHE_DIR: &str = "media-cache";
/// Headers of [`core_media_put`]; the webview's `persisted_media` sends the same names.
const KEY_HEADER: &str = "x-media-key";
const VERSION_HEADER: &str = "x-media-version";
const TYPE_HEADER: &str = "x-media-type";
const BUDGET_HEADER: &str = "x-media-budget";

/// The open cache of the running core, shared by the core thread (which opens it) and the commands.
#[derive(Clone, Default)]
pub struct MediaSlot(Arc<Mutex<Option<Arc<FileMediaCache>>>>);

/// A key source that keeps a copy of the key it handed out, so the media cache is opened with the
/// store's key without asking the OS keystore twice.
pub struct KeptKey<'a> {
  source: &'a dyn DatabaseKeySource,
  kept: RefCell<Option<SymmetricKey>>,
}

/// What `core_media_get` says about the bytes after it.
#[derive(Serialize)]
struct MediaMeta<'a> {
  version: &'a str,
  content_type: &'a str,
}

impl MediaSlot {
  /// Opens the cache of `user` beside the store in `data_dir`. A cache that doesn't open leaves the
  /// app without one: pictures then come from the server as before.
  pub fn open(&self, data_dir: &Path, user: UserId, database_key: &SymmetricKey) {
    let dir = data_dir.join(MEDIA_CACHE_DIR).join(user.to_string());
    match FileMediaCache::open(dir, database_key) {
      Ok(cache) => self.set(Some(Arc::new(cache))),
      Err(error) => tracing::warn!(%error, "could not open the media cache"),
    }
  }

  pub fn get(&self) -> Option<Arc<FileMediaCache>> {
    self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  fn set(&self, maybe_cache: Option<Arc<FileMediaCache>>) {
    *self.0.lock().unwrap_or_else(PoisonError::into_inner) = maybe_cache;
  }
}

impl<'a> KeptKey<'a> {
  pub fn new(source: &'a dyn DatabaseKeySource) -> Self {
    Self {
      source,
      kept: RefCell::new(None),
    }
  }

  pub fn take(&self) -> Option<SymmetricKey> {
    self.kept.borrow_mut().take()
  }
}

impl DatabaseKeySource for KeptKey<'_> {
  fn database_key(&self) -> Result<SymmetricKey, KeySourceError> {
    let key = self.source.database_key()?;
    *self.kept.borrow_mut() = Some(key.duplicate());
    Ok(key)
  }
}

/// The cached picture of `key` (any version; the webview compares it), or an empty answer.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_media_get(
  host: State<'_, CoreHost>,
  key: MediaKey,
) -> Result<tauri::ipc::Response, CommandError> {
  let Some(cache) = running_cache(&host) else {
    return Ok(tauri::ipc::Response::new(Vec::new()));
  };
  let maybe_media = tauri::async_runtime::spawn_blocking(move || cache.get(&key))
    .await
    .map_err(|error| CommandError::from(error.to_string()))?;
  Ok(tauri::ipc::Response::new(
    maybe_media.map(|media| frame(&media)).unwrap_or_default(),
  ))
}

/// Keeps a picture the webview just loaded, within the size limit it passes.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_media_put(
  host: State<'_, CoreHost>,
  request: tauri::ipc::Request<'_>,
) -> Result<(), CommandError> {
  let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
    return Err(CommandError::from("a picture is sent as raw bytes".to_owned()));
  };
  let header = |name: &str| {
    request
      .headers()
      .get(name)
      .and_then(|value| value.to_str().ok())
      .map(str::to_owned)
      .ok_or_else(|| CommandError::from(format!("the {name} header is missing")))
  };
  let key: MediaKey = serde_json::from_str(&header(KEY_HEADER)?)
    .map_err(|error| CommandError::from(format!("unreadable media key: {error}")))?;
  let budget_bytes: u64 = header(BUDGET_HEADER)?
    .parse()
    .map_err(|_| CommandError::from("the size limit is not a number".to_owned()))?;
  let media = CachedMedia {
    version: header(VERSION_HEADER)?,
    content_type: header(TYPE_HEADER)?,
    bytes: SecretBytes::new(bytes.clone()),
  };
  let Some(cache) = running_cache(&host) else {
    return Ok(());
  };
  tauri::async_runtime::spawn_blocking(move || cache.put(&key, &media, budget_bytes))
    .await
    .map_err(|error| CommandError::from(error.to_string()))?
    .map_err(|error| CommandError::from(error.to_string()))
}

/// Forgets the picture of `key` (it was removed on the server).
#[tauri::command(rename_all = "snake_case")]
pub async fn core_media_remove(host: State<'_, CoreHost>, key: MediaKey) -> Result<(), CommandError> {
  let Some(cache) = running_cache(&host) else {
    return Ok(());
  };
  tauri::async_runtime::spawn_blocking(move || cache.remove(&key))
    .await
    .map_err(|error| CommandError::from(error.to_string()))?
    .map_err(|error| CommandError::from(error.to_string()))
}

/// Sign-out: closes the running core's cache and deletes the caches of every server and user.
#[tauri::command(rename_all = "snake_case")]
pub async fn core_media_clear(app: AppHandle, host: State<'_, CoreHost>) -> Result<(), CommandError> {
  if let Ok(runtime) = host.current() {
    runtime.media().set(None);
  }
  let core_dir = base_dir(&app)?.join("core");
  tauri::async_runtime::spawn_blocking(move || delete_all(&core_dir))
    .await
    .map_err(|error| CommandError::from(error.to_string()))?
}

fn running_cache(host: &CoreHost) -> Option<Arc<FileMediaCache>> {
  host.current().ok()?.media().get()
}

/// The answer of [`core_media_get`] for a hit.
fn frame(media: &CachedMedia) -> Vec<u8> {
  let meta = MediaMeta {
    version: &media.version,
    content_type: &media.content_type,
  };
  // Invariant: two strings serialize to JSON without fail.
  let meta = serde_json::to_vec(&meta).unwrap_or_default();
  let mut framed = Vec::with_capacity(4 + meta.len() + media.bytes.len());
  framed.extend_from_slice(&u32::try_from(meta.len()).unwrap_or(u32::MAX).to_be_bytes());
  framed.extend_from_slice(&meta);
  framed.extend_from_slice(media.bytes.expose_secret());
  framed
}

/// Deletes the media cache folder of every server under `core_dir`.
fn delete_all(core_dir: &Path) -> Result<(), CommandError> {
  let servers = match std::fs::read_dir(core_dir) {
    Ok(servers) => servers,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(CommandError::from(error.to_string())),
  };
  for server in servers.flatten() {
    let dir: PathBuf = server.path().join(MEDIA_CACHE_DIR);
    FileMediaCache::delete_all(&dir).map_err(|error| CommandError::from(error.to_string()))?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use teamcraft_core::store::StaticKeySource;

  use super::*;

  #[test]
  fn a_hit_is_framed_with_its_version_and_type_before_the_bytes() {
    let media = CachedMedia {
      version: "v1".to_owned(),
      content_type: "image/png".to_owned(),
      bytes: SecretBytes::new(b"png".to_vec()),
    };
    let framed = frame(&media);
    let (length, rest) = framed.split_at(4);
    let length = u32::from_be_bytes(length.try_into().expect("four bytes")) as usize;
    let meta: serde_json::Value = serde_json::from_slice(&rest[..length]).expect("meta");
    assert_eq!(
      meta,
      serde_json::json!({ "version": "v1", "content_type": "image/png" })
    );
    assert_eq!(&rest[length..], b"png");
  }

  #[test]
  fn the_kept_key_is_the_one_the_store_got() {
    let source = StaticKeySource::new(SymmetricKey::from_bytes([3; 32]));
    let kept = KeptKey::new(&source);
    assert!(kept.take().is_none(), "nothing asked yet");
    let handed_out = kept.database_key().expect("key");
    let copy = kept.take().expect("kept");
    assert_eq!(copy.expose_secret(), handed_out.expose_secret());
  }

  #[test]
  fn clearing_deletes_the_caches_of_every_server_and_keeps_the_stores() {
    let unique = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|elapsed| elapsed.as_nanos())
      .unwrap_or_default();
    let root = std::env::temp_dir().join(format!("teamcraft-desktop-media-{}-{unique}", std::process::id()));
    let store = root.join("server-a").join("1.db");
    std::fs::create_dir_all(root.join("server-a").join(MEDIA_CACHE_DIR).join("1")).expect("dirs");
    std::fs::create_dir_all(root.join("server-b").join(MEDIA_CACHE_DIR).join("2")).expect("dirs");
    std::fs::write(&store, b"database").expect("store");
    delete_all(&root).expect("clear");
    assert!(!root.join("server-a").join(MEDIA_CACHE_DIR).exists());
    assert!(!root.join("server-b").join(MEDIA_CACHE_DIR).exists());
    assert!(store.exists(), "the store stays");
    delete_all(&root.join("missing")).expect("nothing there is fine");
    drop(std::fs::remove_dir_all(&root));
  }
}
