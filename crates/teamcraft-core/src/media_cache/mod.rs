//! The persistent media cache (issue #749): pictures the app shows again and again – avatars, space
//! and channel pictures, inline renditions of image attachments – kept on the device across
//! restarts, so they show at once after a start and stay visible while the server or its object
//! storage is down.
//!
//! - **Keyed by object and version.** One entry per [`MediaKey`]; it holds the version it was made
//!   from, and a new version replaces it. The UI shows a cached older version while it fetches the
//!   new one in the background, and keeps showing it when that fails.
//! - **Encrypted at rest.** Every entry is sealed with keys derived from the local database key
//!   (`teamcraft-crypto::local_store`, own lookup namespace), under a pseudorandom file or record
//!   name, so neither the pictures nor whose they are can be read from the cache. Renditions of
//!   attachments are end-to-end encrypted content; avatars and space pictures are not secret on the
//!   server, but share the one rule (e2ee.md §9).
//! - **Bounded.** Least recently used entries go first once the cache exceeds its size limit (a
//!   setting, [`DEFAULT_MEDIA_CACHE_BYTES`] by default); an entry bigger than
//!   [`MAX_ENTRY_BYTES`] is not kept. The cache is deleted at sign-out.
//!
//! The desktop shell keeps it as files in the app's data directory ([`FileMediaCache`]); the web
//! client in an IndexedDB database of its own with its own wrapped key, like the local store
//! (`BrowserMediaCache`).

#[cfg(target_arch = "wasm32")]
mod browser;
mod entry;
#[cfg(not(target_arch = "wasm32"))]
mod files;
mod index;

#[cfg(target_arch = "wasm32")]
pub use browser::BrowserMediaCache;
pub use entry::MediaSealer;
#[cfg(not(target_arch = "wasm32"))]
pub use files::FileMediaCache;
pub use index::CacheIndex;

pub use teamcraft_client_api::media::{
  DEFAULT_MEDIA_CACHE_BYTES, DEFAULT_MEDIA_MAX_AGE, MAX_MEDIA_CACHE_BYTES, MAX_MEDIA_MAX_AGE, MediaKey,
};
use teamcraft_crypto::secret::SecretBytes;

use crate::store::StoreError;

/// Largest picture kept. An avatar is at most 2 MiB by default and a rendition a few hundred KiB; a
/// bigger file is shown but not cached.
pub const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;
/// Largest sealed entry read back: the picture plus its names and the seal. A bigger file is not
/// this cache's and isn't read into memory.
const MAX_SEALED_BYTES: u64 = MAX_ENTRY_BYTES + 16 * 1024;

/// Whether an entry written at `written_ms` is past `max_age_ms` at `now_ms`. One written "in the
/// future" (the clock went back) counts as new; the last-use expiry still bounds it.
fn is_too_old(written_ms: u64, now_ms: u64, max_age_ms: u64) -> bool {
  now_ms.saturating_sub(written_ms) > max_age_ms
}

/// The limits the page passes with each read and write: the size limit and the longest an entry is
/// kept after it was written. Hosts clamp what they get ([`Self::clamped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CachePolicy {
  pub budget_bytes: u64,
  pub max_age: std::time::Duration,
}

/// One cached picture, decrypted.
pub struct CachedMedia {
  /// The version it was made from: a picture's version, or what a rendition was made with.
  pub version: String,
  /// The media type of `bytes`.
  pub content_type: String,
  pub bytes: SecretBytes,
}

#[derive(Debug, thiserror::Error)]
pub enum MediaCacheError {
  #[error("media cache: {0}")]
  Io(String),
  #[error("media cache: {0}")]
  Store(#[from] StoreError),
  /// An entry that doesn't open (tampered with, another key, another format): dropped.
  #[error("a media cache entry is unreadable")]
  BadEntry,
}

impl CachePolicy {
  /// Within what the settings offer: a page can't make the cache grow past the largest limit or
  /// keep entries longer than the longest age.
  pub fn clamped(self) -> Self {
    Self {
      budget_bytes: self.budget_bytes.min(MAX_MEDIA_CACHE_BYTES),
      max_age: self.max_age.min(MAX_MEDIA_MAX_AGE),
    }
  }

  fn max_age_ms(&self) -> u64 {
    u64::try_from(self.max_age.as_millis()).unwrap_or(u64::MAX)
  }
}

impl Default for CachePolicy {
  fn default() -> Self {
    Self {
      budget_bytes: DEFAULT_MEDIA_CACHE_BYTES,
      max_age: DEFAULT_MEDIA_MAX_AGE,
    }
  }
}

/// The cache entry of a rendition whose message went away.
impl From<crate::store::RemovedRendition> for MediaKey {
  fn from(removed: crate::store::RemovedRendition) -> Self {
    MediaKey::Rendition {
      conversation: removed.conversation,
      message: removed.message,
      attachment: removed.attachment,
      content_sha256: removed.content_sha256,
    }
  }
}

impl std::fmt::Debug for CachedMedia {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("CachedMedia")
      .field("version", &self.version)
      .field("content_type", &self.content_type)
      .field("bytes", &self.bytes)
      .finish()
  }
}

impl From<std::io::Error> for MediaCacheError {
  fn from(error: std::io::Error) -> Self {
    Self::Io(error.to_string())
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use teamcraft_proto::{Id, Snowflake};

  pub(crate) fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).expect("a valid test ID"))
  }
}
