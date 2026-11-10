//! Settings > Account > "Your data" (issue #211, docs/data-export.md): a copy of what the user can
//! read, as a ZIP archive. Messages are end-to-end encrypted, so the server can't hand them out:
//! this device's core (the desktop shell, or the core in the browser page) decrypts and writes the
//! archive. The card only chooses, starts, shows how far it got and stops.
//!
//! The archive is not encrypted: the card says so before it starts, and the desktop asks where it
//! goes. Nothing of it passes through this page.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::data_export::{
  DataExportFailure, DataExportLabels, DataExportOptions, DataExportPhase, DataExportRequest,
  DataExportScope, DataExportStatus,
};

use crate::clock::{format_date, now_ms};
use crate::components::settings_rows::{SegmentedRow, SettingRow, SettingsCard, SwitchRow};
use crate::i18n::{I18n, use_i18n};
use crate::reactive::{OwnerLiveness, SetIfAlive};

/// How often the card asks while an export runs.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The card; only where a core runs on this device (desktop, the browser's own core).
#[component]
pub fn DataExportCard() -> impl IntoView {
  let i18n = use_i18n();
  let status = RwSignal::new(DataExportStatus::Idle);
  let scope = RwSignal::new(DataExportScope::Own);
  let includes_files = RwSignal::new(true);
  let is_asking = RwSignal::new(false);
  let alive = OwnerLiveness::of_current_owner();

  let send = {
    let alive = alive.clone();
    move |request: DataExportRequest| {
      let alive = alive.clone();
      is_asking.set(true);
      spawn_local(async move {
        let answer = ask(request).await;
        is_asking.set_if_alive(false);
        match answer {
          Ok(next) => {
            let is_active = next.is_active();
            status.set_if_alive(next);
            if is_active {
              poll(status, alive);
            }
          },
          Err(error) => {
            tracing::warn!(%error, "the data export did not answer");
            status.set_if_alive(DataExportStatus::Failed {
              reason: DataExportFailure::Other,
              progress: Default::default(),
            });
          },
        }
      });
    }
  };
  // Where things stand when the card opens: an export may run, or one may have been interrupted.
  send.clone()(DataExportRequest::Status);

  let start = {
    let send = send.clone();
    move |_| {
      let options = DataExportOptions {
        scope: scope.get_untracked(),
        includes_files: includes_files.get_untracked(),
      };
      send(DataExportRequest::Start {
        options,
        labels: Box::new(labels(i18n)),
      });
    }
  };
  let cancel = {
    let send = send.clone();
    move |_| send(DataExportRequest::Cancel)
  };
  let resume = {
    let send = send.clone();
    move |_| send(DataExportRequest::Resume)
  };
  let discard = move |_| send(DataExportRequest::Discard);

  let hint = Signal::derive(move || status_text(i18n, &status.get()));
  let fraction = move || {
    status
      .get()
      .progress()
      .filter(|_| status.get().is_active())
      .and_then(|progress| progress.fraction())
  };
  let scope_value = Signal::derive(move || scope_slug(scope.get()).to_owned());

  view! {
    <SettingsCard heading="data-export" hint="data-export-hint">
      <SegmentedRow
        label="data-export-scope"
        hint="data-export-scope-hint"
        options=vec![
          ("own".to_owned(), "data-export-scope-own".to_owned()),
          ("all".to_owned(), "data-export-scope-all".to_owned()),
        ]
        value=scope_value
        on_change=Callback::new(move |slug: String| {
          scope.set(if slug == "all" { DataExportScope::All } else { DataExportScope::Own })
        })
      />
      <SwitchRow
        label="data-export-files"
        hint="data-export-files-hint"
        value=includes_files
        on_change=Callback::new(move |is_on| includes_files.set(is_on))
      />
      <SettingRow label="data-export-action" live_hint=hint is_stacked=true>
        <div class="data-export-controls" aria-live="polite">
          {move || {
            fraction()
              .map(|fraction| {
                let percent = format!("{:.0}", fraction * 100.0);
                view! {
                  <progress
                    class="data-export-progress"
                    max="100"
                    value=percent
                    aria-label=move || i18n.t("data-export-progress")
                  ></progress>
                }
              })
          }}
          <div class="data-export-buttons">
            {move || match status.get() {
              DataExportStatus::Running { .. } | DataExportStatus::Waiting { .. } => view! {
                <button type="button" class="btn" prop:disabled=move || is_asking.get() on:click=cancel.clone()>
                  {move || i18n.t("data-export-cancel")}
                </button>
              }
              .into_any(),
              DataExportStatus::Interrupted { .. } => view! {
                <button type="button" class="btn btn-primary" prop:disabled=move || is_asking.get() on:click=resume.clone()>
                  {move || i18n.t("data-export-resume")}
                </button>
                <button type="button" class="btn btn-danger-quiet" prop:disabled=move || is_asking.get() on:click=discard.clone()>
                  {move || i18n.t("data-export-discard")}
                </button>
              }
              .into_any(),
              _ => view! {
                <button type="button" class="btn btn-primary" prop:disabled=move || is_asking.get() on:click=start.clone()>
                  {move || i18n.t("data-export-start")}
                </button>
              }
              .into_any(),
            }}
          </div>
        </div>
      </SettingRow>
    </SettingsCard>
  }
}

/// Asks again after [`POLL_INTERVAL`] while the export runs, as long as the card is there.
fn poll(status: RwSignal<DataExportStatus>, alive: OwnerLiveness) {
  set_timeout(
    move || {
      if !alive.is_alive() {
        return;
      }
      spawn_local(async move {
        let answer = ask(DataExportRequest::Status).await;
        if !alive.is_alive() {
          return;
        }
        // A failed ask is tried again: the core may be busy for a moment.
        let is_active = answer.as_ref().map_or(true, DataExportStatus::is_active);
        if let Ok(next) = answer {
          status.set_if_alive(next);
        }
        if is_active {
          poll(status, alive);
        }
      });
    },
    POLL_INTERVAL,
  );
}

async fn ask(request: DataExportRequest) -> Result<DataExportStatus, String> {
  crate::auth::tauri::call::<DataExportStatus>("core_data_export", &serde_json::json!({ "request": request }))
    .await
}

fn scope_slug(scope: DataExportScope) -> &'static str {
  match scope {
    DataExportScope::Own => "own",
    DataExportScope::All => "all",
  }
}

/// The words of the archive's index page, in the user's language.
fn labels(i18n: I18n) -> DataExportLabels {
  let locale = i18n.locale().tag();
  DataExportLabels {
    language: locale.to_owned(),
    title: i18n.t("data-export-index-title"),
    intro: i18n.t("data-export-index-intro"),
    account_heading: i18n.t("data-export-index-account"),
    account_text: i18n.t("data-export-index-account-text"),
    conversations_heading: i18n.t("data-export-index-conversations"),
    conversation_column: i18n.t("data-export-index-conversation"),
    messages_column: i18n.t("data-export-index-messages"),
    files_column: i18n.t("data-export-index-files"),
    history_limited: i18n.t("data-export-index-history-limited"),
    not_included: i18n.t("data-export-index-not-included"),
    manifest_text: i18n.t("data-export-index-manifest"),
    made_on: i18n.t_with(
      "data-export-index-made-on",
      &[("date", &format_date(now_ms(), locale))],
    ),
  }
}

/// The line under the button: where the export stands.
fn status_text(i18n: I18n, status: &DataExportStatus) -> String {
  let line = status_line(status);
  let mut args: Vec<(&str, String)> = Vec::new();
  if let Some(progress) = status.progress() {
    args.push(("done", progress.conversations_done.to_string()));
    args.push(("total", progress.conversations_total.to_string()));
    args.push(("messages", progress.messages.to_string()));
    args.push(("files", progress.files.to_string()));
  }
  if let DataExportStatus::Done { file_name, .. } = status {
    args.push(("file", file_name.clone()));
  }
  let borrowed: Vec<(&str, &str)> = args.iter().map(|(name, value)| (*name, value.as_str())).collect();
  i18n.t_with(line, &borrowed)
}

/// The i18n key of the status line.
fn status_line(status: &DataExportStatus) -> &'static str {
  match status {
    DataExportStatus::Idle => "data-export-idle",
    DataExportStatus::Running { progress } => match progress.maybe_phase {
      None | Some(DataExportPhase::Account) => "data-export-phase-account",
      Some(DataExportPhase::History) => "data-export-phase-history",
      Some(DataExportPhase::Messages) => "data-export-phase-messages",
      Some(DataExportPhase::Files) => "data-export-phase-files",
      Some(DataExportPhase::Finishing) => "data-export-phase-finishing",
    },
    DataExportStatus::Waiting { .. } => "data-export-waiting",
    DataExportStatus::Interrupted { .. } => "data-export-interrupted",
    DataExportStatus::Done { .. } => "data-export-done",
    DataExportStatus::Failed { reason, .. } => match reason {
      DataExportFailure::Storage => "data-export-failed-storage",
      DataExportFailure::Server => "data-export-failed-server",
      DataExportFailure::NotResumable => "data-export-failed-not-resumable",
      DataExportFailure::Other => "data-export-failed",
    },
    DataExportStatus::Cancelled => "data-export-cancelled",
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::data_export::DataExportProgress;

  use super::*;

  fn running(phase: DataExportPhase) -> DataExportStatus {
    DataExportStatus::Running {
      progress: DataExportProgress {
        maybe_phase: Some(phase),
        ..DataExportProgress::default()
      },
    }
  }

  #[test]
  fn every_state_has_its_own_line() {
    let progress = DataExportProgress::default();
    let lines = [
      status_line(&DataExportStatus::Idle),
      status_line(&running(DataExportPhase::Account)),
      status_line(&running(DataExportPhase::History)),
      status_line(&running(DataExportPhase::Messages)),
      status_line(&running(DataExportPhase::Files)),
      status_line(&running(DataExportPhase::Finishing)),
      status_line(&DataExportStatus::Waiting {
        progress,
        retry_at_ms: 0,
      }),
      status_line(&DataExportStatus::Interrupted { progress }),
      status_line(&DataExportStatus::Done {
        progress,
        file_name: "a.zip".into(),
      }),
      status_line(&DataExportStatus::Cancelled),
    ];
    let mut unique = lines.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), lines.len(), "{lines:?}");
  }

  #[test]
  fn each_failure_says_what_went_wrong() {
    let failed = |reason| {
      status_line(&DataExportStatus::Failed {
        reason,
        progress: DataExportProgress::default(),
      })
    };
    assert_eq!(failed(DataExportFailure::Storage), "data-export-failed-storage");
    assert_eq!(failed(DataExportFailure::Server), "data-export-failed-server");
    assert_eq!(
      failed(DataExportFailure::NotResumable),
      "data-export-failed-not-resumable"
    );
    assert_eq!(failed(DataExportFailure::Other), "data-export-failed");
  }

  #[test]
  fn every_line_and_label_is_translated_in_english_and_german() {
    let keys = [
      "data-export",
      "data-export-hint",
      "data-export-scope",
      "data-export-scope-hint",
      "data-export-scope-own",
      "data-export-scope-all",
      "data-export-files",
      "data-export-files-hint",
      "data-export-action",
      "data-export-progress",
      "data-export-start",
      "data-export-cancel",
      "data-export-resume",
      "data-export-discard",
      "data-export-idle",
      "data-export-phase-account",
      "data-export-phase-history",
      "data-export-phase-messages",
      "data-export-phase-files",
      "data-export-phase-finishing",
      "data-export-waiting",
      "data-export-interrupted",
      "data-export-done",
      "data-export-failed",
      "data-export-failed-storage",
      "data-export-failed-server",
      "data-export-failed-not-resumable",
      "data-export-cancelled",
      "data-export-index-title",
      "data-export-index-intro",
      "data-export-index-account",
      "data-export-index-account-text",
      "data-export-index-conversations",
      "data-export-index-conversation",
      "data-export-index-messages",
      "data-export-index-files",
      "data-export-index-history-limited",
      "data-export-index-not-included",
      "data-export-index-manifest",
      "data-export-index-made-on",
    ];
    for (language, file) in [
      ("en", include_str!("../../i18n/en/settings.ftl")),
      ("de", include_str!("../../i18n/de/settings.ftl")),
    ] {
      for key in keys {
        assert!(
          file.lines().any(|line| line.starts_with(&format!("{key} ="))),
          "{key} is missing in {language}"
        );
      }
    }
  }
}
