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

use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::components::top_bar::DownloadsSlot;
use crate::download_speed::{SpeedMeter, seconds_left};
use crate::i18n::{I18n, use_i18n};
use crate::settings;
use crate::state::{AppState, Overlay, use_app_state};

/// The button in the top bar; nothing until the first file was saved.
#[component]
pub fn DownloadsButton() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let user = state.me().id;
  // The top bar's placeholder gives way to the button (or to nothing, once the core says so).
  if let Some(slot) = use_context::<DownloadsSlot>() {
    slot.0.set(true);
  }
  // Until the core first answers, the button is where the last start left it: the search beside it
  // doesn't move when the answer comes.
  let was_shown = settings::load_string(&settings::downloads_shown_key(user)).as_deref() == Some("1");
  let listed = Memo::new(move |previous: Option<&Listed>| {
    Listed::next(previous, state.read(|backend| backend.downloads()))
  });
  let entries = Memo::new(move |_| listed.with(|listed| listed.entries.clone()));
  let is_shown = Memo::new(move |_| listed.with(|listed| listed.is_shown(was_shown)));
  let is_answered = Memo::new(move |_| listed.with(|listed| listed.is_answered));
  Effect::new(move |_| {
    if is_answered.get() {
      settings::store_string(
        &settings::downloads_shown_key(user),
        if is_shown.get() { "1" } else { "0" },
      );
    }
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
    <Show when=move || is_shown.get() fallback=|| ()>
      <div class="downloads">
        <button
          class="icon-btn on-dark downloads-btn"
          class:is-active=move || summary.get().is_active
          class:is-attention=move || summary.get().needs_attention
          class:is-done=move || is_done_flashing.get()
          aria-haspopup="dialog"
          aria-expanded=move || is_open().to_string()
          aria-label=move || button_label(i18n, summary.get())
          title=move || i18n.t("downloads-title")
          on:click=toggle
          on:animationend=move |event: web_sys::AnimationEvent| {
            if event.animation_name() == "downloads-done" {
              is_done_flashing.set(false);
            }
          }
        >
          <svg class="downloads-arrow" viewBox="5 2 14 14" width="14" height="14" aria-hidden="true" focusable="false">
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

/// The list as the core last told it. Visibility follows only the core's answers: a read still on
/// its way (`None`) keeps what was there, so the button never blinks while the app catches up.
#[derive(Clone, Debug, Default, PartialEq)]
struct Listed {
  entries: Vec<DownloadEntry>,
  /// The core answered at least once since the app started.
  is_answered: bool,
}

impl Listed {
  fn next(maybe_previous: Option<&Self>, maybe_read: Option<Vec<DownloadEntry>>) -> Self {
    match maybe_read {
      Some(entries) => Self {
        entries,
        is_answered: true,
      },
      None => maybe_previous.cloned().unwrap_or_default(),
    }
  }

  /// Whether the button shows: once answered, whether there is anything to list; before that, what
  /// the last start ended with.
  fn is_shown(&self, was_shown: bool) -> bool {
    if self.is_answered {
      !self.entries.is_empty()
    } else {
      was_shown
    }
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
          title=move || i18n.t("downloads-close")
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
  // A click on the row's name or icon: a saved file opens at once; a running or paused download is
  // marked to open when it is saved (Chrome's way), and a second click takes the mark back.
  let open_control = move || {
    live.with(|entry| match entry.state {
      DownloadState::Saved => Some(DownloadControl::Open),
      DownloadState::Active | DownloadState::Retrying { .. } | DownloadState::Paused => {
        Some(if entry.is_open_when_done {
          DownloadControl::KeepClosed
        } else {
          DownloadControl::OpenWhenDone
        })
      },
      _ => None,
    })
  };
  let is_waiting_to_open = move || live.with(|entry| entry.is_open_when_done);
  let is_openable = matches!(
    entry.state,
    DownloadState::Saved | DownloadState::Active | DownloadState::Retrying { .. } | DownloadState::Paused
  );
  let open_title = move || match open_control() {
    Some(DownloadControl::Open) => i18n.t("downloads-open"),
    Some(DownloadControl::KeepClosed) => i18n.t("downloads-keep-closed"),
    Some(_) => i18n.t("downloads-open-when-done"),
    None => String::new(),
  };
  let on_open = {
    let id = id.clone();
    move |_| {
      if let Some(control) = open_control() {
        state.write(|backend| backend.control_download(&id, control));
      }
    }
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

  // The name and icon area is its own button, beside the action buttons (never around them), so
  // Enter opens it and the actions' clicks can't reach it. Spans only: a button holds phrasing content.
  let name = entry.file_name.clone();
  let content = move || {
    view! {
      <span class="download-icon" aria-hidden="true">
        <Icon name=icon />
      </span>
      <span class="download-main">
        <span class="download-name" translate="no">
          {name.clone()}
        </span>
        <span class="download-status">{status}</span>
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
      </span>
    }
  };
  let open_area = if is_openable {
    view! {
      <button
        type="button"
        class="download-open"
        title=open_title
        aria-pressed=move || {
          open_control()
            .filter(|control| *control != DownloadControl::Open)
            .map(|_| is_waiting_to_open().to_string())
        }
        on:click=on_open
      >
        {content()}
      </button>
    }
    .into_any()
  } else {
    view! { <span class="download-open" title=entry.file_name.clone()>{content()}</span> }.into_any()
  };

  view! {
    <li class="download-row" class:is-broken=is_broken class:is-paused=entry.state == DownloadState::Paused>
      {open_area}
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
      title=move || i18n.t(label)
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
  let mut parts: Vec<String> = match entry.state {
    DownloadState::Active => {
      let mut parts = vec![format!("↓ {}", sizes())];
      if let Some(rate) = maybe_rate {
        parts.push(i18n.t_with("downloads-speed", &[("speed", &format_bytes(rate as u64))]));
        if let Some(seconds) = seconds_left(done, total, rate) {
          parts.push(time_left(i18n, seconds));
        }
      }
      parts
    },
    DownloadState::Retrying { .. } => vec![sizes(), i18n.t("downloads-retrying")],
    DownloadState::Paused => vec![sizes(), i18n.t("downloads-paused")],
    _ => return finished_status_line(i18n, entry),
  };
  if entry.is_open_when_done {
    parts.push(i18n.t("downloads-opens-when-done"));
  }
  parts.join(" • ")
}

/// The status of a download that is over: saved, failed or cancelled.
fn finished_status_line(i18n: I18n, entry: &DownloadEntry) -> String {
  let total = entry.total_bytes;
  match entry.state {
    DownloadState::Saved => format!("{} • {}", format_bytes(total), i18n.t("downloads-done")),
    DownloadState::Failed { reason } => i18n.t(short_failure_key(reason)),
    DownloadState::Cancelled => i18n.t("downloads-cancelled"),
    DownloadState::Unknown => i18n.t("downloads-unknown"),
    // Handled by `status_line`.
    DownloadState::Active | DownloadState::Retrying { .. } | DownloadState::Paused => String::new(),
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

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::model::ConversationId;

  mod visibility {
    use super::*;

    #[test]
    fn reads_still_on_their_way_keep_the_button() {
      let shown = Listed::next(None, Some(vec![saved("1")]));
      // The core is busy catching up: one read after another comes back without an answer.
      let mut listed = shown.clone();
      for _ in 0..50 {
        listed = Listed::next(Some(&listed), None);
        assert!(listed.is_shown(false));
      }
      assert_eq!(listed, shown);
    }

    #[test]
    fn the_same_answer_again_is_no_change() {
      let first = Listed::next(None, Some(vec![saved("1"), saved("2")]));
      let again = Listed::next(Some(&first), Some(vec![saved("1"), saved("2")]));
      assert_eq!(first, again, "the memo doesn't notify, nothing re-renders");
    }

    #[test]
    fn before_the_first_answer_the_last_start_decides() {
      let waiting = Listed::next(None, None);
      assert!(waiting.is_shown(true));
      assert!(!waiting.is_shown(false));
    }

    #[test]
    fn an_answer_decides_over_the_last_start() {
      let cleared = Listed::next(None, Some(Vec::new()));
      assert!(!cleared.is_shown(true), "the user cleared the list meanwhile");
      let saved_since = Listed::next(None, Some(vec![saved("1")]));
      assert!(saved_since.is_shown(false));
    }
  }

  fn saved(file_id: &str) -> DownloadEntry {
    DownloadEntry {
      file_id: file_id.to_owned(),
      conversation: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      file_name: format!("{file_id}.txt"),
      total_bytes: 10,
      done_bytes: 10,
      started_at_ms: 1,
      state: DownloadState::Saved,
      is_open_when_done: false,
    }
  }
}
