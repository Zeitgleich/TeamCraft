//! IndexedDB requests and transactions as futures.

use futures_channel::oneshot;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbFactory, IdbOpenDbRequest, IdbRequest, IdbTransaction};

use crate::store::StoreError;

/// Object store with the encrypted records.
pub(crate) const RECORDS: &str = "records";
/// Object store with the database key wrapping (a non-extractable `CryptoKey` and the wrapped key).
pub(crate) const KEYS: &str = "keys";
/// Schema version: bump with an upgrade step when object stores change, or when what records hold
/// changes so that an older version would misread it. 2: the MLS state per entry (issue #424); 3:
/// the catalog (issue #155), which a version before would not keep up to date; 4: its feed and
/// relations indexes, likewise. The object stores are those of 1.
const SCHEMA_VERSION: u32 = 4;
/// How long deleting the database may wait for other pages to close it.
const DELETE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Failures the browser reports in the normal course of things, not worth a report: a newer app
/// version upgraded the database (handled as [`StoreError::NewerSchema`]), and `add` refusing a key
/// another page stored first.
const EXPECTED_FAILURES: [&str; 2] = ["VersionError", "ConstraintError"];

/// Told about every IndexedDB failure: the operation and the `DOMException` name (`UnknownError`,
/// `InvalidStateError`, …), never its message, which may name a database or a key. The page sets it
/// to send crash reports, so storage trouble (a home-screen app on iOS losing IndexedDB after a
/// relaunch) shows up without a bug report.
pub type IdbFailureHook = fn(operation: &'static str, error_name: &str);

thread_local! {
  static FAILURE_HOOK: std::cell::Cell<Option<IdbFailureHook>> = const { std::cell::Cell::new(None) };
}

/// Sets the [`IdbFailureHook`] of this thread (the page; the service worker has none).
pub fn set_failure_hook(hook: IdbFailureHook) {
  FAILURE_HOOK.with(|slot| slot.set(Some(hook)));
}

/// How a database is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenMode {
  /// At this version's [`SCHEMA_VERSION`], creating or upgrading it: a page. For the account's
  /// store only the tab holding its Web Lock opens it (e2ee.md §9.4).
  Upgrade,
  /// At whatever version it has, never upgrading it: the service worker. Upgrading closes an older
  /// page that still owns the store; that page would go on without saving (issue #424 review, M2).
  AsIs,
}

/// Opens (and on first use creates) the database `name`, upgrading it to this version.
pub(crate) async fn open_database(name: &str) -> Result<IdbDatabase, StoreError> {
  open_database_with(name, OpenMode::Upgrade).await
}

/// Opens the database `name` as `mode` says; created at the first version if it doesn't exist.
pub(crate) async fn open_database_with(name: &str, mode: OpenMode) -> Result<IdbDatabase, StoreError> {
  let maybe_version = match mode {
    OpenMode::Upgrade => Some(SCHEMA_VERSION),
    OpenMode::AsIs => None,
  };
  open(name, maybe_version, SCHEMA_VERSION).await
}

/// Opens the database `name` at this version of its own, which never follows [`SCHEMA_VERSION`]: any
/// version of the app, and the service worker, can open it without upgrading anything.
pub(crate) async fn open_database_at(name: &str, version: u32) -> Result<IdbDatabase, StoreError> {
  open(name, Some(version), version).await
}

async fn open(name: &str, maybe_version: Option<u32>, supported: u32) -> Result<IdbDatabase, StoreError> {
  let factory = factory()?;
  let request = match maybe_version {
    Some(version) => factory.open_with_u32(name, version),
    None => factory.open(name),
  }
  .map_err(|error| js_error("open", &error))?;
  let on_upgrade = Closure::<dyn FnMut(web_sys::Event)>::new({
    let request = request.clone();
    move |_| upgrade(&request)
  });
  request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
  let result = request_result(&request).await;
  request.set_onupgradeneeded(None);
  // A newer version of the app upgraded the database (issue #160): refused by the browser itself.
  if result.is_err()
    && request
      .error()
      .ok()
      .flatten()
      .is_some_and(|error| error.name() == "VersionError")
  {
    return Err(StoreError::NewerSchema {
      found: i64::from(supported) + 1,
      supported: i64::from(supported),
    });
  }
  result?
    .dyn_into::<IdbDatabase>()
    .map_err(|_| StoreError::Database("open did not return a database".into()))
}

/// Deletes the database `name` with everything in it. A page that still holds it open is asked to
/// close it (`versionchange`; this app's pages do); if one doesn't (`blocked`), or nothing happens
/// within [`DELETE_TIMEOUT`], this fails with [`StoreError::InUse`] instead of waiting forever.
/// The browser then still deletes it once the last page let go.
pub(crate) async fn delete_database(name: &str) -> Result<(), StoreError> {
  let request = factory()?
    .delete_database(name)
    .map_err(|error| js_error("delete", &error))?;
  let (sender, receiver) = oneshot::channel::<Result<(), ()>>();
  let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(sender)));
  let on_blocked = completion(&sender, Err(()));
  request.set_onblocked(Some(on_blocked.as_ref().unchecked_ref()));
  let deleted = Box::pin(request_result(&request));
  let blocked = Box::pin(async {
    let timeout = Box::pin(crate::platform::sleep(DELETE_TIMEOUT));
    // Only the blocked event or the timeout end this side.
    drop(futures_util::future::select(receiver, timeout).await);
  });
  let outcome = futures_util::future::select(deleted, blocked).await;
  request.set_onblocked(None);
  match outcome {
    futures_util::future::Either::Left((result, _)) => result.map(|_| ()),
    futures_util::future::Either::Right(((), _)) => Err(StoreError::InUse),
  }
}

/// Waits for a request and returns its result.
pub(crate) async fn request_result(request: &IdbRequest) -> Result<JsValue, StoreError> {
  let (sender, receiver) = oneshot::channel::<Result<(), ()>>();
  let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(sender)));
  let on_success = completion(&sender, Ok(()));
  let on_error = completion(&sender, Err(()));
  request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
  request.set_onerror(Some(on_error.as_ref().unchecked_ref()));
  let outcome = receiver.await;
  request.set_onsuccess(None);
  request.set_onerror(None);
  match outcome {
    Ok(Ok(())) => request.result().map_err(|error| js_error("request", &error)),
    _ => Err(StoreError::Database(dom_error(
      "request",
      request.error().ok().flatten(),
    ))),
  }
}

/// Waits until a transaction committed.
pub(crate) async fn transaction_done(transaction: &IdbTransaction) -> Result<(), StoreError> {
  let (sender, receiver) = oneshot::channel::<Result<(), ()>>();
  let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(sender)));
  let on_complete = completion(&sender, Ok(()));
  let on_error = completion(&sender, Err(()));
  transaction.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
  transaction.set_onerror(Some(on_error.as_ref().unchecked_ref()));
  transaction.set_onabort(Some(on_error.as_ref().unchecked_ref()));
  let outcome = receiver.await;
  transaction.set_oncomplete(None);
  transaction.set_onerror(None);
  transaction.set_onabort(None);
  match outcome {
    Ok(Ok(())) => Ok(()),
    _ => Err(StoreError::Database(dom_error(
      "transaction",
      transaction.error(),
    ))),
  }
}

pub(crate) fn js_error(context: &'static str, error: &JsValue) -> StoreError {
  let name = error
    .dyn_ref::<web_sys::DomException>()
    .map_or_else(|| "Error".to_owned(), web_sys::DomException::name);
  note_failure(context, &name);
  StoreError::Database(format!(
    "IndexedDB {context}: {}",
    crate::platform::describe_js_error(error)
  ))
}

type Completion = std::rc::Rc<std::cell::RefCell<Option<oneshot::Sender<Result<(), ()>>>>>;

fn completion(sender: &Completion, outcome: Result<(), ()>) -> Closure<dyn FnMut(web_sys::Event)> {
  let sender = sender.clone();
  Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
    if let Some(sender) = sender.borrow_mut().take() {
      // The receiver only disappears if the waiting future was dropped; nobody is left to tell.
      let _ = sender.send(outcome);
    }
  })
}

fn upgrade(request: &IdbOpenDbRequest) {
  let Ok(result) = request.result() else {
    return;
  };
  let database: IdbDatabase = result.unchecked_into();
  let existing = database.object_store_names();
  for name in [RECORDS, KEYS] {
    if !existing.contains(name)
      && let Err(error) = database.create_object_store(name)
    {
      tracing::error!(
        store = name,
        error = %crate::platform::describe_js_error(&error),
        "creating IndexedDB object store failed"
      );
    }
  }
}

/// `indexedDB` of the window or worker.
fn factory() -> Result<IdbFactory, StoreError> {
  let value = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("indexedDB"))
    .map_err(|error| js_error("lookup", &error))?;
  value
    .dyn_into::<IdbFactory>()
    .map_err(|_| StoreError::Database("IndexedDB is not available".into()))
}

fn dom_error(operation: &'static str, maybe_error: Option<web_sys::DomException>) -> String {
  let name = maybe_error
    .as_ref()
    .map_or_else(|| "Unknown".to_owned(), web_sys::DomException::name);
  note_failure(operation, &name);
  maybe_error.map_or_else(
    || "IndexedDB operation failed".into(),
    |error| format!("{}: {}", error.name(), error.message()),
  )
}

fn note_failure(operation: &'static str, error_name: &str) {
  if EXPECTED_FAILURES.contains(&error_name) {
    return;
  }
  if let Some(hook) = FAILURE_HOOK.with(std::cell::Cell::get) {
    hook(operation, error_name);
  }
}
