//! The desktop app's downloads, like a browser's: a button in the header once there are any, and a
//! list of running and finished downloads. Running ones show their progress, speed and time left;
//! finished ones open with the system's app or show in their folder (the shell looks the path up in
//! the core, the page only names the file).

use std::collections::{HashMap, VecDeque};

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{DownloadItem, DownloadState};
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::failure::ActionFailure;

use crate::clock::{format_time, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::feedback::report_failure;
use crate::i18n::{I18n, use_i18n};
use crate::state::use_app_state;

/// The speed is the average over this long, so it doesn't jump with every progress update.
const SPEED_WINDOW_MS: i64 = 3_000;

/// The downloads button and its list. Only in the desktop app: the browser keeps its own list.
#[component]
pub fn DownloadsButton() -> impl IntoView {
  if !crate::auth::tauri::is_available() {
    return ().into_any();
  }
  let state = use_app_state();
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let downloads = Memo::new(move |_| state.read(|backend| backend.downloads()));
  let speeds = StoredValue::new(SpeedMeter::default());
  let has_any = move || downloads.with(|list| !list.is_empty());
  // All running downloads together, for the ring on the button.
  let maybe_percent = move || {
    downloads.with(|list| {
      let (done, total) = list
        .iter()
        .fold((0u64, 0u64), |(done, total), item| match item.state {
          DownloadState::Active {
            done_bytes,
            total_bytes,
          } => (done + done_bytes, total + total_bytes),
          _ => (done, total),
        });
      (total > 0).then(|| (done.saturating_mul(100) / total).min(100))
    })
  };
  let rows = move || {
    let now = now_ms();
    downloads
      .get()
      .into_iter()
      .map(|item| {
        let maybe_speed = match item.state {
          DownloadState::Active { done_bytes, .. } => speeds
            .try_update_value(|meter| meter.record(&item.file_id, now, done_bytes))
            .flatten(),
          _ => None,
        };
        view! { <DownloadRow item=item maybe_speed=maybe_speed /> }
      })
      .collect_view()
  };
  view! {
    <Show when=has_any fallback=|| ()>
      <div class="downloads">
        <button
          class="icon-btn on-dark downloads-btn"
          class:is-active=move || maybe_percent().is_some()
          aria-haspopup="dialog"
          aria-expanded=move || is_open.get().to_string()
          aria-label=move || i18n.t("downloads-title")
          title=move || i18n.t("downloads-title")
          on:click=move |_| is_open.update(|open| *open = !*open)
        >
          <Icon name=IconName::Download />
          {move || maybe_percent().map(|percent| view! {
            <span class="transfer-ring downloads-ring" style:--media-progress=percent.to_string() aria-hidden="true"></span>
          })}
        </button>
        <Show when=move || is_open.get() fallback=|| ()>
          <MenuSurface
            class="downloads-panel align-end"
            role="dialog"
            label=Signal::derive(move || i18n.t("downloads-title"))
            on_close=Callback::new(move |()| is_open.set(false))
          >
            <div class="downloads-head">
              <span class="downloads-title">{move || i18n.t("downloads-title")}</span>
              <button class="link-btn" on:click=move |_| state.write(|backend| backend.clear_downloads())>
                {move || i18n.t("downloads-clear")}
              </button>
            </div>
            <ul class="downloads-list">{rows}</ul>
          </MenuSurface>
        </Show>
      </div>
    </Show>
  }
  .into_any()
}

/// One download: its name, where it stands and what can be done with it.
#[component]
fn DownloadRow(item: DownloadItem, maybe_speed: Option<u64>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let DownloadItem {
    conversation,
    message,
    file_id,
    file_name,
    size_bytes,
    ..
  } = item.clone();
  let retry_id = file_id.clone();
  let retry = move |_| state.write(|backend| backend.download_attachment(conversation, message, &retry_id));
  let (status, actions) = match item.state {
    DownloadState::Active {
      done_bytes,
      total_bytes,
    } => (
      view! {
        <progress class="file-progress" max=total_bytes.max(1) value=done_bytes></progress>
        <span class="file-state">{active_line(i18n, done_bytes, total_bytes, maybe_speed)}</span>
      }
      .into_any(),
      ().into_any(),
    ),
    DownloadState::Waiting => (
      view! { <span class="file-state">{move || i18n.t("downloads-waiting")}</span> }.into_any(),
      ().into_any(),
    ),
    DownloadState::Saved { finished_at, .. } => {
      let (open_id, reveal_id) = (file_id.clone(), file_id.clone());
      (
        view! {
          <span class="file-state">
            {format!("{} · {}", format_bytes(size_bytes), format_time(finished_at, i18n.locale().tag()))}
          </span>
        }
        .into_any(),
        view! {
          <button
            class="icon-btn"
            aria-label=move || i18n.t("downloads-open")
            data-tip=move || i18n.t("downloads-open")
            on:click=move |_| open_download(open_id.clone(), false)
          >
            <Icon name=IconName::External />
          </button>
          <button
            class="icon-btn"
            aria-label=move || i18n.t("downloads-show-in-folder")
            data-tip=move || i18n.t("downloads-show-in-folder")
            on:click=move |_| open_download(reveal_id.clone(), true)
          >
            <Icon name=IconName::Folder />
          </button>
        }
        .into_any(),
      )
    },
    DownloadState::Failed { .. }
    | DownloadState::Interrupted
    | DownloadState::Removed
    | DownloadState::Unknown => {
      let key = match item.state {
        DownloadState::Interrupted => "downloads-interrupted",
        DownloadState::Removed => "downloads-removed",
        _ => "downloads-failed",
      };
      (
        view! { <span class="file-error">{move || i18n.t(key)}</span> }.into_any(),
        view! {
          <button class="link-btn" on:click=retry>{move || i18n.t("downloads-retry")}</button>
        }
        .into_any(),
      )
    },
  };
  view! {
    <li class="download-row">
      <div class="file-meta">
        <span class="file-name" translate="no" title=file_name.clone()>{file_name.clone()}</span>
        {status}
      </div>
      <div class="download-actions">{actions}</div>
    </li>
  }
}

/// "12 MB / 48 MB · 3.2 MB/s · 10 s left"; speed and time once there is a speed.
fn active_line(i18n: I18n, done_bytes: u64, total_bytes: u64, maybe_speed: Option<u64>) -> String {
  let mut line = format!("{} / {}", format_bytes(done_bytes), format_bytes(total_bytes));
  if let Some(speed) = maybe_speed.filter(|speed| *speed > 0) {
    let left_secs = total_bytes.saturating_sub(done_bytes).div_ceil(speed);
    let left = match left_secs {
      0..60 => i18n.t_with("downloads-left-seconds", &[("seconds", &left_secs.to_string())]),
      _ => i18n.t_with(
        "downloads-left-minutes",
        &[("minutes", &left_secs.div_ceil(60).to_string())],
      ),
    };
    let rate = i18n.t_with("downloads-speed", &[("speed", &format_bytes(speed))]);
    line = format!("{line} · {rate} · {left}");
  }
  line
}

/// Opens a saved file, or shows it in its folder.
fn open_download(file_id: String, reveal: bool) {
  spawn_local(async move {
    let args = serde_json::json!({ "file_id": file_id, "reveal": reveal });
    let Ok(js_args) = js_sys::JSON::parse(&args.to_string()) else {
      return;
    };
    if let Err(error) = crate::auth::tauri::invoke("core_open_download", js_args).await {
      report_failure(ActionFailure::from_wire(&error.to_string()));
    }
  });
}

/// Bytes per second of each running download, averaged over [`SPEED_WINDOW_MS`].
#[derive(Default)]
struct SpeedMeter {
  samples: HashMap<String, VecDeque<(i64, u64)>>,
}

impl SpeedMeter {
  /// Notes how far `file_id` got at `now_ms`; its speed once two samples span some time.
  fn record(&mut self, file_id: &str, now_ms: i64, done_bytes: u64) -> Option<u64> {
    let samples = self.samples.entry(file_id.to_owned()).or_default();
    if samples.back().is_none_or(|(_, last)| *last != done_bytes) {
      samples.push_back((now_ms, done_bytes));
    }
    while samples
      .front()
      .is_some_and(|(at, _)| now_ms.saturating_sub(*at) > SPEED_WINDOW_MS && samples.len() > 2)
    {
      samples.pop_front();
    }
    let (&(first_at, first_bytes), &(last_at, last_bytes)) = (samples.front()?, samples.back()?);
    let elapsed_ms = u64::try_from(last_at.saturating_sub(first_at))
      .ok()
      .filter(|ms| *ms > 0)?;
    Some(last_bytes.saturating_sub(first_bytes).saturating_mul(1000) / elapsed_ms)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_speed_needs_two_samples_apart_in_time() {
    let mut meter = SpeedMeter::default();
    assert_eq!(meter.record("a", 1_000, 0), None);
    assert_eq!(meter.record("a", 2_000, 1_000_000), Some(1_000_000));
  }

  #[test]
  fn the_speed_is_averaged_over_the_window_only() {
    let mut meter = SpeedMeter::default();
    meter.record("a", 0, 0);
    meter.record("a", 1_000, 10_000_000);
    meter.record("a", 5_000, 11_000_000);
    // The burst in the first second is older than the window.
    assert_eq!(meter.record("a", 6_000, 12_000_000), Some(1_000_000));
  }

  #[test]
  fn files_are_measured_apart() {
    let mut meter = SpeedMeter::default();
    meter.record("a", 0, 0);
    meter.record("b", 0, 0);
    assert_eq!(meter.record("a", 1_000, 500), Some(500));
    assert_eq!(meter.record("b", 1_000, 2_000), Some(2_000));
  }
}
