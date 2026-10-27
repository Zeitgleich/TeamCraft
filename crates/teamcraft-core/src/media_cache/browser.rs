//! The media cache in IndexedDB (web): a database of its own per account, apart from the local
//! store, with its own database key wrapped by a non-extractable Web Crypto key exactly like the
//! store's (e2ee.md §9.1). Deleting it never touches the store, and two tabs racing to create the
//! key at most lose each other's entries, which are then misses.
//!
//! Records in the `records` object store: `d:{lookup ID}` holds the sealed entry, `m:{lookup ID}`
//! its size and last use (`"size,millis"`), so opening reads only the small ones. Each tab keeps
//! its own index; entries another tab wrote meanwhile are counted from its next start.

use std::cell::RefCell;

use teamcraft_crypto::local_store::LookupId;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbKeyRange, IdbObjectStore, IdbTransaction, IdbTransactionMode};

use super::entry::{hex_name, lookup_id_of};
use super::{
  CacheIndex, CachePolicy, CachedMedia, MAX_ENTRY_BYTES, MAX_SEALED_BYTES, MediaCacheError, MediaKey,
  MediaSealer, is_too_old,
};
use crate::store::browser::idb::{self, OpenMode, RECORDS};
use crate::store::{StoreError, browser_database_key, browser_database_key_without_upgrade};

const DATA_PREFIX: &str = "d:";
const META_PREFIX: &str = "m:";
/// The key right after every `m:` key, the end of their range.
const META_END: &str = "m;";
/// Every account's database is named this followed by the user ID.
const DATABASE_PREFIX: &str = "teamcraft-media-";

/// See the module docs.
pub struct BrowserMediaCache {
  database: IdbDatabase,
  sealer: MediaSealer,
  index: RefCell<CacheIndex>,
  /// Another tab signing out deletes the database: this one lets go, and later reads miss.
  _on_version_change: Closure<dyn FnMut(web_sys::Event)>,
}

impl BrowserMediaCache {
  /// Opens the IndexedDB database `name`, creating it and its key on first use (a page: upgrades
  /// the database to this version).
  pub async fn open(name: &str) -> Result<Self, MediaCacheError> {
    Self::open_with(name, OpenMode::Upgrade).await
  }

  /// [`Self::open`] for the service worker: at whatever version the database has, never upgrading
  /// it (e2ee.md §9.4).
  pub async fn open_without_upgrade(name: &str) -> Result<Self, MediaCacheError> {
    Self::open_with(name, OpenMode::AsIs).await
  }

  async fn open_with(name: &str, mode: OpenMode) -> Result<Self, MediaCacheError> {
    let key = match mode {
      OpenMode::Upgrade => browser_database_key(name).await?,
      OpenMode::AsIs => browser_database_key_without_upgrade(name).await?,
    };
    let database = idb::open_database_with(name, mode).await?;
    let index = read_index(&database).await?;
    let on_version_change = Closure::<dyn FnMut(web_sys::Event)>::new({
      let database = database.clone();
      move |_| database.close()
    });
    database.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
    Ok(Self {
      database,
      sealer: MediaSealer::new(&key),
      index: RefCell::new(index),
      _on_version_change: on_version_change,
    })
  }

  /// The database name of `user`'s cache.
  pub fn database_name(user: teamcraft_proto::UserId) -> String {
    format!("{DATABASE_PREFIX}{user}")
  }

  /// Deletes the database `name` (sign-out); a handle to it must be [`Self::close`]d first.
  pub async fn delete(name: &str) -> Result<(), MediaCacheError> {
    Ok(idb::delete_database(name).await?)
  }

  /// Deletes every account's cache database this browser lists (sign-out, or a start while signed
  /// out picking up one a closed tab left). Browsers without `indexedDB.databases()` list none;
  /// the caller deletes the signed-in account's by name anyway.
  pub async fn delete_every() -> Result<(), MediaCacheError> {
    let mut maybe_failure = None;
    for name in listed_databases().await {
      if !name.starts_with(DATABASE_PREFIX) {
        continue;
      }
      if let Err(error) = Self::delete(&name).await {
        tracing::warn!(%error, "could not delete a media cache database");
        maybe_failure = Some(error);
      }
    }
    maybe_failure.map_or(Ok(()), Err)
  }

  pub fn close(&self) {
    self.database.close();
  }

  /// The cached entry of `key`, whatever its version, unless it is older than `max_age`. An
  /// unreadable or too old entry is deleted and missing.
  pub async fn get(&self, key: &MediaKey, max_age: std::time::Duration) -> Option<CachedMedia> {
    let id = self.sealer.lookup_id(key);
    let sealed = match self.read(&id).await {
      Ok(Some(sealed)) if sealed.len() as u64 > MAX_SEALED_BYTES => {
        self.drop_logged(id).await;
        return None;
      },
      Ok(Some(sealed)) => sealed,
      Ok(None) => {
        self.index.borrow_mut().remove(&id);
        return None;
      },
      Err(error) => {
        tracing::warn!(%error, "could not read a media cache entry");
        return None;
      },
    };
    let now = now_ms();
    let max_age_ms = u64::try_from(max_age.as_millis()).unwrap_or(u64::MAX);
    match self.sealer.open(key, &sealed) {
      Ok((_, written_ms)) if is_too_old(written_ms, now, max_age_ms) => {
        self.drop_logged(id).await;
        None
      },
      Ok((media, _)) => {
        self.index.borrow_mut().touch(&id, now);
        // Only the eviction order rests on it.
        if let Err(error) = self.write_meta(&id, sealed.len() as u64, now).await {
          tracing::debug!(%error, "could not mark a media cache entry as used");
        }
        Some(media)
      },
      Err(error) => {
        tracing::warn!(%error, "dropping an unreadable media cache entry");
        self.drop_logged(id).await;
        None
      },
    }
  }

  async fn drop_logged(&self, id: LookupId) {
    if let Err(error) = self.delete_entries(&[id]).await {
      tracing::warn!(%error, "could not delete a media cache entry");
    }
  }

  /// Keeps `media` as the entry of `key` (replacing an older version), then drops the entries past
  /// the policy's age and the least recently used ones beyond its size limit. A picture too big to
  /// keep only removes the older version.
  pub async fn put(
    &self,
    key: &MediaKey,
    media: &CachedMedia,
    policy: CachePolicy,
  ) -> Result<(), MediaCacheError> {
    let policy = policy.clamped();
    let id = self.sealer.lookup_id(key);
    if media.bytes.len() as u64 > MAX_ENTRY_BYTES.min(policy.budget_bytes) {
      return self.delete_entries(&[id]).await;
    }
    let now = now_ms();
    let sealed = self.sealer.seal(key, media, now)?;
    let size_bytes = sealed.len() as u64;
    if size_bytes > policy.budget_bytes {
      return self.delete_entries(&[id]).await;
    }
    let evicted = {
      let mut index = self.index.borrow_mut();
      index.insert(id, size_bytes, now);
      let mut evicted = index.take_expired(now, policy.max_age_ms());
      evicted.extend(index.evict_to(policy.budget_bytes, Some(&id)));
      evicted
    };
    let (transaction, store) = self.transaction(IdbTransactionMode::Readwrite)?;
    let data = js_sys::Uint8Array::from(sealed.as_slice());
    store
      .put_with_key(&data, &data_key(&id))
      .map_err(|error| idb::js_error("put", &error))?;
    store
      .put_with_key(&meta_value(size_bytes, now), &meta_key(&id))
      .map_err(|error| idb::js_error("put", &error))?;
    for old in &evicted {
      delete_in(&store, old)?;
    }
    Ok(idb::transaction_done(&transaction).await?)
  }

  pub async fn remove(&self, key: &MediaKey) -> Result<(), MediaCacheError> {
    self.delete_entries(&[self.sealer.lookup_id(key)]).await
  }

  async fn read(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    let (_transaction, store) = self
      .transaction(IdbTransactionMode::Readonly)
      .map_err(|error| StoreError::Database(error.to_string()))?;
    let request = store
      .get(&data_key(id))
      .map_err(|error| idb::js_error("get", &error))?;
    let value = idb::request_result(&request).await?;
    Ok(
      value
        .dyn_into::<js_sys::Uint8Array>()
        .ok()
        .map(|array| array.to_vec()),
    )
  }

  async fn write_meta(&self, id: &LookupId, size_bytes: u64, now: u64) -> Result<(), MediaCacheError> {
    let (transaction, store) = self.transaction(IdbTransactionMode::Readwrite)?;
    store
      .put_with_key(&meta_value(size_bytes, now), &meta_key(id))
      .map_err(|error| idb::js_error("put", &error))?;
    Ok(idb::transaction_done(&transaction).await?)
  }

  async fn delete_entries(&self, ids: &[LookupId]) -> Result<(), MediaCacheError> {
    {
      let mut index = self.index.borrow_mut();
      for id in ids {
        index.remove(id);
      }
    }
    let (transaction, store) = self.transaction(IdbTransactionMode::Readwrite)?;
    for id in ids {
      delete_in(&store, id)?;
    }
    Ok(idb::transaction_done(&transaction).await?)
  }

  fn transaction(
    &self,
    mode: IdbTransactionMode,
  ) -> Result<(IdbTransaction, IdbObjectStore), MediaCacheError> {
    let transaction = self
      .database
      .transaction_with_str_and_mode(RECORDS, mode)
      .map_err(|error| idb::js_error("transaction", &error))?;
    let store = transaction
      .object_store(RECORDS)
      .map_err(|error| idb::js_error("object store", &error))?;
    Ok((transaction, store))
  }
}

impl Drop for BrowserMediaCache {
  fn drop(&mut self) {
    self.database.set_onversionchange(None);
    self.database.close();
  }
}

/// The index from the `m:` records; one that doesn't parse is skipped (and later overwritten).
async fn read_index(database: &IdbDatabase) -> Result<CacheIndex, MediaCacheError> {
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| idb::js_error("transaction", &error))?;
  let store = transaction
    .object_store(RECORDS)
    .map_err(|error| idb::js_error("object store", &error))?;
  let range = IdbKeyRange::bound(&JsValue::from_str(META_PREFIX), &JsValue::from_str(META_END))
    .map_err(|error| idb::js_error("range", &error))?;
  let keys_request = store
    .get_all_keys_with_key(&range)
    .map_err(|error| idb::js_error("index", &error))?;
  let values_request = store
    .get_all_with_key(&range)
    .map_err(|error| idb::js_error("index", &error))?;
  let keys: js_sys::Array = idb::request_result(&keys_request).await?.unchecked_into();
  let values: js_sys::Array = idb::request_result(&values_request).await?.unchecked_into();
  let mut index = CacheIndex::default();
  for (key, value) in keys.iter().zip(values.iter()) {
    let parsed = key
      .as_string()
      .and_then(|key| lookup_id_of(key.strip_prefix(META_PREFIX)?))
      .zip(value.as_string().and_then(|value| parse_meta(&value)));
    if let Some((id, (size_bytes, last_used_ms))) = parsed {
      index.insert(id, size_bytes, last_used_ms);
    }
  }
  Ok(index)
}

fn delete_in(store: &IdbObjectStore, id: &LookupId) -> Result<(), MediaCacheError> {
  for key in [data_key(id), meta_key(id)] {
    store
      .delete(&key)
      .map_err(|error| idb::js_error("delete", &error))?;
  }
  Ok(())
}

fn data_key(id: &LookupId) -> JsValue {
  JsValue::from_str(&format!("{DATA_PREFIX}{}", hex_name(id)))
}

fn meta_key(id: &LookupId) -> JsValue {
  JsValue::from_str(&format!("{META_PREFIX}{}", hex_name(id)))
}

fn meta_value(size_bytes: u64, last_used_ms: u64) -> JsValue {
  JsValue::from_str(&format!("{size_bytes},{last_used_ms}"))
}

fn parse_meta(value: &str) -> Option<(u64, u64)> {
  let (size, used) = value.split_once(',')?;
  Some((size.parse().ok()?, used.parse().ok()?))
}

/// The names `indexedDB.databases()` lists; none where the browser lacks it.
async fn listed_databases() -> Vec<String> {
  let Ok(factory) = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("indexedDB")) else {
    return Vec::new();
  };
  let Ok(list) = js_sys::Reflect::get(&factory, &JsValue::from_str("databases")) else {
    return Vec::new();
  };
  let Some(list) = list.dyn_ref::<js_sys::Function>() else {
    return Vec::new();
  };
  let Ok(promise) = list.call0(&factory) else {
    return Vec::new();
  };
  let Ok(databases) = wasm_bindgen_futures::JsFuture::from(promise.unchecked_into::<js_sys::Promise>()).await
  else {
    return Vec::new();
  };
  js_sys::Array::from(&databases)
    .iter()
    .filter_map(|info| {
      js_sys::Reflect::get(&info, &JsValue::from_str("name"))
        .ok()?
        .as_string()
    })
    .collect()
}

fn now_ms() -> u64 {
  // Invariant: Date.now() is a non-negative integer of milliseconds, exact in an f64.
  js_sys::Date::now().max(0.0) as u64
}
