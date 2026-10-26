//! The persistent media cache from the page's side (issue #749; `teamcraft-core` `media_cache`).
//!
//! Avatars, space and channel pictures and inline renditions of image attachments are kept on the
//! device, sealed with keys derived from the local database key: in the desktop app by the shell
//! (files in the app's data directory), in the browser in an IndexedDB database of the account.
//! The hubs show a cached picture at once, fetch a newer version in the background and keep the
//! cached one while the server or its storage is down; see [`Freshness`].
//!
//! Every call here is best effort: a cache that can't be read or written only means the picture
//! comes from the server, as without it. Nothing is cached for guests or the demo data.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use futures_util::future::{FutureExt, LocalBoxFuture, Shared};
pub use teamcraft_client_api::media::MediaKey;
#[cfg(target_arch = "wasm32")]
use teamcraft_core::media_cache::BrowserMediaCache;
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::model::UserId;
use crate::settings::{MediaCacheLimit, MediaCacheMaxAge};

/// Headers of the shell's `core_media_put`; the shell reads the same names.
const KEY_HEADER: &str = "x-media-key";
const VERSION_HEADER: &str = "x-media-version";
const TYPE_HEADER: &str = "x-media-type";
const BUDGET_HEADER: &str = "x-media-budget";
const MAX_AGE_HEADER: &str = "x-media-max-age";

#[cfg(target_arch = "wasm32")]
thread_local! {
  /// The account whose cache the page uses, and the cache once opened.
  static BROWSER: RefCell<Option<BrowserAccount>> = const { RefCell::new(None) };
}

/// A cached picture, as the page needs it for a `blob:` URL.
#[derive(Clone)]
pub struct CachedPicture {
  pub version: String,
  pub content_type: String,
  pub bytes: Vec<u8>,
}

/// How a cached picture relates to the version the server announces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
  /// The announced version: nothing to download.
  Current,
  /// Older, or the version is unknown (an older server): shown until the download replaces it,
  /// and kept when the download fails.
  Stale,
}

#[cfg(target_arch = "wasm32")]
struct BrowserAccount {
  user: UserId,
  /// Opening the cache, started by the first use and shared by every later one, so the database
  /// and its key are created once. `None` inside: it didn't open (no IndexedDB, private mode) and
  /// isn't tried again this session.
  maybe_opening: Option<OpeningCache>,
}

#[cfg(target_arch = "wasm32")]
type OpeningCache = Shared<LocalBoxFuture<'static, Option<Rc<BrowserMediaCache>>>>;

impl CachedPicture {
  /// Whether this picture is the announced `maybe_version` (`None`: the server names none).
  pub fn freshness(&self, maybe_version: Option<&str>) -> Freshness {
    match maybe_version {
      Some(version) if version == self.version => Freshness::Current,
      _ => Freshness::Stale,
    }
  }
}

/// The browser keeps the cache of `user` (the signed-in account); the desktop shell knows the
/// account from its core.
pub fn use_account(user: UserId) {
  #[cfg(target_arch = "wasm32")]
  BROWSER.with_borrow_mut(|account| {
    if account.as_ref().is_none_or(|current| current.user != user) {
      *account = Some(BrowserAccount {
        user,
        maybe_opening: None,
      });
    }
  });
  #[cfg(not(target_arch = "wasm32"))]
  let _ = user;
}

/// The cached picture of `key`, whatever its version, unless it is older than the settings keep.
pub async fn get(key: MediaKey) -> Option<CachedPicture> {
  if is_demo() {
    return None;
  }
  let max_age = MediaCacheMaxAge::load().duration();
  if tauri::is_available() {
    return get_from_shell(key, max_age).await;
  }
  get_from_page(key, max_age).await
}

/// Keeps a picture just downloaded or made, within the size limit of the settings. In the
/// background; a failure only means it isn't cached.
pub fn put(key: MediaKey, picture: CachedPicture) {
  if is_demo() {
    return;
  }
  let policy = Policy {
    budget_bytes: MediaCacheLimit::load().bytes(),
    max_age: MediaCacheMaxAge::load().duration(),
  };
  leptos::task::spawn_local(async move {
    let result = if tauri::is_available() {
      put_to_shell(key, picture, policy).await
    } else {
      put_to_page(key, picture, policy).await
    };
    if let Err(error) = result {
      tracing::debug!(%error, "could not cache a picture");
    }
  });
}

/// Forgets the picture of `key`: it was removed on the server.
pub fn remove(key: MediaKey) {
  if is_demo() {
    return;
  }
  leptos::task::spawn_local(async move {
    let result = if tauri::is_available() {
      remove_in_shell(key).await
    } else {
      remove_in_page(key).await
    };
    if let Err(error) = result {
      tracing::debug!(%error, "could not drop a cached picture");
    }
  });
}

/// Sign-out: deletes the cache. Desktop: of every server and account. Browser: of
/// `maybe_user` (the session's account) by name, whether or not this tab opened it, and of every
/// other account the browser lists.
pub async fn clear(maybe_user: Option<UserId>) {
  let result = if tauri::is_available() {
    tauri::invoke("core_media_clear", wasm_bindgen::JsValue::UNDEFINED)
      .await
      .map(|_| ())
      .map_err(|error| error.to_string())
  } else {
    clear_page(maybe_user).await
  };
  if let Err(error) = result {
    tracing::warn!(%error, "could not delete the media cache");
  }
}

/// Signed out at start: deletes cache databases a sign-out in a closed tab may have left (browser;
/// the desktop shell finishes an interrupted deletion itself).
pub async fn sweep_signed_out() {
  if tauri::is_available() || is_demo() {
    return;
  }
  if let Err(error) = sweep_page().await {
    tracing::warn!(%error, "could not delete leftover media caches");
  }
}

/// The renditions whose message went away (the core in the page notes them): out of the cache.
#[cfg(target_arch = "wasm32")]
pub fn drop_renditions(removed: Vec<teamcraft_core::store::RemovedRendition>) {
  for rendition in removed {
    remove(MediaKey::from(rendition));
  }
}

/// The settings a write goes with.
#[derive(Clone, Copy)]
struct Policy {
  budget_bytes: u64,
  max_age: std::time::Duration,
}

async fn get_from_shell(key: MediaKey, max_age: std::time::Duration) -> Option<CachedPicture> {
  let args = serde_json::json!({ "key": key, "max_age_secs": max_age.as_secs() });
  let args = js_sys::JSON::parse(&args.to_string()).ok()?;
  let answer = match tauri::invoke("core_media_get", args).await {
    Ok(answer) => answer,
    Err(error) => {
      tracing::debug!(%error, "could not read the media cache");
      return None;
    },
  };
  let buffer = answer.dyn_into::<js_sys::ArrayBuffer>().ok()?;
  unframe(&js_sys::Uint8Array::new(&buffer).to_vec())
}

async fn put_to_shell(key: MediaKey, picture: CachedPicture, policy: Policy) -> Result<(), String> {
  let key = serde_json::to_string(&key).map_err(|error| error.to_string())?;
  let headers = [
    (KEY_HEADER, key),
    (VERSION_HEADER, picture.version),
    (TYPE_HEADER, picture.content_type),
    (BUDGET_HEADER, policy.budget_bytes.to_string()),
    (MAX_AGE_HEADER, policy.max_age.as_secs().to_string()),
  ];
  let body = js_sys::Uint8Array::from(picture.bytes.as_slice());
  tauri::invoke_raw("core_media_put", &body, &headers)
    .await
    .map(|_| ())
}

async fn remove_in_shell(key: MediaKey) -> Result<(), String> {
  let args = js_sys::JSON::parse(&serde_json::json!({ "key": key }).to_string())
    .map_err(|_| "could not encode the key".to_owned())?;
  tauri::invoke("core_media_remove", args)
    .await
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// The shell's answer to `core_media_get`: `meta length (u32, big-endian) || meta JSON || bytes`,
/// nothing for a miss.
fn unframe(framed: &[u8]) -> Option<CachedPicture> {
  #[derive(serde::Deserialize)]
  struct Meta {
    version: String,
    content_type: String,
  }
  let (length, rest) = framed.split_first_chunk::<4>()?;
  let length = usize::try_from(u32::from_be_bytes(*length)).ok()?;
  if length > rest.len() {
    return None;
  }
  let (meta, bytes) = rest.split_at(length);
  let meta: Meta = serde_json::from_slice(meta).ok()?;
  Some(CachedPicture {
    version: meta.version,
    content_type: meta.content_type,
    bytes: bytes.to_vec(),
  })
}

fn is_demo() -> bool {
  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    return true;
  }
  false
}

#[cfg(target_arch = "wasm32")]
async fn get_from_page(key: MediaKey, max_age: std::time::Duration) -> Option<CachedPicture> {
  let cache = browser_cache().await?;
  let media = cache.get(&key, max_age).await?;
  Some(CachedPicture {
    version: media.version,
    content_type: media.content_type,
    bytes: media.bytes.expose_secret().to_vec(),
  })
}

#[cfg(target_arch = "wasm32")]
async fn put_to_page(key: MediaKey, picture: CachedPicture, policy: Policy) -> Result<(), String> {
  let Some(cache) = browser_cache().await else {
    return Ok(());
  };
  let media = teamcraft_core::media_cache::CachedMedia {
    version: picture.version,
    content_type: picture.content_type,
    bytes: picture.bytes.into(),
  };
  let policy = teamcraft_core::media_cache::CachePolicy {
    budget_bytes: policy.budget_bytes,
    max_age: policy.max_age,
  };
  cache
    .put(&key, &media, policy)
    .await
    .map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn remove_in_page(key: MediaKey) -> Result<(), String> {
  let Some(cache) = browser_cache().await else {
    return Ok(());
  };
  cache.remove(&key).await.map_err(|error| error.to_string())
}

#[cfg(target_arch = "wasm32")]
async fn clear_page(maybe_user: Option<UserId>) -> Result<(), String> {
  let mut users: Vec<UserId> = maybe_user.into_iter().collect();
  if let Some(account) = BROWSER.with_borrow_mut(Option::take) {
    if let Some(Some(cache)) = account.maybe_opening.as_ref().and_then(|opening| opening.peek()) {
      cache.close();
    }
    users.push(account.user);
  }
  users.dedup();
  let mut maybe_failure = None;
  for user in users {
    if let Err(error) = BrowserMediaCache::delete(&BrowserMediaCache::database_name(user)).await {
      maybe_failure = Some(error.to_string());
    }
  }
  if let Err(error) = sweep_page().await {
    maybe_failure = Some(error);
  }
  maybe_failure.map_or(Ok(()), Err)
}

#[cfg(target_arch = "wasm32")]
async fn sweep_page() -> Result<(), String> {
  BrowserMediaCache::delete_every()
    .await
    .map_err(|error| error.to_string())
}

/// The signed-in account's cache, opened on first use.
#[cfg(target_arch = "wasm32")]
async fn browser_cache() -> Option<Rc<BrowserMediaCache>> {
  let opening = BROWSER.with_borrow_mut(|account| {
    let account = account.as_mut()?;
    let user = account.user;
    let opening = account
      .maybe_opening
      .get_or_insert_with(|| open_browser_cache(user).boxed_local().shared());
    Some(opening.clone())
  })?;
  opening.await
}

#[cfg(target_arch = "wasm32")]
async fn open_browser_cache(user: UserId) -> Option<Rc<BrowserMediaCache>> {
  match BrowserMediaCache::open(&BrowserMediaCache::database_name(user)).await {
    Ok(cache) => Some(Rc::new(cache)),
    Err(error) => {
      tracing::info!(%error, "no media cache in this browser");
      None
    },
  }
}

#[cfg(not(target_arch = "wasm32"))]
async fn get_from_page(_key: MediaKey, _max_age: std::time::Duration) -> Option<CachedPicture> {
  None
}

#[cfg(not(target_arch = "wasm32"))]
async fn put_to_page(_key: MediaKey, _picture: CachedPicture, _policy: Policy) -> Result<(), String> {
  Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn remove_in_page(_key: MediaKey) -> Result<(), String> {
  Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn clear_page(_maybe_user: Option<UserId>) -> Result<(), String> {
  Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
async fn sweep_page() -> Result<(), String> {
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_shells_answer_is_unframed_and_a_miss_or_garbage_is_nothing() {
    let meta = br#"{"version":"v2","content_type":"image/png"}"#;
    let mut framed = (meta.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(meta);
    framed.extend_from_slice(b"png bytes");
    let picture = unframe(&framed).expect("a hit");
    assert_eq!(
      (
        picture.version.as_str(),
        picture.content_type.as_str(),
        picture.bytes.as_slice()
      ),
      ("v2", "image/png", &b"png bytes"[..])
    );
    assert!(unframe(&[]).is_none(), "empty: not cached");
    assert!(unframe(&[0, 0, 1, 0, b'{']).is_none(), "truncated");
    assert!(unframe(&[0, 0, 0, 1, b'x']).is_none(), "not JSON");
  }

  #[test]
  fn only_the_announced_version_is_current() {
    let picture = CachedPicture {
      version: "v1".to_owned(),
      content_type: "image/png".to_owned(),
      bytes: Vec::new(),
    };
    assert_eq!(picture.freshness(Some("v1")), Freshness::Current);
    assert_eq!(
      picture.freshness(Some("v2")),
      Freshness::Stale,
      "a new version was announced"
    );
    assert_eq!(
      picture.freshness(None),
      Freshness::Stale,
      "an older server names none"
    );
  }
}
