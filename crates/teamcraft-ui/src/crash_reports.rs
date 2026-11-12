//! Crash reports of the page (docs/crash-reports.md): wasm panics and uncaught script errors, in
//! the browser and in the desktop app's page. Reports are built and scrubbed by
//! `teamcraft_client_api::crash_reporting`, kept in `localStorage` and posted to the server's relay
//! (the web through its own origin, the desktop page through the shell).
//!
//! A panic kills the wasm instance, so its report is only queued; the next start sends it. No
//! timers: the page sends on start, after a script error and when the browser comes back online.
//!
//! Performance data (`crate::perf`, issue #1521) waits in a queue of its own, kept in memory and
//! stored when the page is hidden; it goes with those sends, and on its own once a small batch is
//! queued, so an idle page sends nothing.

use std::cell::{Cell, RefCell};
use std::panic::PanicHookInfo;

use leptos::prelude::*;
use serde::Deserialize;
use teamcraft_client_api::crash_reporting::trace::{self, TraceSettings, WireTraceTree};
use teamcraft_client_api::crash_reporting::{
  AppInfo, AppPlatform, BrowserInfo, CallReport, CallReportGate, CrashEvent, CrashLevel, CrashReporter,
  CrashReportingConfig, CrashReportingConsent, CrashSendOutcome, ErrorSource, EventId, InstallationId,
  PanicParts, PendingPanic, StackFrame, Transaction, frames_from_script_stack, without_ids,
};
use teamcraft_client_api::debug_log;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::server_entry::SERVER_INFO_PATH;
use teamcraft_proto::{CRASH_REPORTS_PATH, ServerInfoResponse};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::{AppConfig, fetch, tauri};

const KEY_QUEUE: &str = "teamcraft.crash_reports.queue";
const KEY_TRANSACTIONS: &str = "teamcraft.crash_reports.transactions";
const KEY_INSTALLATION: &str = "teamcraft.crash_reports.installation";
const KEY_CONFIG: &str = "teamcraft.crash_reports.config";
const KEY_NOTICE_SEEN: &str = "teamcraft.crash_reports.notice_seen";
/// Script errors reported per page life at most: a loop of the same error can't fill the queue.
const MAX_SCRIPT_ERRORS: u32 = 5;
/// Lost sign-ins reported per page life at most.
const MAX_SIGN_OUT_REPORTS: u32 = 3;
/// IndexedDB failures reported per page life at most: a broken store fails on every access.
const MAX_STORAGE_REPORTS: u32 = 5;
/// Call failures reported per page life at most: somebody who keeps trying to join sends a few.
const MAX_CALL_REPORTS: u32 = 5;

thread_local! {
  static REPORTER: RefCell<Option<CrashReporter>> = const { RefCell::new(None) };
  static IS_FLUSHING: Cell<bool> = const { Cell::new(false) };
  static IS_DEAD: Cell<bool> = const { Cell::new(false) };
  static SCRIPT_ERRORS: Cell<u32> = const { Cell::new(0) };
  static SIGN_OUT_REPORTS: Cell<u32> = const { Cell::new(0) };
  static STORAGE_REPORTS: Cell<u32> = const { Cell::new(0) };
  static CALL_REPORTS: Cell<u32> = const { Cell::new(0) };
  static CALL_GATE: RefCell<CallReportGate> = RefCell::new(CallReportGate::new(0));
  static STATUS: RwSignal<Option<CrashReportsStatus>> = RwSignal::new(None);
}

/// What the privacy settings and the first-start notice show: reports are on for this instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrashReportsStatus {
  pub allows_opt_out: bool,
  pub shows_notice: bool,
}

/// Sets the reporter up before anything else runs: the last known instance setting and the
/// user's choice (from the stored preferences), the queue of the last run, and the listeners for
/// script errors and coming back online. Then asks the server for its current setting and sends.
pub fn install() {
  let mut reporter = CrashReporter::new(
    app_info(),
    installation(),
    load_json(KEY_QUEUE).unwrap_or_default(),
  );
  reporter.set_config(load_json::<CrashReportingConfig>(KEY_CONFIG));
  reporter = reporter.with_transactions(load_json(KEY_TRANSACTIONS).unwrap_or_default());
  reporter.set_consent(consent(
    crate::settings::stored_prefs().privacy.send_crash_reports,
  ));
  publish_status(&reporter);
  REPORTER.with(|cell| *cell.borrow_mut() = Some(reporter));
  teamcraft_core::store::set_idb_failure_hook(capture_storage_error);
  listen_to_window();
  wasm_bindgen_futures::spawn_local(refresh_config());
}

/// From the panic hook: queues the report and stores the queue. Nothing more can run afterwards.
pub fn capture_panic(info: &PanicHookInfo<'_>) {
  if IS_DEAD.with(|dead| dead.replace(true)) {
    return;
  }
  // Only the page's own log: the hook calls nothing that might borrow what the panic left borrowed.
  let event = PanicParts::from_hook(info, String::new()).into_event(event_id(), now_ms(), page_breadcrumbs());
  with_reporter(|reporter| reporter.capture(event, random_roll()));
  store_queue();
}

/// The user's switch in the privacy settings changed.
pub fn set_consent(is_allowed: bool) {
  let was_active = with_reporter(|reporter| reporter.is_active()).unwrap_or(false);
  with_reporter(|reporter| reporter.set_consent(consent(is_allowed)));
  let is_active = with_reporter(|reporter| reporter.is_active()).unwrap_or(false);
  // Turned on again: a new installation ID, so the reports before and after can't be joined.
  if is_active && !was_active {
    let fresh = new_installation();
    store_text(KEY_INSTALLATION, &fresh.to_string());
    with_reporter(|reporter| {
      let queue = reporter.queue().clone();
      let config = reporter.config().cloned();
      *reporter = CrashReporter::new(app_info(), fresh, queue);
      reporter.set_config(config);
      reporter.set_consent(consent(is_allowed));
    });
  }
  with_reporter(|reporter| apply_trace_settings(reporter.trace_settings()));
  store_queue();
  store_transactions();
  flush();
}

/// Whether an operation starting now is measured (`crate::perf`): reports on, the user didn't opt
/// out, and the instance's trace rate keeps it.
pub fn should_trace() -> bool {
  !IS_DEAD.with(Cell::get) && with_reporter(|reporter| reporter.should_trace(random_roll())).unwrap_or(false)
}

/// Queues a finished measure; a full batch is sent at once.
pub fn capture_transaction(transaction: &Transaction) {
  take_traces();
  let is_batch = with_reporter(|reporter| {
    reporter.capture_transaction(transaction);
    reporter.has_transaction_batch()
  })
  .unwrap_or(false);
  if is_batch {
    flush();
  }
}

/// Whether the instance has crash reports on, and how (reactive).
pub fn status() -> Option<CrashReportsStatus> {
  STATUS.with(|status| status.get())
}

/// Whether the first-start notice is due: reports on, the instance wants it, not shown yet.
pub fn is_notice_due() -> bool {
  status().is_some_and(|status| status.shows_notice) && load_text(KEY_NOTICE_SEEN).is_none()
}

pub fn mark_notice_seen() {
  store_text(KEY_NOTICE_SEEN, "1");
}

/// Sends what is due, one report at a time, in the background; a second call while sending
/// does nothing.
pub fn flush() {
  if IS_DEAD.with(Cell::get) || IS_FLUSHING.with(|flushing| flushing.replace(true)) {
    return;
  }
  take_traces();
  with_reporter(CrashReporter::begin_flush);
  wasm_bindgen_futures::spawn_local(async {
    take_shell_traces().await;
    let mut accepted = 0_u32;
    while let Some(pending) = with_reporter(|reporter| reporter.next_send(now_ms())).flatten() {
      let outcome = post(pending.request.body).await;
      if outcome == CrashSendOutcome::Accepted {
        accepted += 1;
      }
      let should_go_on =
        with_reporter(|reporter| reporter.record_outcome(pending.event_id, outcome, now_ms()))
          .unwrap_or(false);
      store_queue();
      if !should_go_on {
        break;
      }
    }
    store_transactions();
    // async-ok: a thread-local Cell, not a signal.
    IS_FLUSHING.with(|flushing| flushing.set(false));
    // At a full trace rate a flush's share (5) may not empty the queue: go on while the backend
    // takes what is sent; a refusal or a wait ends it until the next trigger.
    let is_more = accepted > 0 && with_reporter(|reporter| reporter.has_transaction_batch()).unwrap_or(false);
    if is_more {
      flush();
    }
  });
}

/// `GET /api/v1/server-info`: the instance's setting now. Offline, the last known one stays.
async fn refresh_config() {
  let Some(info) = server_info().await else {
    flush();
    return;
  };
  let maybe_config = info
    .maybe_crash_reports
    .as_ref()
    .and_then(|info| CrashReportingConfig::from_info(info).ok());
  match &maybe_config {
    Some(config) => store_text(KEY_CONFIG, &serde_json::to_string(config).unwrap_or_default()),
    None => remove_key(KEY_CONFIG),
  }
  with_reporter(|reporter| {
    reporter.set_config(maybe_config);
    publish_status(reporter);
  });
  take_shell_panics().await;
  store_queue();
  flush();
}

/// The desktop shell's panics since the last start (its hook keeps them for the page): reported
/// like the page's own, with the instance's setting and the user's choice.
async fn take_shell_panics() {
  if !tauri::is_available() {
    return;
  }
  let Ok(pending) =
    tauri::call::<Vec<PendingPanic>>("crash_reports_take_native", &serde_json::json!({})).await
  else {
    return;
  };
  for panic in pending {
    let event = panic
      .parts
      .into_event(event_id(), panic.timestamp_ms, panic.breadcrumbs);
    with_reporter(|reporter| reporter.capture(event, random_roll()));
  }
}

async fn server_info() -> Option<ServerInfoResponse> {
  #[derive(Deserialize)]
  struct Payload {
    status: u16,
    body: String,
  }
  let (status, body) = if tauri::is_available() {
    let payload = tauri::call::<Payload>("public_get", &serde_json::json!({ "path": SERVER_INFO_PATH }))
      .await
      .ok()?;
    (payload.status, payload.body)
  } else {
    let response = fetch::fetch(
      "GET",
      &AppConfig::load().api_url(SERVER_INFO_PATH),
      &[("Accept".to_owned(), "application/json".to_owned())],
      None,
    )
    .await
    .ok()?;
    (
      response.status,
      String::from_utf8_lossy(&response.body).into_owned(),
    )
  };
  (status == 200)
    .then(|| serde_json::from_str(&body).ok())
    .flatten()
}

/// One envelope to the server's relay.
async fn post(body: Vec<u8>) -> CrashSendOutcome {
  #[derive(Deserialize)]
  struct Answer {
    status: u16,
    #[serde(default)]
    maybe_retry_after: Option<String>,
  }
  let answer = if tauri::is_available() {
    let Ok(text) = String::from_utf8(body) else {
      return CrashSendOutcome::Rejected;
    };
    tauri::call::<Answer>("crash_report_post", &serde_json::json!({ "envelope": text }))
      .await
      .map_err(|_| ())
  } else {
    fetch::fetch(
      "POST",
      &AppConfig::load().api_url(CRASH_REPORTS_PATH),
      &[(
        "Content-Type".to_owned(),
        teamcraft_client_api::crash_reporting::ENVELOPE_CONTENT_TYPE.to_owned(),
      )],
      Some(body),
    )
    .await
    .map(|response| Answer {
      status: response.status,
      maybe_retry_after: response.maybe_retry_after,
    })
    .map_err(|_| ())
  };
  match answer {
    Ok(answer) => CrashSendOutcome::from_status(answer.status, answer.maybe_retry_after.as_deref()),
    Err(()) => CrashSendOutcome::Unreachable,
  }
}

/// Uncaught script errors and rejected promises (sampled, at most a few per page life), and
/// coming back online.
fn listen_to_window() {
  let Some(window) = web_sys::window() else {
    return;
  };
  let on_error = Closure::<dyn Fn(web_sys::ErrorEvent)>::new(|event: web_sys::ErrorEvent| {
    let mut frames = frames_of(&event.error());
    if frames.is_empty() {
      frames.extend(StackFrame::from_error_event(
        &event.filename(),
        event.lineno(),
        event.colno(),
      ));
    }
    capture_script_error("Error", event.message(), frames);
  });
  let on_rejection =
    Closure::<dyn Fn(web_sys::PromiseRejectionEvent)>::new(|event: web_sys::PromiseRejectionEvent| {
      let reason = event.reason();
      let message = reason
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| reason.as_string())
        .unwrap_or_else(|| "a promise was rejected".to_owned());
      capture_script_error("UnhandledRejection", message, frames_of(&reason));
    });
  let on_online = Closure::<dyn Fn()>::new(flush);
  // Performance data lives in memory; it is stored when the page may go away (hidden, closed).
  let on_hidden = Closure::<dyn Fn()>::new(|| {
    let is_hidden = web_sys::window()
      .and_then(|window| window.document())
      .is_some_and(|document| document.visibility_state() == web_sys::VisibilityState::Hidden);
    if is_hidden {
      store_transactions();
    }
  });
  let on_pagehide = Closure::<dyn Fn()>::new(store_transactions);
  // Listeners that fail to register only mean fewer reports; the app works without them.
  let _ = window.add_event_listener_with_callback("error", on_error.as_ref().unchecked_ref());
  let _ =
    window.add_event_listener_with_callback("unhandledrejection", on_rejection.as_ref().unchecked_ref());
  let _ = window.add_event_listener_with_callback("online", on_online.as_ref().unchecked_ref());
  let _ = window.add_event_listener_with_callback("pagehide", on_pagehide.as_ref().unchecked_ref());
  if let Some(document) = window.document() {
    let _ = document.add_event_listener_with_callback("visibilitychange", on_hidden.as_ref().unchecked_ref());
  }
  // Invariant: the listeners live as long as the page.
  on_error.forget();
  on_rejection.forget();
  on_online.forget();
  on_hidden.forget();
  on_pagehide.forget();
}

/// The frames of a thrown value's `stack` (an `Error`; anything else has none). Without them a
/// rejection only says what failed, not where (GlitchTip TEAMCRAFT-1I).
fn frames_of(thrown: &wasm_bindgen::JsValue) -> Vec<StackFrame> {
  if !thrown.is_object() {
    return Vec::new();
  }
  let Some(stack) = js_sys::Reflect::get(thrown, &"stack".into())
    .ok()
    .and_then(|stack| stack.as_string())
  else {
    return Vec::new();
  };
  let origin = web_sys::window()
    .and_then(|window| window.location().origin().ok())
    .unwrap_or_default();
  frames_from_script_stack(&stack, &origin)
}

/// The app lost a sign-in the user didn't end (a refused or missing saved sign-in, a lost device
/// key, a vault that can't be read). `reason` is a fixed text naming the path, without content;
/// the breadcrumbs carry the redacted debug log that led there. A warning, so it is sampled like
/// other handled errors. Queued at once: the page may go to the identity provider right after.
pub fn capture_lost_sign_in(reason: &'static str) {
  if SIGN_OUT_REPORTS.with(|count| count.replace(count.get() + 1)) >= MAX_SIGN_OUT_REPORTS {
    return;
  }
  capture_handled("LostSignIn", reason.to_owned());
}

/// A call failed on this device for a reason that isn't the user's or the device's (a blocked
/// microphone, a missing camera): the join, or the call breaking off after it had started. The
/// message starts with the failure's stable wire name (`call_encryption_failed: …`), IDs taken
/// out, so one cause is one issue; the breadcrumbs carry the call module's log next to the page's.
pub fn capture_call_failure(failure: &ActionFailure) {
  if !failure.kind.is_reported_call_failure()
    || CALL_REPORTS.with(|count| count.replace(count.get() + 1)) >= MAX_CALL_REPORTS
  {
    return;
  }
  capture_handled("CallFailed", without_ids(&failure.to_wire()));
}

/// The media layer's problem reports from a call snapshot (docs/crash-reports.md, "Call
/// problems"): each once, at most the instance's cap per call, sampled like other handled errors.
/// The web page adds the browser's family and major version; the desktop's media is native.
pub fn capture_call_reports(reports: &[CallReport]) {
  if reports.is_empty() {
    return;
  }
  let per_call = with_reporter(|reporter| reporter.config().map(|config| config.call_reports_per_call))
    .flatten()
    .unwrap_or(0);
  let now = now_ms();
  let admitted: Vec<CallReport> = CALL_GATE.with(|gate| {
    let mut gate = gate.borrow_mut();
    gate.set_per_call(per_call);
    reports
      .iter()
      .filter(|report| gate.admit(report, now))
      .cloned()
      .collect()
  });
  if admitted.is_empty() {
    return;
  }
  let maybe_browser = (!tauri::is_available()).then(browser);
  for report in admitted {
    let event = report.into_event(event_id(), maybe_browser, breadcrumbs());
    with_reporter(|reporter| reporter.capture(event, random_roll()));
  }
  store_queue();
  flush();
}

/// IndexedDB failed (`teamcraft_core::store::IdbFailureHook`): the operation and the
/// `DOMException` name, both fixed identifiers.
fn capture_storage_error(operation: &'static str, error_name: &str) {
  if STORAGE_REPORTS.with(|count| count.replace(count.get() + 1)) >= MAX_STORAGE_REPORTS {
    return;
  }
  capture_handled("IndexedDbError", format!("{operation}: {error_name}"));
}

fn capture_script_error(error_type: &str, message: String, frames: Vec<StackFrame>) {
  // After a panic the wasm instance is dead; the browser's "unreachable" error is the panic itself.
  if IS_DEAD.with(Cell::get)
    || SCRIPT_ERRORS.with(|count| count.replace(count.get() + 1)) >= MAX_SCRIPT_ERRORS
  {
    return;
  }
  let event = CrashEvent {
    event_id: event_id(),
    timestamp_ms: now_ms(),
    level: CrashLevel::Error,
    source: ErrorSource::JavaScript,
    error_type: error_type.to_owned(),
    message,
    frames,
    breadcrumbs: breadcrumbs(),
    maybe_call: None,
  };
  with_reporter(|reporter| reporter.capture(event, random_roll()));
  store_queue();
  flush();
}

/// A handled problem of the app's own code, as a sampled warning without a stack trace: the
/// breadcrumbs (the redacted debug log) tell what led there. Queued at once.
pub(crate) fn capture_handled(error_type: &str, message: String) {
  let event = CrashEvent {
    event_id: event_id(),
    timestamp_ms: now_ms(),
    level: CrashLevel::Warning,
    source: ErrorSource::Rust,
    error_type: error_type.to_owned(),
    message,
    frames: Vec::new(),
    breadcrumbs: breadcrumbs(),
    maybe_call: None,
  };
  with_reporter(|reporter| reporter.capture(event, random_roll()));
  store_queue();
  flush();
}

fn publish_status(reporter: &CrashReporter) {
  apply_trace_settings(reporter.trace_settings());
  let status = reporter.config().map(|config| CrashReportsStatus {
    allows_opt_out: config.allows_opt_out,
    shows_notice: config.shows_notice,
  });
  STATUS.with(|signal| signal.try_set(status));
}

fn with_reporter<T>(work: impl FnOnce(&mut CrashReporter) -> T) -> Option<T> {
  REPORTER.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(work))
}

fn store_queue() {
  if let Some(Ok(json)) = with_reporter(|reporter| serde_json::to_string(reporter.queue())) {
    store_text(KEY_QUEUE, &json);
  }
}

fn store_transactions() {
  take_traces();
  if let Some(Ok(json)) = with_reporter(|reporter| serde_json::to_string(reporter.transactions())) {
    store_text(KEY_TRANSACTIONS, &json);
  }
}

fn consent(is_allowed: bool) -> CrashReportingConsent {
  if is_allowed {
    CrashReportingConsent::Allowed
  } else {
    CrashReportingConsent::OptedOut
  }
}

fn app_info() -> AppInfo {
  AppInfo {
    version: env!("CARGO_PKG_VERSION").to_owned(),
    platform: if tauri::is_available() {
      AppPlatform::Desktop
    } else {
      AppPlatform::Web
    },
    os_name: os_name(),
    maybe_os_version: None,
  }
}

/// The browser's family and major version from the user agent, for call reports.
fn browser() -> BrowserInfo {
  let agent = web_sys::window()
    .and_then(|window| window.navigator().user_agent().ok())
    .unwrap_or_default();
  BrowserInfo::from_user_agent(&agent)
}

/// The OS family from the user agent; nothing finer (no browser version, no device).
fn os_name() -> String {
  let agent = web_sys::window()
    .and_then(|window| window.navigator().user_agent().ok())
    .unwrap_or_default();
  let name = if agent.contains("Android") {
    "Android"
  } else if agent.contains("iPhone") || agent.contains("iPad") {
    "iOS"
  } else if agent.contains("Windows") {
    "Windows"
  } else if agent.contains("Mac OS") {
    "macOS"
  } else if agent.contains("Linux") {
    "Linux"
  } else {
    "Other"
  };
  name.to_owned()
}

fn installation() -> InstallationId {
  if let Some(id) =
    load_text(KEY_INSTALLATION).and_then(|text| serde_json::from_value(serde_json::Value::String(text)).ok())
  {
    return id;
  }
  let fresh = new_installation();
  store_text(KEY_INSTALLATION, &fresh.to_string());
  fresh
}

fn new_installation() -> InstallationId {
  InstallationId::from_random_bytes(random_bytes())
}

fn event_id() -> EventId {
  EventId::from_random_bytes(random_bytes())
}

fn random_bytes() -> [u8; 16] {
  let mut bytes = [0; 16];
  // Without the browser's random source the IDs are zero: a report then merges with others in the
  // backend, which is harmless; it never fails the app.
  let _ = getrandom::getrandom(&mut bytes);
  bytes
}

fn random_roll() -> f64 {
  js_sys::Math::random()
}

/// The redacted debug log, then what the page's own spans did and which are still open.
/// The page's log and the call module's (a wasm instance with a log of its own), merged by time:
/// every line starts with its UTC time. Then the trace lines.
fn breadcrumbs() -> Vec<String> {
  let mut lines = debug_log::recent_lines();
  lines.extend(crate::web_calls::call_module_log());
  // Stable: lines of the same second keep their order within each log.
  lines.sort_by(|a, b| a.get(..20).cmp(&b.get(..20)));
  lines.extend(trace::breadcrumb_lines(js_sys::Date::now()));
  lines
}

/// The page's log and the trace lines, without the call module's.
fn page_breadcrumbs() -> Vec<String> {
  let mut lines = debug_log::recent_lines();
  lines.extend(trace::breadcrumb_lines(js_sys::Date::now()));
  lines
}

/// Collection follows the reporter (docs/tracing.md): in the page's own process, and on desktop in
/// the shell's, whose spans only the page can send.
fn apply_trace_settings(settings: TraceSettings) {
  trace::configure(settings);
  if tauri::is_available() {
    wasm_bindgen_futures::spawn_local(async move {
      let args = serde_json::json!({
        "is_on": settings.is_on,
        "trace_rate": settings.trace_rate.get(),
        "slow_ms": settings.slow_ms,
      });
      // An older shell without the command only means its spans aren't collected.
      let _ = tauri::call::<()>("perf_configure", &args).await;
    });
  }
}

/// The trees the page's trace layer kept, queued as transactions.
fn take_traces() {
  let trees = trace::take_kept();
  if !trees.is_empty() {
    with_reporter(|reporter| reporter.capture_traces(&trees, random_bytes));
  }
}

/// The desktop shell's kept trees, checked again on arrival ([`WireTraceTree::into_tree`]).
async fn take_shell_traces() {
  if !tauri::is_available() {
    return;
  }
  let Ok(wire) = tauri::call::<Vec<WireTraceTree>>("perf_traces", &serde_json::json!({})).await else {
    return;
  };
  let trees: Vec<_> = wire.into_iter().filter_map(WireTraceTree::into_tree).collect();
  if !trees.is_empty() {
    with_reporter(|reporter| reporter.capture_traces(&trees, random_bytes));
  }
}

fn now_ms() -> i64 {
  // Precision: `Date.now()` is a whole number of milliseconds well inside i64.
  js_sys::Date::now() as i64
}

fn storage() -> Option<web_sys::Storage> {
  web_sys::window()?.local_storage().ok()?
}

fn load_text(key: &str) -> Option<String> {
  storage()?.get_item(key).ok()?
}

fn load_json<T: serde::de::DeserializeOwned>(key: &str) -> Option<T> {
  serde_json::from_str(&load_text(key)?).ok()
}

fn store_text(key: &str, value: &str) {
  // A full or blocked storage only loses reports; the app goes on.
  if let Some(storage) = storage() {
    let _ = storage.set_item(key, value);
  }
}

fn remove_key(key: &str) {
  if let Some(storage) = storage() {
    let _ = storage.remove_item(key);
  }
}
