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
use super::{CacheIndex, CachedMedia, MAX_ENTRY_BYTES, MediaCacheError, MediaKey, MediaSealer};
use crate::store::browser::idb::{self, RECORDS};
use crate::store::{StoreError, browser_database_key};

const DATA_PREFIX: &str = "d:";
const META_PREFIX: &str = "m:";
/// The key right after every `m:` key, the end of their range.
const META_END: &str = "m;";

/// See the module docs.
pub struct BrowserMediaCache {
  database: IdbDatabase,
  sealer: MediaSealer,
  index: RefCell<CacheIndex>,
  /// Another tab signing out deletes the database: this one lets go, and later reads miss.
  _on_version_change: Closure<dyn FnMut(web_sys::Event)>,
}

impl BrowserMediaCache {
  /// Opens the IndexedDB database `name`, creating it and its key on first use.
  pub async fn open(name: &str) -> Result<Self, MediaCacheError> {
    let key = browser_database_key(name).await?;
    let database = idb::open_database(name).await?;
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

  /// Deletes the database `name` (sign-out); a handle to it must be [`Self::close`]d first.
  pub async fn delete(name: &str) -> Result<(), MediaCacheError> {
    Ok(idb::delete_database(name).await?)
  }

  pub fn close(&self) {
    self.database.close();
  }

  /// The cached entry of `key`, whatever its version. An unreadable entry is deleted and missing.
  pub async fn get(&self, key: &MediaKey) -> Option<CachedMedia> {
    let id = self.sealer.lookup_id(key);
    let sealed = match self.read(&id).await {
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
    match self.sealer.open(key, &sealed) {
      Ok(media) => {
        let now = now_ms();
        self.index.borrow_mut().touch(&id, now);
        // Only the eviction order rests on it.
        if let Err(error) = self.write_meta(&id, sealed.len() as u64, now).await {
          tracing::debug!(%error, "could not mark a media cache entry as used");
        }
        Some(media)
      },
      Err(error) => {
        tracing::warn!(%error, "dropping an unreadable media cache entry");
        if let Err(error) = self.delete_entries(&[id]).await {
          tracing::warn!(%error, "could not delete an unreadable media cache entry");
        }
        None
      },
    }
  }

  /// Keeps `media` as the entry of `key` (replacing an older version) and evicts the least recently
  /// used entries beyond `budget_bytes`. An entry too big to keep only removes the older one.
  pub async fn put(
    &self,
    key: &MediaKey,
    media: &CachedMedia,
    budget_bytes: u64,
  ) -> Result<(), MediaCacheError> {
    let sealed = self.sealer.seal(key, media)?;
    let size_bytes = sealed.len() as u64;
    let id = self.sealer.lookup_id(key);
    if size_bytes > budget_bytes.min(MAX_ENTRY_BYTES) {
      return self.delete_entries(&[id]).await;
    }
    let now = now_ms();
    let evicted = {
      let mut index = self.index.borrow_mut();
      index.insert(id, size_bytes, now);
      index.evict_to(budget_bytes, Some(&id))
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

fn now_ms() -> u64 {
  // Invariant: Date.now() is a non-negative integer of milliseconds, exact in an f64.
  js_sys::Date::now().max(0.0) as u64
}
