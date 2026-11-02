//! The first sync of a newly set up device, under the top bar: what is loading, how far in
//! conversations, and a thin bar. The numbers are the core's; the bar is indeterminate until it has
//! one. The conversation list above works all the while.

use leptos::prelude::*;
use teamcraft_client_api::model::FirstSyncStage;

use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Always there (the shell's grid keeps a row for it), empty when no first sync runs.
#[component]
pub fn FirstSyncBar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let progress = Memo::new(move |_| state.read(|backend| backend.first_sync()));
  view! {
    <div class="first-sync-row">
      {move || {
        progress.get().map(|progress| {
          let label = move || {
            i18n.t(match progress.stage {
              FirstSyncStage::Newest => "first-sync-newest",
              FirstSyncStage::Older => "first-sync-older",
            })
          };
          let count = (progress.total > 0).then(|| {
            let done = progress.done.to_string();
            i18n.t_count_with("first-sync-count", progress.total, &[("done", &done)])
          });
          let maybe_percent = progress.maybe_fraction.map(|fraction| format!("{:.0}", fraction * 100.0));
          view! {
            <div
              class="first-sync-bar"
              class:is-indeterminate=progress.maybe_fraction.is_none()
              role="progressbar"
              aria-label=label
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow=maybe_percent
            >
              <div class="first-sync-text">
                <span>{label}</span>
                {count.map(|count| view! { <span class="first-sync-count">{count}</span> })}
              </div>
              <div class="first-sync-track">
                <div
                  class="first-sync-fill"
                  style:--first-sync-fraction=progress.maybe_fraction.unwrap_or(0.0).to_string()
                ></div>
              </div>
            </div>
          }
        })
      }}
    </div>
  }
}
