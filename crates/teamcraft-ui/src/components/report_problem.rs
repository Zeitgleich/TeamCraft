//! "Report a problem" (docs/bug-reports.md, #1362): the person describes what happened, may add a
//! screenshot, sees exactly the report that will be sent, and sends it. The report goes end-to-end
//! encrypted to the recipients of the verified workspace policy; offline it waits on the device.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::bug_report::{
  BugReport, BugReportFieldError, CallFacts, MAX_DESCRIPTION_CHARS, ReportContext, ReportScreenshot,
  check_fields, report_file_name,
};
use teamcraft_client_api::remote::CoreConnection;
use teamcraft_proto::{Base64Bytes, ClientMessageId};
use wasm_bindgen::JsCast;

use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::workspace_dialogs::Dialog;
use crate::feedback::{PendingAction, failure_message};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::reactive::SetIfAlive;
use crate::state::{AppState, use_app_state};

const FORM_ID: &str = "report-form";
const DESCRIPTION_ID: &str = "report-description";
const SCREENSHOT_ID: &str = "report-screenshot";
/// Lines of all the logs together a report carries at most, the newest.
const MAX_LOG_LINES: usize = 1000;

/// A picture the person picked; its bytes are read only when it is small enough to send.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PickedScreenshot {
  file_name: String,
  media_type: String,
  size_bytes: u64,
  maybe_bytes: Option<Vec<u8>>,
}

#[component]
pub fn ReportProblemDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let description = RwSignal::new(String::new());
  let screenshot = RwSignal::new(None::<PickedScreenshot>);
  // `None` while the app's state and logs are gathered; then the text the report file holds.
  let report_text = RwSignal::new(None::<String>);
  let errors = FormErrors::new();
  let pending = PendingAction::new();
  let file_input = NodeRef::<leptos::html::Input>::new();
  // One ID per opened dialog: a retried send is the same report, delivered once.
  let id = ClientMessageId::random();
  let created_at_ms = js_sys::Date::now() as i64;

  spawn_local(async move {
    let context = gather_context(state, created_at_ms).await;
    report_text.set_if_alive(Some(context.render()));
  });
  Effect::new(move |_| {
    let found = field_errors(i18n, &description.get(), screenshot.get().as_ref());
    errors.follow(found);
  });

  let on_pick = move |_| {
    let Some(file) = file_input
      .get()
      .and_then(|input| input.files())
      .and_then(|files| files.get(0))
    else {
      return;
    };
    spawn_local(async move {
      let picked = read_screenshot(file).await;
      screenshot.set_if_alive(Some(picked));
    });
  };
  let send = move |_| {
    let found = field_errors(
      i18n,
      &description.get_untracked(),
      screenshot.get_untracked().as_ref(),
    );
    if !found.is_empty() {
      errors.refuse(FORM_ID, found);
      return;
    }
    let Some(text) = report_text.get_untracked() else {
      return;
    };
    let maybe_screenshot = screenshot.get_untracked().and_then(|picked| {
      picked.maybe_bytes.map(|bytes| ReportScreenshot {
        file_name: picked.file_name,
        media_type: picked.media_type,
        bytes: Base64Bytes::new(bytes),
      })
    });
    let report = BugReport {
      id,
      description: description.get_untracked().trim().to_owned(),
      report_text: text,
      file_name: report_file_name(created_at_ms),
      maybe_screenshot,
    };
    pending.run_and_then(
      move || state.write(|backend| backend.send_bug_report(report)),
      move || {
        state.close_overlay();
        state.show_toast(i18n.t("report-queued"));
      },
    );
  };

  view! {
    <Dialog title_key="report-title" class="report-dialog">
      <form id=FORM_ID on:submit=|ev| ev.prevent_default()>
        <p class="muted-text small">{move || i18n.t("report-intro")}</p>
        <label for=DESCRIPTION_ID>{move || i18n.t("report-description")}</label>
        <textarea
          id=DESCRIPTION_ID
          class="input report-description"
          rows="5"
          data-autofocus
          aria-required="true"
          maxlength=MAX_DESCRIPTION_CHARS.to_string()
          aria-invalid=errors.invalid(DESCRIPTION_ID)
          aria-describedby=errors.described_or(DESCRIPTION_ID, Some("report-description-hint"))
          prop:value=move || description.get()
          on:input=move |ev| description.set(event_target_value(&ev))
        ></textarea>
        <p id="report-description-hint" class="muted-text small">{move || i18n.t("report-description-hint")}</p>
        <FieldError errors=errors field=DESCRIPTION_ID />

        <div
          id=SCREENSHOT_ID
          class="report-screenshot"
          role="group"
          tabindex="-1"
          aria-labelledby="report-screenshot-label"
          aria-invalid=errors.invalid(SCREENSHOT_ID)
          aria-describedby=errors.described(SCREENSHOT_ID)
        >
          <span id="report-screenshot-label" class="report-screenshot-label">{move || i18n.t("report-screenshot")}</span>
          <input
            node_ref=file_input
            type="file"
            class="visually-hidden"
            accept="image/png,image/jpeg,image/webp"
            tabindex="-1"
            aria-hidden="true"
            on:change=on_pick
          />
          {move || match screenshot.get() {
            Some(picked) => view! {
              <span class="report-screenshot-name" translate="no">{picked.file_name}</span>
              <button type="button" class="btn btn-small" on:click=move |_| {
                screenshot.set(None);
                if let Some(input) = file_input.get() {
                  input.set_value("");
                }
              }>{move || i18n.t("report-screenshot-remove")}</button>
            }.into_any(),
            None => view! {
              <button type="button" class="btn btn-small" on:click=move |_| {
                if let Some(input) = file_input.get() {
                  input.click();
                }
              }>
                <Icon name=IconName::Image />
                <span>{move || i18n.t("report-screenshot-add")}</span>
              </button>
            }.into_any(),
          }}
        </div>
        <p class="muted-text small">{move || i18n.t("report-screenshot-hint")}</p>
        <FieldError errors=errors field=SCREENSHOT_ID />

        <details class="report-preview">
          <summary>{move || i18n.t("report-preview")}</summary>
          <p class="muted-text small">{move || i18n.t("report-preview-hint")}</p>
          {move || match report_text.get() {
            Some(text) => view! { <pre class="report-text" tabindex="0" translate="no">{text}</pre> }.into_any(),
            None => view! { <p class="muted-text small" role="status">{move || i18n.t("report-gathering")}</p> }.into_any(),
          }}
        </details>

        {move || pending.maybe_failure().map(|failure| view! {
          <p class="report-alert" role="alert">{failure_message(i18n, &failure)}</p>
        })}
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <PendingButton
            pending=pending
            class="btn btn-primary"
            is_disabled=Signal::derive(move || report_text.with(Option::is_none))
            on_click=send
          >
            {move || i18n.t("report-send")}
          </PendingButton>
        </footer>
      </form>
    </Dialog>
  }
}

/// The errors of the description and the screenshot, in their order.
fn field_errors(i18n: I18n, description: &str, maybe_screenshot: Option<&PickedScreenshot>) -> FieldErrors {
  let mut found = FieldErrors::new();
  let picked = maybe_screenshot.map(|picked| (picked.media_type.as_str(), picked.size_bytes));
  let errors = check_fields(description, picked);
  for error in &errors {
    let (field, key) = match error {
      BugReportFieldError::DescriptionMissing => (DESCRIPTION_ID, "report-error-description"),
      BugReportFieldError::DescriptionTooLong => (DESCRIPTION_ID, "report-error-description-long"),
      BugReportFieldError::ScreenshotNotAnImage => (SCREENSHOT_ID, "report-error-screenshot-type"),
      BugReportFieldError::ScreenshotTooLarge => (SCREENSHOT_ID, "report-error-screenshot-size"),
    };
    found.push(field, i18n.t(key));
  }
  // A picture that passed the checks but couldn't be read would go missing without a word.
  let is_unread = maybe_screenshot.is_some_and(|picked| picked.maybe_bytes.is_none());
  if is_unread
    && !errors.iter().any(|error| {
      matches!(
        error,
        BugReportFieldError::ScreenshotNotAnImage | BugReportFieldError::ScreenshotTooLarge
      )
    })
  {
    found.push(SCREENSHOT_ID, i18n.t("report-error-screenshot-read"));
  }
  found
}

/// What the app knows about itself right now; names never, IDs only.
async fn gather_context(state: AppState, created_at_ms: i64) -> ReportContext {
  let (me, maybe_call) = state.read(|backend| (backend.me(), backend.active_call()));
  let is_desktop = crate::auth::tauri::is_available();
  ReportContext {
    created_at_ms,
    app_version: env!("CARGO_PKG_VERSION").to_owned(),
    platform: if is_desktop { "desktop" } else { "web" }.to_owned(),
    system: web_sys::window()
      .and_then(|window| window.navigator().user_agent().ok())
      .unwrap_or_default(),
    user: Some(me.id),
    connection: connection_word().await,
    maybe_call: maybe_call.as_ref().map(CallFacts::of),
    log_lines: gather_log(is_desktop).await,
  }
}

async fn connection_word() -> String {
  let word = match crate::remote_backend::connection_state().await {
    Ok(CoreConnection::Connected) => "connected",
    Ok(CoreConnection::Connecting) => "connecting",
    Ok(CoreConnection::Slow) => "connected, slow",
    Ok(CoreConnection::Offline { .. }) => "offline",
    Ok(CoreConnection::SignInRequired { .. }) => "signed out",
    Err(_) => "unknown",
  };
  word.to_owned()
}

/// The page's log, the desktop shell's (core and calls) or the browser's call module's, merged
/// by time: every line starts with its UTC time.
async fn gather_log(is_desktop: bool) -> Vec<String> {
  let mut lines = teamcraft_client_api::debug_log::recent_lines();
  if is_desktop {
    match crate::auth::tauri::call::<Vec<String>>("core_recent_log", &serde_json::json!({})).await {
      Ok(shell) => lines.extend(shell),
      Err(error) => tracing::warn!(%error, "the shell's log could not be read"),
    }
  }
  #[cfg(target_arch = "wasm32")]
  lines.extend(crate::web_calls::call_module_log());
  // Stable: lines of the same second keep their order within each log.
  lines.sort_by(|a, b| a.get(..20).cmp(&b.get(..20)));
  let excess = lines.len().saturating_sub(MAX_LOG_LINES);
  lines.drain(..excess);
  lines
}

async fn read_screenshot(file: web_sys::File) -> PickedScreenshot {
  let size_bytes = file.size() as u64;
  let mut picked = PickedScreenshot {
    file_name: file.name(),
    media_type: file.type_(),
    size_bytes,
    maybe_bytes: None,
  };
  // Too large or no picture: the field says so, nothing is read.
  if check_fields("-", Some((&picked.media_type, size_bytes))).is_empty() {
    match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
      Ok(buffer) => picked.maybe_bytes = Some(js_sys::Uint8Array::new(&buffer).to_vec()),
      Err(error) => {
        tracing::warn!(error = ?error.dyn_ref::<js_sys::Error>().map(js_sys::Error::name), "the screenshot could not be read")
      },
    }
  }
  picked
}
