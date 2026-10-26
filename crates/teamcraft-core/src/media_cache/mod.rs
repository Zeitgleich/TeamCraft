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

pub use teamcraft_client_api::media::{DEFAULT_MEDIA_CACHE_BYTES, MediaKey};
use teamcraft_crypto::secret::SecretBytes;

use crate::store::StoreError;

/// Largest entry kept. An avatar is at most 2 MiB by default and a rendition a few hundred KiB; a
/// bigger file is shown but not cached.
pub const MAX_ENTRY_BYTES: u64 = 8 * 1024 * 1024;

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
