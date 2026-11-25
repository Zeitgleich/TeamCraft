//! The web core in a dedicated worker over the OPFS store (issue #2363, e2ee.md §9.1, §9.4).
//!
//! The IndexedDB store reads every record when it opens; the OPFS store reads an index and a record
//! when asked, but its sync access handles exist only in dedicated workers. So a member's core runs
//! in one, started by the tab that holds the account's Web Lock (`web_tabs`): that tab's page is to
//! its worker what a follower tab is to the owner tab. It sends the same `core_*` commands, as JSON,
//! and hears the same change notices; the worker runs `web_core` exactly as an owner tab without a
//! lock would.
//!
//! **No second download.** The worker runs the app's own wasm: the page posts its compiled module
//! (`wasm_bindgen::module()`) and the URL of its bindings to `core-worker.js`, which instantiates it
//! there; `main` sees the worker's scope and calls [`run`] instead of mounting the UI. The split
//! modules the core asks for load in the worker through the same chunk loader. A guest's meeting,
//! which runs the core in the page, downloads nothing more (robustness.md, "Download size").
//!
//! **What stays in the page**, and goes over the channel: the access token and ID token (the worker
//! reads the DPoP key from IndexedDB itself, it is the same origin's), visibility and the page's
//! lifecycle (hidden, frozen, closed: the worker saves the store's index and flushes), the request
//! for persistent storage, the service worker's messages, background failures and the "store was
//! ready" mark in `localStorage`. Calls run in the page, which asks the worker for the join and
//! each call key binding (the device key stays here, e2ee.md §8, #2397); streamed videos download
//! whole for now (the rest of #2363).
//!
//! **Which core.** The worker runs for members by default (#2397), unless the user turned it off
//! (`teamcraft.coreWorker = "0"`) before the store moved; always once the store has moved: OPFS
//! holds its log, IndexedDB's records were emptied, and only a worker can read it again. Never for
//! guests.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt;
use futures_util::future::{Either, select};
use serde_json::Value;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_core::platform;
use teamcraft_proto::UserId;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, Worker, WorkerOptions, WorkerType};

use crate::core_worker_messages::{FailureTag, ToPage, ToWorker};
use crate::settings::load_string;

/// The worker's script (`core-worker.js`, copied as it is): it instantiates the module it is sent.
const WORKER_SCRIPT: &str = "/core-worker.js";
/// Where `init.js` puts the absolute URL of the app's bindings, for the worker to import.
const GLUE_URL_GLOBAL: &str = "teamcraftMainJs";
/// Where `core-worker.js` keeps the messages that arrived before [`run`] listened.
const INBOX_GLOBAL: &str = "teamcraftCoreWorkerInbox";
/// The switch in `localStorage` that keeps the core in the page (`"0"`), for a browser whose worker
/// misbehaves; a store that moved runs in the worker anyway.
const SWITCH_KEY: &str = "teamcraft.coreWorker";
/// How long the page waits for the worker to start: instantiating the module and the support check.
/// The worker compiles nothing (it gets the compiled module), so this is generous.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a call waits for its answer: a catch-up holds the core for a while, never this long.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long closing waits for the worker to drop its store (the next owner opens the files).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// What calls answer once the worker failed.
const WORKER_DIED: &str = "the core's worker stopped; reload the page";

thread_local! {
  static IS_WORKER: Cell<Option<bool>> = const { Cell::new(None) };
  /// In the worker: whether the page is visible, as the page last said.
  static IS_VISIBLE: Cell<bool> = const { Cell::new(true) };
  /// In the worker: whether the store was ready on this device before (the page's `localStorage`).
  static WAS_STORE_READY: Cell<bool> = const { Cell::new(false) };
  /// In the worker: the core's token copy, which [`run`] feeds from the page.
  static TOKENS: RefCell<Option<mpsc::UnboundedSender<TokenUpdate>>> = const { RefCell::new(None) };
}

/// The page's end of its core's worker. Dropping it ends the worker.
pub(crate) struct WorkerLink {
  worker: Worker,
  next_call: Cell<u64>,
  pending: RefCell<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
  /// Until the worker said it started.
  maybe_started: RefCell<Option<oneshot::Sender<Result<(), StartFailure>>>>,
  /// Called on the worker's notices: data changed, the catalog build, failures.
  on_event: Box<dyn Fn(WorkerEvent)>,
  /// Alive as long as the link: the page's token loop stops with it.
  alive: Rc<()>,
  /// The worker failed (a panic ends its instance): calls fail at once instead of timing out.
  is_dead: Cell<bool>,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
  _on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
}

/// What the page hears from its worker besides answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerEvent {
  /// The core's data changed: this tab's UI and the follower tabs (`is_local`: this tab only).
  Changed { is_local: bool },
  /// How far the store's first catalog build (or its move from IndexedDB) is, while it shows.
  StoreBuild(Option<u8>),
  /// A command the core ran by itself failed (`feedback::report_background_failure`).
  Failure(ActionFailure),
  /// The device's store is set up: noted in `localStorage` (`was_data_lost` of a later start).
  StoreReady,
  /// The core wants a fresh ID token.
  WantsIdToken,
}

/// Why the worker didn't start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum StartFailure {
  /// The browser can't run it: no module workers, no sync access handles, no bindings URL.
  #[error("this browser can't run the core in a worker: {0}")]
  Unsupported(String),
  /// It started and failed, or didn't answer.
  #[error("the core's worker did not start: {0}")]
  Failed(String),
}

/// What the page sends the worker to start its core.
pub(crate) struct WorkerStart {
  pub(crate) user: UserId,
  pub(crate) server_url: String,
  pub(crate) maybe_token: Option<String>,
  pub(crate) maybe_id_token: Option<String>,
  pub(crate) is_bound: bool,
  pub(crate) is_visible: bool,
  pub(crate) was_store_ready: bool,
}

/// A token for the core in the worker.
struct TokenUpdate {
  token: String,
  maybe_id_token: Option<String>,
  is_bound: bool,
}

/// Whether this code runs in a dedicated worker (the core's), not in a page.
pub(crate) fn is_worker() -> bool {
  IS_WORKER.with(|cached| match cached.get() {
    Some(is_worker) => is_worker,
    None => {
      let is_worker = js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>().is_ok();
      cached.set(Some(is_worker));
      is_worker
    },
  })
}

/// Whether the core should run in a worker: by default, unless the user turned it off (#2397).
pub(crate) fn is_wanted() -> bool {
  load_string(SWITCH_KEY).as_deref() != Some("0")
}

impl WorkerLink {
  /// Starts the worker and its core. `on_event` hears its notices for as long as the link lives.
  pub(crate) async fn start(
    start: WorkerStart,
    on_event: impl Fn(WorkerEvent) + 'static,
  ) -> Result<Rc<Self>, StartFailure> {
    let glue = js_sys::Reflect::get(&js_sys::global(), &GLUE_URL_GLOBAL.into())
      .ok()
      .and_then(|url| url.as_string())
      .ok_or_else(|| StartFailure::Unsupported("the page doesn't name its bindings".to_owned()))?;
    let options = WorkerOptions::new();
    options.set_type(WorkerType::Module);
    let worker = Worker::new_with_options(WORKER_SCRIPT, &options)
      .map_err(|error| StartFailure::Unsupported(format!("{error:?}")))?;
    let (started, is_started) = oneshot::channel();
    let link = Rc::new_cyclic(|weak: &Weak<Self>| {
      let weak_for_errors = weak.clone();
      let weak = weak.clone();
      let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        if let Some(link) = weak.upgrade() {
          link.receive(&event.data());
        }
      });
      worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
      let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(move |event: web_sys::ErrorEvent| {
        if let Some(link) = weak_for_errors.upgrade() {
          link.died(&event.message());
        }
      });
      worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
      Self {
        worker: worker.clone(),
        next_call: Cell::new(0),
        pending: RefCell::default(),
        maybe_started: RefCell::new(Some(started)),
        on_event: Box::new(on_event),
        alive: Rc::new(()),
        is_dead: Cell::new(false),
        _on_message: on_message,
        _on_error: on_error,
      }
    });
    let boot = js_sys::Object::new();
    let set = |name: &str, value: &JsValue| {
      // Deliberately ignored: setting a field of a fresh plain object can't fail.
      let _ = js_sys::Reflect::set(&boot, &name.into(), value);
    };
    set("type", &"boot".into());
    set("module", &wasm_bindgen::module());
    set("glue", &glue.into());
    link
      .worker
      .post_message(&boot)
      .map_err(|error| StartFailure::Failed(format!("{error:?}")))?;
    link.post(&ToWorker::Start {
      user: start.user,
      server_url: start.server_url,
      maybe_token: start.maybe_token,
      maybe_id_token: start.maybe_id_token,
      is_bound: start.is_bound,
      is_visible: start.is_visible,
      was_store_ready: start.was_store_ready,
    });
    let timeout = Box::pin(platform::sleep(START_TIMEOUT));
    match select(is_started, timeout).await {
      Either::Left((Ok(Ok(())), _)) => Ok(link),
      Either::Left((Ok(Err(failure)), _)) => Err(failure),
      Either::Left((Err(_), _)) => Err(StartFailure::Failed("the worker went away".to_owned())),
      Either::Right(_) => Err(StartFailure::Failed("no answer".to_owned())),
    }
  }

  /// Runs a `core_*` command on the worker's core, as a follower tab does on the owner's.
  pub(crate) async fn call(&self, command: &str, args: Value) -> Result<Value, String> {
    if self.is_dead.get() {
      return Err(WORKER_DIED.to_owned());
    }
    let id = self.next_call.get().wrapping_add(1);
    self.next_call.set(id);
    let (reply, answer) = oneshot::channel();
    self.pending.borrow_mut().insert(id, reply);
    self.post(&ToWorker::Call {
      id,
      command: command.to_owned(),
      args,
    });
    let timeout = Box::pin(platform::sleep(CALL_TIMEOUT));
    let outcome = select(answer, timeout).await;
    match outcome {
      Either::Left((Ok(result), _)) => result,
      Either::Left((Err(_), _)) => Err("the core's worker went away".to_owned()),
      Either::Right(_) => {
        self.pending.borrow_mut().remove(&id);
        Err(format!("{command}: the core's worker did not answer"))
      },
    }
  }

  /// Hands the core a fresh access token (and ID token); `is_bound` says whether it is a DPoP
  /// token, whose key the worker reads itself.
  pub(crate) fn send_token(&self, token: String, maybe_id_token: Option<String>, is_bound: bool) {
    self.post(&ToWorker::Token {
      token,
      maybe_id_token,
      is_bound,
    });
  }

  /// The page was shown or hidden: the core reads in the background only while it is in view.
  pub(crate) fn set_visible(&self, is_visible: bool) {
    self.post(&ToWorker::Visible { is_visible });
  }

  /// The page is hidden, frozen or closing: the worker saves the store's index and flushes.
  pub(crate) fn write_now(&self) {
    self.post(&ToWorker::WriteNow);
  }

  /// Saves and closes the store (its files are free for the next owner), then ends the worker.
  pub(crate) async fn close(&self) {
    let closed = Box::pin(self.call("core_close", Value::Null));
    match select(closed, Box::pin(platform::sleep(CLOSE_TIMEOUT))).await {
      Either::Left((Ok(_), _)) => {},
      Either::Left((Err(error), _)) => tracing::warn!(%error, "the core's worker did not close its store"),
      Either::Right(_) => tracing::warn!("the core's worker is still closing its store; ending it"),
    }
    self.worker.terminate();
  }

  /// Alive while the link is: loops for the worker end with it.
  pub(crate) fn alive(&self) -> Weak<()> {
    Rc::downgrade(&self.alive)
  }

  fn post(&self, message: &ToWorker) {
    match serde_json::to_string(message) {
      Ok(json) => {
        if let Err(error) = self.worker.post_message(&JsValue::from_str(&json)) {
          tracing::warn!(?error, "could not reach the core's worker");
        }
      },
      Err(error) => tracing::error!(%error, "could not write a message for the core's worker"),
    }
  }

  fn receive(&self, data: &JsValue) {
    let maybe_message = data
      .as_string()
      .and_then(|json| serde_json::from_str::<ToPage>(&json).ok())
      .or_else(|| boot_failure(data));
    let Some(message) = maybe_message else {
      tracing::warn!("an unreadable message from the core's worker");
      return;
    };
    match message {
      ToPage::Started => self.started(Ok(())),
      ToPage::Unsupported { reason } => self.started(Err(StartFailure::Unsupported(reason))),
      ToPage::BootFailed { error } => self.started(Err(StartFailure::Failed(error))),
      ToPage::Answer { id, result } => {
        if let Some(reply) = self.pending.borrow_mut().remove(&id) {
          // The caller may have timed out.
          let _ = reply.send(result);
        }
      },
      ToPage::Changed { is_local } => (self.on_event)(WorkerEvent::Changed { is_local }),
      ToPage::StoreBuild { maybe_percent } => (self.on_event)(WorkerEvent::StoreBuild(maybe_percent)),
      ToPage::Failure { kind, detail } => (self.on_event)(WorkerEvent::Failure(ActionFailure {
        kind: kind.into(),
        detail,
      })),
      ToPage::StoreReady => (self.on_event)(WorkerEvent::StoreReady),
      ToPage::WantsIdToken => (self.on_event)(WorkerEvent::WantsIdToken),
      // The page runs the calls; its core's worker hears the gateway (#2397).
      ToPage::CallEvent { event } => crate::web_calls::on_gateway_event(&event),
      // Kept for "Report a problem" and crash breadcrumbs, and shown where the page's log is.
      ToPage::Log { line } => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&line));
        teamcraft_client_api::debug_log::global().add_line(line);
      },
      // The worker dies with it: what waits for it fails when its error arrives.
      ToPage::Panic { panic } => crate::crash_reports::capture_worker_panic(*panic),
    }
  }

  /// The worker's instance failed: what waits for it fails now, and the user hears it once. The
  /// page reloads to start a new one (the store's writes are in its files).
  fn died(&self, error: &str) {
    if self.is_dead.replace(true) {
      return;
    }
    tracing::error!(error, "the core's worker failed");
    self.started(Err(StartFailure::Failed(error.to_owned())));
    let pending: Vec<_> = self
      .pending
      .borrow_mut()
      .drain()
      .map(|(_, reply)| reply)
      .collect();
    for reply in pending {
      // The caller may have timed out.
      let _ = reply.send(Err(WORKER_DIED.to_owned()));
    }
    (self.on_event)(WorkerEvent::Failure(ActionFailure {
      kind: teamcraft_client_api::failure::FailureKind::Other,
      detail: format!("{WORKER_DIED}: {error}"),
    }));
  }

  fn started(&self, outcome: Result<(), StartFailure>) {
    if let Some(started) = self.maybe_started.borrow_mut().take() {
      // The start may have timed out.
      let _ = started.send(outcome);
    }
  }
}

impl Drop for WorkerLink {
  /// Sign-out or another account: the worker ends at once. Its writes are in the files already (a
  /// sync access handle writes through); a handover closes it first ([`WorkerLink::close`]).
  fn drop(&mut self) {
    self.worker.set_onmessage(None);
    self.worker.set_onerror(None);
    self.worker.terminate();
  }
}

/// `core-worker.js`'s own report of a module that didn't instantiate: a plain object.
fn boot_failure(data: &JsValue) -> Option<ToPage> {
  let kind = js_sys::Reflect::get(data, &"type".into()).ok()?.as_string()?;
  if kind != "boot_failed" {
    return None;
  }
  let error = js_sys::Reflect::get(data, &"error".into())
    .ok()
    .and_then(|error| error.as_string())
    .unwrap_or_default();
  Some(ToPage::BootFailed { error })
}

/// The worker's entry: `main` calls it instead of mounting the UI when it runs in the core's
/// worker. Listens to the page and takes what arrived before.
pub fn run() {
  // The page keeps the core's log lines for its reports (#2397).
  teamcraft_client_api::debug_log::global().forward_lines(forward_log_line);
  // Logged to the console, and reported by the page: the worker has no reporter (#2397).
  std::panic::set_hook(Box::new(|info| {
    console_error_panic_hook::hook(info);
    post_to_page(&ToPage::Panic {
      panic: Box::new(crate::crash_reports::worker_panic(info)),
    });
  }));
  // The core's split modules and the media cache spawn on Leptos' executor.
  if any_spawner::Executor::init_wasm_bindgen().is_err() {
    tracing::debug!("the task executor was set up already");
  }
  let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
  let on_message = Closure::<dyn FnMut(MessageEvent)>::new(|event: MessageEvent| receive(&event.data()));
  scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
  // Lives as long as the worker.
  on_message.forget();
  let inbox = js_sys::Reflect::get(&scope, &INBOX_GLOBAL.into()).unwrap_or(JsValue::UNDEFINED);
  if let Some(inbox) = inbox.dyn_ref::<js_sys::Array>() {
    for event in inbox.iter() {
      if let Some(event) = event.dyn_ref::<MessageEvent>() {
        receive(&event.data());
      }
    }
    inbox.set_length(0);
  }
}

/// One message from the page, in the worker.
fn receive(data: &JsValue) {
  let Some(message) = data
    .as_string()
    .and_then(|json| serde_json::from_str::<ToWorker>(&json).ok())
  else {
    // The boot message (an object) was `core-worker.js`'s; anything else is not ours.
    return;
  };
  match message {
    ToWorker::Start {
      user,
      server_url,
      maybe_token,
      maybe_id_token,
      is_bound,
      is_visible,
      was_store_ready,
    } => {
      if let Err(reason) = check_support() {
        post_to_page(&ToPage::Unsupported { reason });
        return;
      }
      IS_VISIBLE.with(|visible| visible.set(is_visible));
      WAS_STORE_READY.with(|ready| ready.set(was_store_ready));
      crate::persisted_media::use_account(user);
      let (updates, received) = mpsc::unbounded();
      TOKENS.with(|current| *current.borrow_mut() = Some(updates));
      wasm_bindgen_futures::spawn_local(async move {
        // A bound token goes with the DPoP key, read before the core makes its first request.
        let maybe_prover = match maybe_token.is_some() && is_bound {
          true => dpop_prover().await,
          false => None,
        };
        let tokens =
          crate::web_core::start_in_worker(user, server_url, maybe_token, maybe_id_token, maybe_prover);
        post_to_page(&ToPage::Started);
        feed_tokens(tokens, received).await;
      });
    },
    ToWorker::Call { id, command, args } => {
      wasm_bindgen_futures::spawn_local(async move {
        let result = crate::web_core::serve_page_of_worker(&command, args).await;
        post_to_page(&ToPage::Answer { id, result });
      });
    },
    ToWorker::Token {
      token,
      maybe_id_token,
      is_bound,
    } => TOKENS.with(|current| {
      if let Some(updates) = current.borrow().as_ref() {
        // The core stopped: nothing to hand the token to.
        let _ = updates.unbounded_send(TokenUpdate {
          token,
          maybe_id_token,
          is_bound,
        });
      }
    }),
    ToWorker::Visible { is_visible } => IS_VISIBLE.with(|visible| visible.set(is_visible)),
    ToWorker::WriteNow => crate::web_core::write_store_now(),
  }
}

/// Hands the core its tokens in order. A bound token goes with the DPoP key, which the worker reads
/// from IndexedDB (the same origin's, e2ee.md §3.1.2) before the token: the token needs it.
async fn feed_tokens(tokens: crate::web_core::WebTokens, mut received: mpsc::UnboundedReceiver<TokenUpdate>) {
  while let Some(update) = received.next().await {
    if !update.is_bound {
      tokens.set_prover(None);
    } else if teamcraft_core::api::AccessTokenSource::dpop_prover(&tokens).is_none() {
      tokens.set_prover(dpop_prover().await);
    }
    crate::web_core::take_worker_token(&tokens, update.token, update.maybe_id_token);
  }
}

async fn dpop_prover() -> Option<std::sync::Arc<teamcraft_core::api::dpop::DpopProver>> {
  teamcraft_core::store::browser_dpop_prover()
    .await
    .inspect_err(|error| tracing::warn!(%error, "could not read the DPoP key in the core's worker"))
    .ok()
    .flatten()
}

/// What the worker needs of the browser: sync access handles on OPFS files.
fn check_support() -> Result<(), String> {
  let global = js_sys::global();
  let handle = js_sys::Reflect::get(&global, &"FileSystemFileHandle".into()).unwrap_or(JsValue::UNDEFINED);
  let prototype = js_sys::Reflect::get(&handle, &"prototype".into()).unwrap_or(JsValue::UNDEFINED);
  let has_sync_handles = !prototype.is_undefined()
    && js_sys::Reflect::has(&prototype, &"createSyncAccessHandle".into()).unwrap_or(false);
  if has_sync_handles {
    Ok(())
  } else {
    Err("no sync access handles on OPFS".to_owned())
  }
}

fn post_to_page(message: &ToPage) {
  let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
  match serde_json::to_string(message) {
    Ok(json) => {
      if let Err(error) = scope.post_message(&JsValue::from_str(&json)) {
        tracing::warn!(?error, "could not reach the page of the core's worker");
      }
    },
    Err(error) => tracing::error!(%error, "could not write a message for the page"),
  }
}

/// In the worker: the core's data changed (`is_local`: for the page only, not its follower tabs).
pub(crate) fn notify_page(is_local: bool) {
  post_to_page(&ToPage::Changed { is_local });
}

/// In the worker: how far the catalog build or the move from IndexedDB is.
pub(crate) fn report_store_build(maybe_percent: Option<u8>) {
  post_to_page(&ToPage::StoreBuild { maybe_percent });
}

/// In the worker: a background command failed; the page shows it.
pub(crate) fn report_failure(failure: &ActionFailure) {
  post_to_page(&ToPage::Failure {
    kind: FailureTag::of(&failure.kind),
    detail: failure.detail.clone(),
  });
}

/// In the worker: the store is set up; the page notes it.
pub(crate) fn mark_store_ready() {
  WAS_STORE_READY.with(|ready| ready.set(true));
  post_to_page(&ToPage::StoreReady);
}

/// In the worker: whether the store was set up on this device before.
pub(crate) fn was_store_ready() -> bool {
  WAS_STORE_READY.with(Cell::get)
}

/// In the worker: whether the page is in view.
pub(crate) fn is_page_visible() -> bool {
  IS_VISIBLE.with(Cell::get)
}

/// In the worker: a line of the debug log for the page. Logs nothing itself: a failure here would
/// write a line, which comes here again.
fn forward_log_line(line: &str) {
  let message = ToPage::Log {
    line: line.to_owned(),
  };
  if let Ok(json) = serde_json::to_string(&message) {
    let scope: DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    // Deliberately ignored: see above; a page that is gone hears nothing anyway.
    let _ = scope.post_message(&wasm_bindgen::JsValue::from_str(&json));
  }
}

/// In the worker: a call event of the gateway for the page's calls (#2397).
pub(crate) fn forward_call_event(event: &teamcraft_proto::GatewayEvent) {
  post_to_page(&ToPage::CallEvent {
    event: Box::new(event.clone()),
  });
}

/// In the worker: the core wants a new ID token.
pub(crate) fn ask_for_id_token() {
  post_to_page(&ToPage::WantsIdToken);
}
