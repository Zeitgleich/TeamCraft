//! The media cache as files (desktop): one sealed file per entry in a directory per account, named
//! by its lookup ID. A file's modification time is its last use, so the eviction order survives a
//! restart without an index file that could disagree with the directory.
//!
//! Blocking file I/O: callers on an async runtime run it on a blocking thread.

use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;

use super::entry::{hex_name, lookup_id_of};
use super::{CacheIndex, CachedMedia, MAX_ENTRY_BYTES, MediaCacheError, MediaKey, MediaSealer};

/// Suffix of a file being written; renamed into place when complete, deleted when found on opening.
const PARTIAL_SUFFIX: &str = ".partial";

/// See the module docs.
pub struct FileMediaCache {
  dir: PathBuf,
  sealer: MediaSealer,
  index: Mutex<CacheIndex>,
}

impl FileMediaCache {
  /// Opens the cache in `dir`, creating it, with keys derived from the local database key.
  pub fn open(dir: impl Into<PathBuf>, database_key: &SymmetricKey) -> Result<Self, MediaCacheError> {
    let dir = dir.into();
    fs::create_dir_all(&dir)?;
    let index = scan(&dir)?;
    Ok(Self {
      dir,
      sealer: MediaSealer::new(database_key),
      index: Mutex::new(index),
    })
  }

  /// Deletes the caches of every account under `root` (sign-out). Nothing there is fine.
  pub fn delete_all(root: &Path) -> Result<(), MediaCacheError> {
    match fs::remove_dir_all(root) {
      Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
      _ => Ok(()),
    }
  }

  /// The cached entry of `key`, whatever its version. An unreadable entry is deleted and missing.
  pub fn get(&self, key: &MediaKey) -> Option<CachedMedia> {
    self.get_at(key, SystemTime::now())
  }

  /// Keeps `media` as the entry of `key` (replacing an older version) and evicts the least recently
  /// used entries beyond `budget_bytes`. An entry too big to keep only removes the older one.
  pub fn put(&self, key: &MediaKey, media: &CachedMedia, budget_bytes: u64) -> Result<(), MediaCacheError> {
    self.put_at(key, media, budget_bytes, SystemTime::now())
  }

  pub fn remove(&self, key: &MediaKey) -> Result<(), MediaCacheError> {
    let id = self.sealer.lookup_id(key);
    self.lock().remove(&id);
    remove_file(&self.path(&id))
  }

  pub fn used_bytes(&self) -> u64 {
    self.lock().used_bytes()
  }

  fn get_at(&self, key: &MediaKey, now: SystemTime) -> Option<CachedMedia> {
    let id = self.sealer.lookup_id(key);
    let path = self.path(&id);
    let sealed = match fs::read(&path) {
      Ok(sealed) => sealed,
      Err(error) => {
        if error.kind() != ErrorKind::NotFound {
          tracing::warn!(%error, "could not read a media cache entry");
        }
        self.lock().remove(&id);
        return None;
      },
    };
    match self.sealer.open(key, &sealed) {
      Ok(media) => {
        self.lock().touch(&id, millis(now));
        // Only the eviction order rests on it; a file system without settable times evicts by age.
        if let Err(error) = File::options()
          .write(true)
          .open(&path)
          .and_then(|file| file.set_modified(now))
        {
          tracing::debug!(%error, "could not mark a media cache entry as used");
        }
        Some(media)
      },
      Err(error) => {
        tracing::warn!(%error, "dropping an unreadable media cache entry");
        self.lock().remove(&id);
        if let Err(error) = remove_file(&path) {
          tracing::warn!(%error, "could not delete an unreadable media cache entry");
        }
        None
      },
    }
  }

  fn put_at(
    &self,
    key: &MediaKey,
    media: &CachedMedia,
    budget_bytes: u64,
    now: SystemTime,
  ) -> Result<(), MediaCacheError> {
    let sealed = self.sealer.seal(key, media)?;
    let size_bytes = sealed.len() as u64;
    if size_bytes > budget_bytes.min(MAX_ENTRY_BYTES) {
      return self.remove(key);
    }
    let id = self.sealer.lookup_id(key);
    let path = self.path(&id);
    let partial = path.with_extension(PARTIAL_SUFFIX.trim_start_matches('.'));
    fs::write(&partial, &sealed)?;
    if let Err(error) = File::options()
      .write(true)
      .open(&partial)
      .and_then(|file| file.set_modified(now))
    {
      tracing::debug!(%error, "could not date a media cache entry");
    }
    fs::rename(&partial, &path)?;
    let evicted = {
      let mut index = self.lock();
      index.insert(id, size_bytes, millis(now));
      index.evict_to(budget_bytes, Some(&id))
    };
    for old in evicted {
      if let Err(error) = remove_file(&self.path(&old)) {
        tracing::warn!(%error, "could not evict a media cache entry");
      }
    }
    Ok(())
  }

  fn path(&self, id: &LookupId) -> PathBuf {
    self.dir.join(hex_name(id))
  }

  fn lock(&self) -> MutexGuard<'_, CacheIndex> {
    // The index is consistent after every statement; a panic elsewhere leaves nothing half-done.
    self.index.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// The index of the entries in `dir`; files a write left behind are deleted, others ignored.
fn scan(dir: &Path) -> Result<CacheIndex, MediaCacheError> {
  let mut index = CacheIndex::default();
  for item in fs::read_dir(dir)? {
    let item = item?;
    let name = item.file_name();
    let Some(name) = name.to_str() else {
      continue;
    };
    if name.ends_with(PARTIAL_SUFFIX) {
      remove_file(&item.path())?;
      continue;
    }
    let Some(id) = lookup_id_of(name) else {
      continue;
    };
    let metadata = item.metadata()?;
    if !metadata.is_file() {
      continue;
    }
    let used = metadata.modified().unwrap_or(UNIX_EPOCH);
    index.insert(id, metadata.len(), millis(used));
  }
  Ok(index)
}

fn remove_file(path: &Path) -> Result<(), MediaCacheError> {
  match fs::remove_file(path) {
    Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
    _ => Ok(()),
  }
}

fn millis(time: SystemTime) -> u64 {
  let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
  u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::secret::SecretBytes;
  use teamcraft_proto::{AttachmentId, SpaceId, UserId};

  use super::super::tests::id;
  use super::*;

  const PICTURE: &[u8] = b"a decrypted rendition of an attachment";

  #[test]
  fn an_entry_survives_reopening_and_is_encrypted_on_disk() {
    let dir = TestDir::new("survives");
    let key = MediaKey::Rendition(id::<AttachmentId>(1));
    let cache = FileMediaCache::open(dir.path(), &database_key(1)).expect("open");
    cache
      .put(&key, &media("inline-960", PICTURE), BUDGET)
      .expect("put");
    drop(cache);

    for file in fs::read_dir(dir.path()).expect("list") {
      let file = file.expect("file");
      let name = file.file_name().into_string().expect("utf-8");
      assert!(
        lookup_id_of(&name).is_some(),
        "named by a pseudorandom ID: {name}"
      );
      let bytes = fs::read(file.path()).expect("read");
      assert!(
        !bytes.windows(PICTURE.len()).any(|window| window == PICTURE),
        "no plaintext on disk"
      );
    }

    let reopened = FileMediaCache::open(dir.path(), &database_key(1)).expect("reopen");
    let hit = reopened.get(&key).expect("still cached after a restart");
    assert_eq!(hit.version, "inline-960");
    assert_eq!(hit.bytes.expose_secret(), PICTURE);
    assert!(reopened.used_bytes() > PICTURE.len() as u64);
  }

  #[test]
  fn another_database_key_reads_nothing_and_drops_the_entry() {
    let dir = TestDir::new("other-key");
    let key = MediaKey::Avatar(id::<UserId>(1));
    FileMediaCache::open(dir.path(), &database_key(1))
      .expect("open")
      .put(&key, &media("v1", PICTURE), BUDGET)
      .expect("put");
    let other = FileMediaCache::open(dir.path(), &database_key(2)).expect("open");
    assert!(other.get(&key).is_none());
  }

  #[test]
  fn a_tampered_entry_is_a_miss_and_is_deleted() {
    let dir = TestDir::new("tampered");
    let key = MediaKey::Avatar(id::<UserId>(1));
    let cache = FileMediaCache::open(dir.path(), &database_key(1)).expect("open");
    cache.put(&key, &media("v1", PICTURE), BUDGET).expect("put");
    let path = cache.path(&cache.sealer.lookup_id(&key));
    let mut bytes = fs::read(&path).expect("read");
    if let Some(last) = bytes.last_mut() {
      *last ^= 1;
    }
    fs::write(&path, bytes).expect("tamper");
    assert!(cache.get(&key).is_none());
    assert!(!path.exists(), "deleted, so it isn't read again");
    assert_eq!(cache.used_bytes(), 0);
  }

  #[test]
  fn a_new_version_replaces_the_old_one() {
    let dir = TestDir::new("versions");
    let key = MediaKey::SpaceImage(id::<SpaceId>(1));
    let cache = FileMediaCache::open(dir.path(), &database_key(1)).expect("open");
    cache
      .put(&key, &media("v1", b"old picture"), BUDGET)
      .expect("put v1");
    assert_eq!(cache.get(&key).expect("v1").version, "v1");
    cache
      .put(&key, &media("v2", b"new picture"), BUDGET)
      .expect("put v2");
    let hit = cache.get(&key).expect("v2");
    assert_eq!(
      (hit.version.as_str(), hit.bytes.expose_secret()),
      ("v2", &b"new picture"[..])
    );
    assert_eq!(
      fs::read_dir(dir.path()).expect("list").count(),
      1,
      "one file per key"
    );
  }

  #[test]
  fn the_least_recently_used_entries_are_evicted_beyond_the_limit_also_after_a_restart() {
    let dir = TestDir::new("eviction");
    let keys: Vec<MediaKey> = (1..=3)
      .map(|n| MediaKey::Rendition(id::<AttachmentId>(n)))
      .collect();
    let picture = vec![7u8; 1000];
    let cache = FileMediaCache::open(dir.path(), &database_key(1)).expect("open");
    let entry_bytes = {
      cache
        .put_at(&keys[0], &media("r", &picture), BUDGET, at(1))
        .expect("put");
      cache.used_bytes()
    };
    cache
      .put_at(&keys[1], &media("r", &picture), BUDGET, at(2))
      .expect("put");
    // The first one is read again, so the second is now the least recently used.
    assert!(cache.get_at(&keys[0], at(3)).is_some());
    drop(cache);

    let reopened = FileMediaCache::open(dir.path(), &database_key(1)).expect("reopen");
    let budget = entry_bytes * 2;
    reopened
      .put_at(&keys[2], &media("r", &picture), budget, at(4))
      .expect("put");
    assert!(
      reopened.get_at(&keys[1], at(5)).is_none(),
      "the least recently used went"
    );
    assert!(reopened.get_at(&keys[0], at(5)).is_some());
    assert!(reopened.get_at(&keys[2], at(5)).is_some());
    assert!(reopened.used_bytes() <= budget);
  }

  #[test]
  fn an_entry_bigger_than_the_limit_is_not_kept_and_drops_its_older_version() {
    let dir = TestDir::new("too-big");
    let key = MediaKey::Avatar(id::<UserId>(1));
    let cache = FileMediaCache::open(dir.path(), &database_key(1)).expect("open");
    cache.put(&key, &media("v1", b"small"), 1000).expect("put");
    cache.put(&key, &media("v2", &[1u8; 2000]), 1000).expect("put");
    assert!(
      cache.get(&key).is_none(),
      "an outdated version isn't served for good"
    );
    assert_eq!(cache.used_bytes(), 0);
  }

  #[test]
  fn deleting_all_caches_clears_them_and_leftovers_are_swept() {
    let root = TestDir::new("sign-out");
    let dir = root.path().join("1");
    let key = MediaKey::Avatar(id::<UserId>(1));
    FileMediaCache::open(&dir, &database_key(1))
      .expect("open")
      .put(&key, &media("v1", PICTURE), BUDGET)
      .expect("put");
    fs::write(dir.join(format!("{}{PARTIAL_SUFFIX}", "ab".repeat(32))), b"half").expect("leftover");
    fs::write(dir.join("unrelated"), b"x").expect("unrelated");
    let reopened = FileMediaCache::open(&dir, &database_key(1)).expect("reopen");
    assert!(!dir.join(format!("{}{PARTIAL_SUFFIX}", "ab".repeat(32))).exists());
    drop(reopened);

    FileMediaCache::delete_all(root.path()).expect("sign-out");
    assert!(!root.path().exists());
    FileMediaCache::delete_all(root.path()).expect("nothing left is fine");
    let fresh = FileMediaCache::open(&dir, &database_key(1)).expect("open after sign-out");
    assert!(fresh.get(&key).is_none());
  }

  const BUDGET: u64 = 1024 * 1024;

  fn media(version: &str, bytes: &[u8]) -> CachedMedia {
    CachedMedia {
      version: version.to_owned(),
      content_type: "image/jpeg".to_owned(),
      bytes: SecretBytes::new(bytes.to_vec()),
    }
  }

  fn database_key(seed: u8) -> SymmetricKey {
    SymmetricKey::from_bytes([seed; 32])
  }

  fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
  }

  /// A fresh directory of its own (as the store tests use), removed when dropped.
  struct TestDir(PathBuf);

  impl TestDir {
    fn new(name: &str) -> Self {
      let unique = rand_core::RngCore::next_u64(&mut rand_core::OsRng);
      Self(std::env::temp_dir().join(format!("teamcraft-media-cache-{name}-{unique}")))
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      drop(fs::remove_dir_all(&self.0));
    }
  }
}
