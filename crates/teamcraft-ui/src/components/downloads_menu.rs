//! The desktop app's downloads (#153): a button in the top bar beside the avatar, shown once a
//! file was saved, and a list of the recent downloads under it, as in a browser. The bar under the
//! button's arrow fills with the progress of what is running; it turns yellow when a download broke
//! off and flashes green when one finished. A row shows speed and time left while it runs; hovering
//! (or focusing) it offers pause and cancel, or, once saved, show in folder, open and remove.

use std::collections::{HashMap, HashSet};

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::actions::{DownloadControl, DownloadEntry, DownloadState, FileFailure};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::download_speed::{SpeedMeter, seconds_left};
use crate::i18n::{I18n, use_i18n};
use crate::state::{AppState, Overlay, use_app_state};

/// The button in the top bar; nothing until the first file was saved.
#[component]
pub fn DownloadsButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // A read that is still on its way keeps the last list instead of hiding the button.
  let entries = Memo::new(move |previous: Option<&Vec<DownloadEntry>>| {
    state
      .read(|backend| backend.downloads())
      .or_else(|| previous.cloned())
      .unwrap_or_default()
  });
  let is_open = move || matches!(state.overlay.get(), Overlay::Downloads);
  let seen_failures = RwSignal::new(HashSet::<String>::new());
  let is_done_flashing = RwSignal::new(false);
  let last_states = StoredValue::new(HashMap::<String, DownloadState>::new());

  // A download that finished while the list was watched flashes the bar green once.
  Effect::new(move |_| {
    let list = entries.get();
    let has_finished = last_states.with_value(|last| {
      list.iter().any(|entry| {
        entry.state == DownloadState::Saved
          && last
            .get(&entry.file_id)
            .is_some_and(|state| *state != DownloadState::Saved)
      })
    });
    last_states.set_value(
      list
        .iter()
        .map(|entry| (entry.file_id.clone(), entry.state))
        .collect(),
    );
    if has_finished {
      is_done_flashing.set(true);
    }
  });
  let summary = Memo::new(move |_| Summary::of(&entries.get(), &seen_failures.get()));
  let toggle = move |_| {
    if is_open() {
      state.close_overlay();
    } else {
      // Opening the list shows the failures: the bar stops asking for attention.
      seen_failures.set(failed_ids(&entries.get_untracked()));
      state.overlay.set(Overlay::Downloads);
    }
  };

  view! {
    <Show when=move || tauri::is_available() && !entries.with(Vec::is_empty) fallback=|| ()>
      <div class="downloads">
        <button
          class="icon-btn on-dark downloads-btn"
          class:is-active=move || summary.get().is_active
          class:is-attention=move || summary.get().needs_attention
          class:is-done=move || is_done_flashing.get()
          aria-haspopup="dialog"
          aria-expanded=move || is_open().to_string()
          aria-label=move || button_label(i18n, summary.get())
          data-tip=move || i18n.t("downloads-title")
          on:click=toggle
          on:animationend=move |event: web_sys::AnimationEvent| {
            if event.animation_name() == "downloads-done" {
              is_done_flashing.set(false);
            }
          }
        >
          <svg class="downloads-arrow" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" focusable="false">
            <path d="M12 3v11 M7 9.5l5 5 5-5" />
          </svg>
          <span class="downloads-bar" aria-hidden="true">
            <span class="downloads-bar-fill" style:--downloads-progress=move || summary.get().fraction.to_string()></span>
          </span>
        </button>
        <Show when=is_open fallback=|| ()>
          <DownloadsMenu entries=entries />
        </Show>
      </div>
    </Show>
  }
}

/// What the button shows of the whole list.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Summary {
  /// Downloads transferring now.
  active: usize,
  is_active: bool,
  /// Of the running downloads together, 0 to 1.
  fraction: f64,
  /// A download broke off since the list was last opened.
  needs_attention: bool,
}

impl Summary {
  fn of(entries: &[DownloadEntry], seen_failures: &HashSet<String>) -> Self {
    let running: Vec<&DownloadEntry> = entries
      .iter()
      .filter(|entry| entry.state == DownloadState::Active)
      .collect();
    let (done, total) = running.iter().fold((0u64, 0u64), |(done, total), entry| {
      (
        done.saturating_add(entry.done_bytes.min(entry.total_bytes)),
        total.saturating_add(entry.total_bytes),
      )
    });
    Self {
      active: running.len(),
      is_active: !running.is_empty(),
      fraction: if total == 0 {
        0.0
      } else {
        done as f64 / total as f64
      },
      needs_attention: failed_ids(entries).iter().any(|id| !seen_failures.contains(id)),
    }
  }
}

/// Downloads that broke off: failed, or waiting for a retry after a failure.
fn failed_ids(entries: &[DownloadEntry]) -> HashSet<String> {
  entries
    .iter()
    .filter(|entry| {
      matches!(
        entry.state,
        DownloadState::Failed { .. } | DownloadState::Retrying { .. }
      )
    })
    .map(|entry| entry.file_id.clone())
    .collect()
}

fn button_label(i18n: I18n, summary: Summary) -> String {
  if summary.is_active {
    i18n.t_count("downloads-label-active", summary.active)
  } else {
    i18n.t("downloads-title")
  }
}

/// The list under the button.
#[component]
fn DownloadsMenu(entries: Memo<Vec<DownloadEntry>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let menu = NodeRef::<html::Div>::new();
  let meter = StoredValue::new(SpeedMeter::default());
  // Speeds of the running downloads, measured on every read of the list.
  let speeds = Memo::new(move |_| {
    let list = entries.get();
    let now = now_ms() as f64;
    meter.update_value(|meter| {
      meter.retain(|file| {
        list
          .iter()
          .any(|entry| entry.file_id == file && entry.state == DownloadState::Active)
      });
    });
    list
      .iter()
      .filter(|entry| entry.state == DownloadState::Active)
      .filter_map(|entry| {
        let rate = meter
          .try_update_value(|meter| meter.measure(&entry.file_id, entry.done_bytes, now))
          .flatten()?;
        Some((entry.file_id.clone(), rate))
      })
      .collect::<HashMap<String, f64>>()
  });
  let close = move || {
    state.close_overlay();
    focus_downloads_button();
  };
  let on_keydown = move |event: web_sys::KeyboardEvent| {
    if event.key() == "Escape" {
      event.stop_propagation();
      close();
    }
  };
  Effect::new(move |_| {
    if let Some(element) = menu.get() {
      drop(element.focus());
    }
  });

  view! {
    <MenuSurface
      class="downloads-menu"
      sheet_class="downloads-menu"
      role="dialog"
      label=Signal::derive(move || i18n.t("downloads-title"))
      on_close=Callback::new(move |()| state.close_overlay())
      node_ref=menu
      on_keydown=Callback::new(on_keydown)
    >
      <div class="downloads-head">
        <h2 class="downloads-heading">{move || i18n.t("downloads-recent")}</h2>
        <button
          class="icon-btn icon-btn-small"
          aria-label=move || i18n.t("downloads-close")
          data-tip=move || i18n.t("downloads-close")
          on:click=move |_| close()
        >
          <Icon name=IconName::Close />
        </button>
      </div>
      <ul class="downloads-list" tabindex="-1">
        // Rows stay while their bytes move (a row made anew would lose its hover).
        <For each=move || entries.get() key=|entry| (entry.file_id.clone(), entry.state) let:entry>
          <DownloadRow state=state entry=entry entries=entries speeds=speeds />
        </For>
      </ul>
    </MenuSurface>
  }
}

/// One download: what it is, how far it got, and what can be done with it.
#[component]
fn DownloadRow(
  state: AppState,
  entry: DownloadEntry,
  entries: Memo<Vec<DownloadEntry>>,
  speeds: Memo<HashMap<String, f64>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = entry.file_id.clone();
  let control = {
    let id = id.clone();
    move |control: DownloadControl| {
      let id = id.clone();
      move |_| state.write(|backend| backend.control_download(&id, control))
    }
  };
  // The row's own entry as the list reads it now, for the bytes.
  let live = {
    let id = id.clone();
    let initial = entry.clone();
    // Keeps the last one while the row leaves after its entry did.
    Memo::new(move |previous: Option<&DownloadEntry>| {
      entries
        .with(|list| list.iter().find(|candidate| candidate.file_id == id).cloned())
        .or_else(|| previous.cloned())
        .unwrap_or_else(|| initial.clone())
    })
  };
  let shows_progress = matches!(
    entry.state,
    DownloadState::Active | DownloadState::Paused | DownloadState::Retrying { .. }
  );
  let fraction = move || {
    live.with(|entry| match entry.total_bytes {
      0 => 0.0,
      total => entry.done_bytes.min(total) as f64 / total as f64,
    })
  };
  let is_broken = matches!(
    entry.state,
    DownloadState::Failed { .. } | DownloadState::Cancelled | DownloadState::Unknown
  );
  let status = move || live.with(|entry| status_line(i18n, entry, speeds.get().get(&entry.file_id).copied()));
  let icon = if is_broken {
    IconName::Alert
  } else {
    IconName::File
  };
  let actions = match entry.state {
    DownloadState::Active | DownloadState::Retrying { .. } => view! {
      <ActionButton label="downloads-pause" icon=IconName::Pause on_click=control(DownloadControl::Pause) />
      <ActionButton label="downloads-cancel" icon=IconName::Close on_click=control(DownloadControl::Cancel) />
    }
    .into_any(),
    DownloadState::Paused => view! {
      <ActionButton label="downloads-resume" icon=IconName::Play on_click=control(DownloadControl::Resume) />
      <ActionButton label="downloads-cancel" icon=IconName::Close on_click=control(DownloadControl::Cancel) />
    }
    .into_any(),
    DownloadState::Saved => view! {
      <ActionButton label="downloads-reveal" icon=IconName::Folder on_click=control(DownloadControl::Reveal) />
      <ActionButton label="downloads-open" icon=IconName::External on_click=control(DownloadControl::Open) />
      <ActionButton label="downloads-forget" icon=IconName::Trash on_click=control(DownloadControl::Forget) />
    }
    .into_any(),
    DownloadState::Failed { .. } | DownloadState::Cancelled | DownloadState::Unknown => view! {
      <ActionButton label="downloads-retry" icon=IconName::Rotate on_click=control(DownloadControl::Resume) />
      <ActionButton label="downloads-forget" icon=IconName::Trash on_click=control(DownloadControl::Forget) />
    }
    .into_any(),
  };

  view! {
    <li class="download-row" class:is-broken=is_broken class:is-paused=entry.state == DownloadState::Paused>
      <span class="download-icon" aria-hidden="true">
        <Icon name=icon />
      </span>
      <div class="download-main">
        <div class="download-name" translate="no" title=entry.file_name.clone()>
          {entry.file_name.clone()}
        </div>
        <div class="download-status">{status}</div>
        {shows_progress.then(|| view! {
          <span
            class="download-progress"
            role="progressbar"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow=move || (fraction() * 100.0).round().to_string()
            aria-label=move || i18n.t("file-progress")
          >
            <span class="download-progress-fill" style:--downloads-progress=move || fraction().to_string()></span>
          </span>
        })}
      </div>
      <div class="download-actions">{actions}</div>
    </li>
  }
}

#[component]
fn ActionButton(
  label: &'static str,
  icon: IconName,
  on_click: impl Fn(web_sys::MouseEvent) + 'static,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button
      class="icon-btn icon-btn-small download-action"
      aria-label=move || i18n.t(label)
      data-tip=move || i18n.t(label)
      on:click=on_click
    >
      <Icon name=icon />
    </button>
  }
}

/// "↓ 70.1 MB / 100 MB • 12.3 MB/s • 3 s left", "100 MB • Done", "Cancelled" …
fn status_line(i18n: I18n, entry: &DownloadEntry, maybe_rate: Option<f64>) -> String {
  let (done, total) = (entry.done_bytes.min(entry.total_bytes), entry.total_bytes);
  let sizes = || {
    i18n.t_with(
      "file-progress-bytes",
      &[("done", &format_bytes(done)), ("total", &format_bytes(total))],
    )
  };
  match entry.state {
    DownloadState::Active => {
      let mut parts = vec![format!("↓ {}", sizes())];
      if let Some(rate) = maybe_rate {
        parts.push(i18n.t_with("downloads-speed", &[("speed", &format_bytes(rate as u64))]));
        if let Some(seconds) = seconds_left(done, total, rate) {
          parts.push(time_left(i18n, seconds));
        }
      }
      parts.join(" • ")
    },
    DownloadState::Retrying { .. } => format!("{} • {}", sizes(), i18n.t("downloads-retrying")),
    DownloadState::Paused => format!("{} • {}", sizes(), i18n.t("downloads-paused")),
    DownloadState::Saved => format!("{} • {}", format_bytes(total), i18n.t("downloads-done")),
    DownloadState::Failed { reason } => i18n.t(short_failure_key(reason)),
    DownloadState::Cancelled => i18n.t("downloads-cancelled"),
    DownloadState::Unknown => i18n.t("downloads-unknown"),
  }
}

/// A failure in a few words, as the row has room for.
fn short_failure_key(reason: FileFailure) -> &'static str {
  match reason {
    FileFailure::Network | FileFailure::Intercepted => "downloads-failed-network",
    FileFailure::StorageUnavailable => "downloads-failed-storage",
    FileFailure::Missing => "downloads-failed-missing",
    FileFailure::Corrupted => "downloads-failed-corrupted",
    FileFailure::LocalFile => "downloads-failed-disk",
    FileFailure::TooLarge => "downloads-failed-too-large",
  }
}

fn time_left(i18n: I18n, seconds: u64) -> String {
  match seconds {
    0..60 => i18n.t_count("downloads-left-seconds", seconds as usize),
    60..3600 => i18n.t_count("downloads-left-minutes", seconds.div_ceil(60) as usize),
    _ => i18n.t_count("downloads-left-hours", seconds.div_ceil(3600) as usize),
  }
}

fn focus_downloads_button() {
  let button = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(".downloads-btn").ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(button) = button {
    drop(button.focus());
  }
}
