//! Crash reports of the page (docs/crash-reports.md): wasm panics and uncaught script errors, in
//! the browser and in the desktop app's page. Reports are built and scrubbed by
//! `teamcraft_client_api::crash_reporting`, kept in `localStorage` and posted to the server's relay
//! (the web through its own origin, the desktop page through the shell).
//!
//! A panic kills the wasm instance, so its report is only queued; the next start sends it. No
//! timers: the page sends on start, after a script error and when the browser comes back online.

use std::cell::{Cell, RefCell};
use std::panic::PanicHookInfo;

use leptos::prelude::*;
use serde::Deserialize;
use teamcraft_client_api::crash_reporting::{
  AppInfo, AppPlatform, CrashEvent, CrashLevel, CrashReporter, CrashReportingConfig, CrashReportingConsent,
  CrashSendOutcome, ErrorSource, EventId, InstallationId, PanicParts, StackFrame,
};
use teamcraft_client_api::debug_log;
use teamcraft_client_api::server_entry::SERVER_INFO_PATH;
use teamcraft_proto::{CRASH_REPORTS_PATH, ServerInfoResponse};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::{AppConfig, fetch, tauri};

const KEY_QUEUE: &str = "teamcraft.crash_reports.queue";
const KEY_INSTALLATION: &str = "teamcraft.crash_reports.installation";
const KEY_CONFIG: &str = "teamcraft.crash_reports.config";
const KEY_NOTICE_SEEN: &str = "teamcraft.crash_reports.notice_seen";
/// Script errors reported per page life at most: a loop of the same error can't fill the queue.
const MAX_SCRIPT_ERRORS: u32 = 5;

thread_local! {
  static REPORTER: RefCell<Option<CrashReporter>> = const { RefCell::new(None) };
  static IS_FLUSHING: Cell<bool> = const { Cell::new(false) };
  static IS_DEAD: Cell<bool> = const { Cell::new(false) };
  static SCRIPT_ERRORS: Cell<u32> = const { Cell::new(0) };
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
  reporter.set_consent(consent(
    crate::settings::stored_prefs().privacy.send_crash_reports,
  ));
  publish_status(&reporter);
  REPORTER.with(|cell| *cell.borrow_mut() = Some(reporter));
  listen_to_window();
  wasm_bindgen_futures::spawn_local(refresh_config());
}

/// From the panic hook: queues the report and stores the queue. Nothing more can run afterwards.
pub fn capture_panic(info: &PanicHookInfo<'_>) {
  if IS_DEAD.with(|dead| dead.replace(true)) {
    return;
  }
  let event = PanicParts::from_hook(info, String::new()).into_event(event_id(), now_ms(), breadcrumbs());
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
  store_queue();
  flush();
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
  wasm_bindgen_futures::spawn_local(async {
    while let Some(pending) = with_reporter(|reporter| reporter.next_send(now_ms())).flatten() {
      let outcome = post(pending.request.body).await;
      let should_go_on =
        with_reporter(|reporter| reporter.record_outcome(pending.event_id, outcome, now_ms()))
          .unwrap_or(false);
      store_queue();
      if !should_go_on {
        break;
      }
    }
    IS_FLUSHING.with(|flushing| flushing.set(false));
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
  store_queue();
  flush();
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
    let frame = StackFrame {
      maybe_function: None,
      maybe_file: Some(event.filename()).filter(|file| !file.is_empty()),
      maybe_line: Some(event.lineno()).filter(|line| *line > 0),
      maybe_column: Some(event.colno()).filter(|column| *column > 0),
      is_in_app: true,
    };
    capture_script_error("Error", event.message(), vec![frame]);
  });
  let on_rejection =
    Closure::<dyn Fn(web_sys::PromiseRejectionEvent)>::new(|event: web_sys::PromiseRejectionEvent| {
      let reason = event.reason();
      let message = reason
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .or_else(|| reason.as_string())
        .unwrap_or_else(|| "a promise was rejected".to_owned());
      capture_script_error("UnhandledRejection", message, Vec::new());
    });
  let on_online = Closure::<dyn Fn()>::new(flush);
  // Listeners that fail to register only mean fewer reports; the app works without them.
  let _ = window.add_event_listener_with_callback("error", on_error.as_ref().unchecked_ref());
  let _ =
    window.add_event_listener_with_callback("unhandledrejection", on_rejection.as_ref().unchecked_ref());
  let _ = window.add_event_listener_with_callback("online", on_online.as_ref().unchecked_ref());
  // Invariant: the listeners live as long as the page.
  on_error.forget();
  on_rejection.forget();
  on_online.forget();
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
  };
  with_reporter(|reporter| reporter.capture(event, random_roll()));
  store_queue();
  flush();
}

fn publish_status(reporter: &CrashReporter) {
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

fn breadcrumbs() -> Vec<String> {
  debug_log::recent_lines()
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
