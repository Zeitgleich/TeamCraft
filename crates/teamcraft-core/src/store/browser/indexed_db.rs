//! [`LocalStore`] on IndexedDB: a [`SealedStore`] whose records live in one object store
//! (e2ee.md §9.1).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads every record's ciphertext into
//! memory, which the store decrypts only when a record is asked for (issue #155, #778; #1127 in
//! docs/performance.md says why it can't fetch per conversation instead); writes are queued
//! and issued together in one transaction a moment later ([`BATCH_DELAY`], or at once when much is
//! queued or on [`IndexedDbStore::flush`]). Of several writes of one record in that time only the
//! last is issued: a catch-up rewrites some records after every group it processes, and one
//! transaction per write left the browser writing for minutes after the catch-up was done
//! (docs/performance.md). Records are sealed only when their batch is issued, so a replaced write
//! costs no encryption either, and a batch holding a large record waits [`LARGE_BATCH_EXTRA`]
//! longer to take the next rewrite of it along (issue #326: the MLS state was one record of
//! megabytes, sealed after every group a catch-up joined; since #424 it is one record per entry).
//! One batch runs at a time (`write_queue`): the next is issued when it ended, so the database
//! follows the cache in order, also when a batch aborts. [`IndexedDbStore::flush`] waits until
//! everything written so far is committed.
//!
//! Durability: a batch is one transaction, and a later one never commits without an earlier one that
//! failed, so what survives a page that dies is always a prefix of the writes in their order (never
//! a cursor or MLS state without the messages written before it; issue #840).
//! What must not be lost is made durable before it is confirmed: the web core's API waits for a
//! flush before every request that changes server state, and the page issues the queue at once when
//! it is hidden, frozen or unloaded ([`IndexedDbStore::write_now`]; behind a running batch it goes
//! when that ended, if the page lives that long). A write lost anyway is one the server never heard
//! of; the next start catches up from the last persisted cursor.
//!
//! A group of writes ([`crate::store::LocalStore::begin_writes`]) is never split between two
//! transactions: it runs synchronously, so the batch timer and the page's lifecycle events can't
//! issue in the middle of it, and the size limit waits until the group ended.
//!
//! A write that fails (quota exceeded, the database closed for a newer version) is kept and written
//! again, so the database catches up with the cache once space is free again instead of silently
//! rolling back on the next reload (e2ee.md §9.4). The flush reports the failure; the app shows it to
//! the user. Retries back off (`write_queue`): a full disk isn't sent every failed write on every
//! flush. Past a limit of records waiting for the disk ([`IndexedDbStore::is_backlogged`]) the
//! owner pauses what it can fetch again later (the web core's sync), until a batch went through.
//!
//! A first catalog build ([`IndexedDbStore::open_reporting`]) takes one batch per task: each is
//! committed before the next starts. Waiting for the commit gives the page its thread back; no timer
//! is involved, so a hidden tab, whose timers are throttled, doesn't crawl, and a frozen one just goes
//! on when it thaws. What was committed survives a reload, which goes on from there. A build found
//! necessary while the store is in use (damage) can't wait for anything: [`LocalStore`] is synchronous.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::rc::{Rc, Weak};
use std::time::Duration;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbObjectStore, IdbTransaction, IdbTransactionMode};

use super::idb::{self, OpenMode, RECORDS};
use crate::clock::{Clock, SystemClock};
use crate::store::sealed::{BuildStep, LazySeal, RecordBackend, SealedStore, format_id, parse_id};
use crate::store::write_queue::{Issue, OpenValue, RecordValue, WriteFailure, Writes};
use crate::store::{SealedStoreLimits, StoreError};

/// How long writes are collected before they are issued together. Short enough that little is
/// lost when the page dies, long enough that a burst of writes becomes one transaction.
const BATCH_DELAY: Duration = Duration::from_millis(50);
/// How much longer a batch over `LARGE_BATCH_BYTES` waits after its delay.
const LARGE_BATCH_EXTRA: Duration = Duration::from_millis(200);
/// Property on a write transaction naming the batch of writes it carries.
const BATCH_PROPERTY: &str = "teamcraftBatch";
/// `DOMException.name` of a write refused for lack of space.
const QUOTA_EXCEEDED: &str = "QuotaExceededError";

/// The encrypted browser store.
pub type IndexedDbStore = SealedStore<IndexedDbRecords>;

/// The IndexedDB object store holding the sealed records.
pub struct IndexedDbRecords {
  inner: Rc<Inner>,
}

/// What the batch timer needs to issue the queued writes.
struct Inner {
  database: IdbDatabase,
  /// Every record's sealed bytes as of its latest issued write (what is queued is in `writes`).
  records: RefCell<BTreeMap<LookupId, Vec<u8>>>,
  writes: Rc<RefCell<Writes>>,
  /// The write transactions' handlers, owned by the page's garbage collector: a transaction still
  /// running when the store is dropped (sign-out, reopening) keeps them alive until it ended,
  /// instead of calling a dropped closure (GlitchTip TEAMCRAFT-1I). They hold `Inner` only weakly.
  on_complete: js_sys::Function,
  on_abort: js_sys::Function,
  _on_version_change: Closure<dyn FnMut(web_sys::Event)>,
  /// This `Inner` itself, for the timer that issues the writes a retry backoff held back.
  own: Weak<Inner>,
}

impl SealedStore<IndexedDbRecords> {
  /// Opens the database `name` with `database_key`, reading its ciphertext into memory; a page that
  /// owns the store (it holds the Web Lock) upgrades it to this version and builds its catalog.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    Self::open_reporting(name, database_key, |_| ControlFlow::Continue(())).await
  }

  /// [`Self::open`] for a page that shows how far a catalog build is: `on_progress` hears the
  /// percentage after every batch (never when no build is needed) and may stop the build, say
  /// because another tab takes over: it fails with [`StoreError::InUse`], and whoever opens the
  /// store next goes on from there. A newer version closing the database stops it with
  /// [`StoreError::Closed`]. Lack of space doesn't stop it: the store works from what it holds, and
  /// the next flush reports it, as for every other write.
  pub async fn open_reporting(
    name: &str,
    database_key: &SymmetricKey,
    mut on_progress: impl FnMut(u8) -> ControlFlow<()>,
  ) -> Result<Self, StoreError> {
    let backend = IndexedDbRecords::read(name, OpenMode::Upgrade).await?;
    let store = Self::open_backend_to_build(backend, database_key, SealedStoreLimits::default())?;
    let Some(mut build) = store.start_catalog_build()? else {
      return Ok(store);
    };
    loop {
      let step = store.step_catalog_build(&mut build)?;
      store.commit_build_batch().await?;
      if step == BuildStep::Complete {
        return Ok(store);
      }
      if on_progress(build.percent()).is_break() {
        return Err(StoreError::InUse);
      }
    }
  }

  /// [`Self::open`] for the service worker, a second writer beside the page that owns the store: at
  /// whatever version the database has, never upgrading it ([`OpenMode::AsIs`]) or building its
  /// catalog.
  pub async fn open_without_upgrade(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let backend = IndexedDbRecords::read(name, OpenMode::AsIs).await?;
    Self::open_backend_without_building(backend, database_key, SealedStoreLimits::default())
  }

  /// Deletes the database `name` (records and key wrapping): the user chose to set up a device
  /// whose data can't be read again (issue #160). The store must be closed first.
  pub async fn delete(name: &str) -> Result<(), StoreError> {
    idb::delete_database(name).await
  }

  /// Issues what is queued and waits until every write issued so far is committed. Writes that
  /// failed are issued again once their retry backoff ran out (until then the flush doesn't wait
  /// for them), and the flush fails with the reason until they went through.
  pub async fn flush(&self) -> Result<(), StoreError> {
    let inner = &self.backend().inner;
    inner.issue_batch(true);
    let maybe_due = inner.writes.borrow().last_batch_due();
    while !inner.writes.borrow().is_closed {
      if inner.writes.borrow().group_depth > 0 {
        // Groups are synchronous: only a panic leaves one open, and its writes stay out for good.
        return Err(StoreError::GroupFailed("a group of writes never ended".into()));
      }
      // A read-only transaction over the same store starts only after earlier read-write ones ended,
      // and their events fired before it finishes, which issues the next batch.
      let transaction = inner
        .database
        .transaction_with_str(RECORDS)
        .map_err(|error| idb::js_error("flush", &error))?;
      idb::transaction_done(&transaction).await?;
      if inner.writes.borrow().is_past(maybe_due) {
        break;
      }
      // Written meanwhile, with nothing running: no need to wait for the batch timer.
      inner.issue_queued();
    }
    inner.writes.borrow().failure()
  }

  /// Waits until the catalog build's last batch is committed: the build's backpressure, and the
  /// moment the page has its thread back.
  async fn commit_build_batch(&self) -> Result<(), StoreError> {
    match self.flush().await {
      Err(StoreError::QuotaExceeded) => {
        tracing::warn!("no space for the local store's catalog; it stays in memory for now");
        Ok(())
      },
      flushed => flushed,
    }
  }

  /// Issues what is queued now instead of after the batch delay, without waiting for it (the page
  /// is being hidden, frozen or unloaded).
  pub fn write_now(&self) {
    self.backend().inner.issue_queued();
  }

  /// Changes with every write: a reader can tell whether anything was written since it last
  /// looked without comparing contents.
  pub fn write_revision(&self) -> u32 {
    self.backend().inner.writes.borrow().next_sequence
  }

  /// Whether so many records wait for a disk that keeps failing that the owner should stop writing
  /// what it can fetch again later (issue #1201): nothing is dropped or refused, so they would
  /// pile up in memory. Ends once a batch committed.
  pub fn is_backlogged(&self) -> bool {
    self.backend().inner.writes.borrow().is_backlogged()
  }

  /// How many records may wait for a failing disk before [`Self::is_backlogged`] says so; the
  /// default is 2,000 (`write_queue::DEFAULT_MAX_UNSAVED`).
  pub fn set_max_unsaved_writes(&self, max_unsaved: usize) {
    self.backend().inner.writes.borrow_mut().max_unsaved = max_unsaved;
  }

  /// How long writes wait after a batch failed before they are issued again: `first`, doubled with
  /// every batch that fails after it, up to `longest` (1 s and 30 s by default). Zero sends them
  /// again with every batch and flush.
  pub fn set_retry_backoff(&self, first: Duration, longest: Duration) {
    self.backend().inner.writes.borrow_mut().retry_backoff = (first, longest);
  }
}

impl RecordBackend for IndexedDbRecords {
  /// No tantivy in the browser: its search reads the word index (issue #1833).
  fn indexes_words(&self) -> bool {
    true
  }

  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self.write(
      format_id(id),
      Some(RecordValue::Sealed(sealed.to_vec())),
      sealed.len(),
    );
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.write(format_id(id), None, 0);
    Ok(())
  }

  fn put_lazy(&self, id: &LookupId, size_hint: usize, seal: LazySeal) -> Result<(), StoreError> {
    self.write(format_id(id), Some(RecordValue::Unsealed(seal)), size_hint);
    Ok(())
  }

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    match self.inner.writes.borrow_mut().latest(&format_id(id))? {
      Some(OpenValue::Put(sealed)) => return Ok(Some(sealed)),
      Some(OpenValue::Delete) => return Ok(None),
      None => {},
    }
    Ok(self.inner.records.borrow().get(id).cloned())
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    let records = self.inner.records.borrow();
    let range = match maybe_after {
      Some(after) => records.range((std::ops::Bound::Excluded(*after), std::ops::Bound::Unbounded)),
      None => records.range(..),
    };
    let mut writes = self.inner.writes.borrow_mut();
    let mut batch = Vec::with_capacity(limit.min(1024));
    for (id, sealed) in range {
      match writes.latest(&format_id(id))? {
        Some(OpenValue::Put(latest)) => batch.push((*id, latest)),
        Some(OpenValue::Delete) => {},
        None => batch.push((*id, sealed.clone())),
      }
      if batch.len() == limit {
        break;
      }
    }
    Ok(batch)
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    let mut writes = self.inner.writes.borrow_mut();
    writes.group_depth = writes.group_depth.saturating_add(1);
    Ok(())
  }

  /// The group is complete: what it queued goes out with the next batch (now, if much is queued).
  fn end_writes(&self) -> Result<(), StoreError> {
    let is_due = {
      let mut writes = self.inner.writes.borrow_mut();
      writes.group_depth = writes.group_depth.saturating_sub(1);
      writes.group_depth == 0 && writes.is_over_limit()
    };
    if is_due {
      self.inner.issue_queued();
    }
    Ok(())
  }

  fn write_soon(&self) {
    if self.inner.writes.borrow().group_depth == 0 {
      self.inner.issue_queued();
    }
  }
}

impl IndexedDbRecords {
  /// Opens the database `name` as `mode` says and reads every record's ciphertext.
  async fn read(name: &str, mode: OpenMode) -> Result<Self, StoreError> {
    let database = idb::open_database_with(name, mode).await?;
    let started_ms = js_sys::Date::now();
    match read_all(&database).await {
      Ok(records) => {
        let sealed_bytes: usize = records.iter().map(|(_, sealed)| sealed.len()).sum();
        tracing::info!(
          records = records.len(),
          sealed_mib = sealed_bytes / (1024 * 1024),
          read_ms = js_sys::Date::now() - started_ms,
          wasm_mib = core::arch::wasm32::memory_size(0) / 16,
          "opened the local store"
        );
        Ok(Self::new(database, records.into_iter().collect()))
      },
      Err(error) => {
        // Closed, so a reset can delete the database (issue #160) instead of being blocked by it.
        database.close();
        Err(error)
      },
    }
  }

  fn new(database: IdbDatabase, records: BTreeMap<LookupId, Vec<u8>>) -> Self {
    Self {
      inner: Rc::new_cyclic(|inner| Inner::new(inner, database, records)),
    }
  }

  /// Queues the write; the batch goes out after [`BATCH_DELAY`], or now when much is queued.
  fn write(&self, key: String, maybe_value: Option<RecordValue>, bytes: usize) {
    let is_due = {
      let mut writes = self.inner.writes.borrow_mut();
      if writes.queue(key, maybe_value, bytes) {
        true
      } else {
        if !writes.is_scheduled {
          writes.is_scheduled = true;
          schedule(Rc::downgrade(&self.inner), BATCH_DELAY, true);
        }
        false
      }
    };
    if is_due {
      self.inner.issue_queued();
    }
  }
}

impl Inner {
  /// `own`: this `Inner` itself, for the handlers that issue the next batch when one ended.
  fn new(own: &Weak<Inner>, database: IdbDatabase, records: BTreeMap<LookupId, Vec<u8>>) -> Self {
    let writes = Rc::new(RefCell::new(Writes::default()));
    let on_complete: js_sys::Function = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      let own = own.clone();
      move |event: web_sys::Event| {
        if let Some(batch) = batch_of(&event) {
          writes.borrow_mut().committed(batch);
        }
        if let Some(inner) = own.upgrade() {
          inner.issue_queued();
        }
      }
    })
    .into_js_value()
    .unchecked_into();
    let on_abort: js_sys::Function = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      let own = own.clone();
      move |event: web_sys::Event| {
        let failure = event
          .target()
          .and_then(|target| target.dyn_into::<IdbTransaction>().ok())
          .and_then(|transaction| transaction.error())
          .map_or(WriteFailure::Other, |error| classify(&error.name()));
        tracing::error!(
          quota = matches!(failure, WriteFailure::QuotaExceeded),
          "an IndexedDB write failed"
        );
        if let Some(batch) = batch_of(&event) {
          writes.borrow_mut().aborted(batch, failure);
        }
        // What waits for this batch goes with this batch's writes once the retry backoff ran out
        // (the next write, flush or batch timer after it). If nothing waits they stay failed until
        // the next write or flush, so a full disk doesn't retry in a loop.
        if let Some(inner) = own.upgrade() {
          inner.issue_queued();
        }
      }
    })
    .into_js_value()
    .unchecked_into();
    // e2ee.md §9.4: a newer version of the app (another tab, the service worker) upgrades the
    // database. Holding it open would block that page; this page stops writing and asks for a reload.
    let on_version_change = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      let database = database.clone();
      move |_| {
        tracing::warn!("a newer version needs the local database; closing it");
        database.close();
        writes.borrow_mut().close();
      }
    });
    database.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
    Self {
      database,
      records: RefCell::new(records),
      writes,
      on_complete,
      on_abort,
      _on_version_change: on_version_change,
      own: own.clone(),
    }
  }

  /// Issues every queued write in one transaction, unless a batch runs: then this happens when it
  /// ended.
  fn issue_queued(&self) {
    self.issue_batch(false);
  }

  /// [`Self::issue_queued`]; a flush (`is_flush`) issues failed writes also when nothing new is
  /// queued. Writes wait while a failed batch's retry backoff runs. A write that can't even start
  /// counts as failed. What may go out, and what fails a batch, is `Writes`'s to decide (and tested
  /// natively); this only talks to the browser.
  fn issue_batch(&self, is_flush: bool) {
    let now_ms = SystemClock.now_ms();
    let maybe_keys = self.writes.borrow_mut().take_next_batch(now_ms, is_flush);
    let Some(keys) = maybe_keys else {
      // New writes wait for the backoff: they go when it ran out, without waiting for a flush.
      let mut writes = self.writes.borrow_mut();
      if writes.needs_retry_timer()
        && let Some(retry_in) = writes.retry_in(now_ms)
      {
        schedule(self.own.clone(), retry_in, false);
      }
      return;
    };
    let (transaction, records) = match self.transaction() {
      Ok(opened) => opened,
      Err(failure) => {
        self.writes.borrow_mut().fail_unsent(&keys, failure);
        return;
      },
    };
    let filled = self.writes.borrow_mut().fill_batch(&keys, |key, issue| {
      let record = JsValue::from_str(key);
      let maybe_id = parse_id(key);
      let sent = match issue {
        Issue::Put { sealed, .. } => {
          if let Some(id) = maybe_id {
            self.records.borrow_mut().insert(id, sealed.to_vec());
          }
          records.put_with_key(&js_sys::Uint8Array::from(*sealed), &record)
        },
        Issue::Delete { .. } => {
          if let Some(id) = maybe_id {
            self.records.borrow_mut().remove(&id);
          }
          records.delete(&record)
        },
      };
      sent.map(|_| ()).map_err(|error| failure_of(&error))
    });
    match filled {
      Ok(batch) => {
        // The events fire after this returns, so the handlers are in time. Setting a property on a
        // fresh transaction object can't fail.
        let _ = js_sys::Reflect::set(&transaction, &BATCH_PROPERTY.into(), &batch.into());
        transaction.set_oncomplete(Some(&self.on_complete));
        transaction.set_onabort(Some(&self.on_abort));
      },
      // Aborted before it carried a batch number: the abort handler leaves it alone. Deliberately
      // ignored: a transaction that can't abort any more was already aborted by the browser.
      Err(_) => {
        let _ = transaction.abort();
      },
    }
  }

  fn transaction(&self) -> Result<(IdbTransaction, IdbObjectStore), WriteFailure> {
    if self.writes.borrow().is_closed {
      return Err(WriteFailure::Closed);
    }
    let transaction = self
      .database
      .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
      .map_err(|error| failure_of(&error))?;
    let records = transaction
      .object_store(RECORDS)
      .map_err(|error| failure_of(&error))?;
    Ok((transaction, records))
  }
}

impl Drop for IndexedDbRecords {
  fn drop(&mut self) {
    // Writes still queued are lost, like a page that is closed; owners flush before dropping.
    self.inner.database.set_onversionchange(None);
    self.inner.database.close();
  }
}

/// Issues the queued writes after `delay`, unless the store is gone by then. A large batch gets
/// [`LARGE_BATCH_EXTRA`] once when `may_extend`.
fn schedule(inner: Weak<Inner>, delay: Duration, may_extend: bool) {
  wasm_bindgen_futures::spawn_local(async move {
    crate::platform::sleep(delay).await;
    let Some(inner) = inner.upgrade() else {
      return;
    };
    let is_large = inner.writes.borrow().is_large();
    if may_extend && is_large {
      schedule(Rc::downgrade(&inner), LARGE_BATCH_EXTRA, false);
    } else {
      inner.issue_queued();
    }
  });
}

fn batch_of(event: &web_sys::Event) -> Option<u32> {
  let transaction = event.target()?;
  let batch = js_sys::Reflect::get(&transaction, &BATCH_PROPERTY.into())
    .ok()?
    .as_f64()?;
  // Written as a u32 above.
  Some(batch as u32)
}

fn failure_of(error: &JsValue) -> WriteFailure {
  error
    .dyn_ref::<web_sys::DomException>()
    .map_or(WriteFailure::Other, |error| classify(&error.name()))
}

fn classify(name: &str) -> WriteFailure {
  match name {
    QUOTA_EXCEEDED => WriteFailure::QuotaExceeded,
    "InvalidStateError" => WriteFailure::Closed,
    _ => WriteFailure::Other,
  }
}

async fn read_all(database: &IdbDatabase) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| idb::js_error("load", &error))?;
  let records = transaction
    .object_store(RECORDS)
    .map_err(|error| idb::js_error("load", &error))?;
  // Both lists come in key order, so they line up.
  let keys = records
    .get_all_keys()
    .map_err(|error| idb::js_error("load", &error))?;
  let values = records.get_all().map_err(|error| idb::js_error("load", &error))?;
  let keys: js_sys::Array = idb::request_result(&keys).await?.unchecked_into();
  let values: js_sys::Array = idb::request_result(&values).await?.unchecked_into();
  keys
    .iter()
    .zip(values.iter())
    .map(|(key, value)| {
      let id = key
        .as_string()
        .as_deref()
        .and_then(parse_id)
        .ok_or_else(|| StoreError::Corrupt("record key".into()))?;
      let sealed = value
        .dyn_ref::<js_sys::Uint8Array>()
        .ok_or_else(|| StoreError::Corrupt("record value".into()))?
        .to_vec();
      Ok((id, sealed))
    })
    .collect()
}
