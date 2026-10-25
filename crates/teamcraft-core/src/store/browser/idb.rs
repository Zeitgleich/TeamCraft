//! IndexedDB requests and transactions as futures.

use futures_channel::oneshot;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbFactory, IdbOpenDbRequest, IdbRequest, IdbTransaction};

use crate::store::StoreError;

/// Object store with the encrypted records.
pub(super) const RECORDS: &str = "records";
/// Object store with the database key wrapping (a non-extractable `CryptoKey` and the wrapped key).
pub(super) const KEYS: &str = "keys";
/// Schema version: bump with an upgrade step when object stores change.
const SCHEMA_VERSION: u32 = 1;

/// Opens (and on first use creates) the database `name`.
pub(super) async fn open_database(name: &str) -> Result<IdbDatabase, StoreError> {
  let request = factory()?
    .open_with_u32(name, SCHEMA_VERSION)
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
      found: i64::from(SCHEMA_VERSION) + 1,
      supported: i64::from(SCHEMA_VERSION),
    });
  }
  result?
    .dyn_into::<IdbDatabase>()
    .map_err(|_| StoreError::Database("open did not return a database".into()))
}

/// Deletes the database `name` with everything in it, once no page holds it open any more.
pub(super) async fn delete_database(name: &str) -> Result<(), StoreError> {
  let request = factory()?
    .delete_database(name)
    .map_err(|error| js_error("delete", &error))?;
  request_result(&request).await.map(|_| ())
}

/// Waits for a request and returns its result.
pub(super) async fn request_result(request: &IdbRequest) -> Result<JsValue, StoreError> {
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
    _ => Err(StoreError::Database(dom_error(request.error().ok().flatten()))),
  }
}

/// Waits until a transaction committed.
pub(super) async fn transaction_done(transaction: &IdbTransaction) -> Result<(), StoreError> {
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
    _ => Err(StoreError::Database(dom_error(transaction.error()))),
  }
}

pub(super) fn js_error(context: &str, error: &JsValue) -> StoreError {
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

fn dom_error(maybe_error: Option<web_sys::DomException>) -> String {
  maybe_error.map_or_else(
    || "IndexedDB operation failed".into(),
    |error| format!("{}: {}", error.name(), error.message()),
  )
}
