//! The call history: missed, incoming and outgoing calls with a button to call back.

use leptos::prelude::*;
use teamcraft_client_api::calls::{CallDirection, CallHistoryItem};

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::clock::format_date_time;
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::state::use_app_state;

/// What the user sees of the history in one go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallFilter {
  All,
  Missed,
}

#[component]
pub fn CallsView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(CallFilter::All);
  let calls = Memo::new(move |_| {
    let mut all = state.read(|backend| backend.call_history());
    if filter.get() == CallFilter::Missed {
      all.retain(|call| call.direction == CallDirection::Missed);
    }
    all
  });
  let filter_button = move |value: CallFilter, label: &'static str| {
    view! {
      <button aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value)>
        {move || i18n.t(label)}
      </button>
    }
  };

  view! {
    <section class="calls-view" aria-label=move || i18n.t("calls-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("calls-title")}</h1>
        </div>
        <div class="segmented" role="group" aria-label=move || i18n.t("calls-filter")>
          <SlidingIndicator />
          {filter_button(CallFilter::All, "calls-all")}
          {filter_button(CallFilter::Missed, "calls-missed")}
        </div>
      </header>
      <ul class="calls-list panel-scroll">
        <For each=move || calls.get() key=|call| call.id.clone() let:call>
          <CallRow call=call />
        </For>
        <Show when=move || calls.get().is_empty() fallback=|| ()>
          <EmptyState icon=IconName::Phone title_key="calls-empty-title" body_key="calls-empty" as_item=true />
        </Show>
      </ul>
    </section>
  }
}

#[component]
fn CallRow(call: CallHistoryItem) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let conversation = state.read(|backend| backend.conversation(call.conversation));
  let title = conversation
    .as_ref()
    .map(|found| found.name.clone())
    .unwrap_or_default();
  let is_direct = conversation
    .as_ref()
    .is_some_and(|found| found.kind == ConversationKind::Direct);
  let kind = if is_direct {
    CallKind::Direct
  } else {
    CallKind::Group
  };
  let direction_key = match call.direction {
    CallDirection::Missed => "calls-direction-missed",
    CallDirection::Incoming => "calls-direction-incoming",
    CallDirection::Outgoing => "calls-direction-outgoing",
  };
  let duration = call.duration_ms().map(format_duration);
  let when = format_date_time(call.started_at, locale);
  let conversation_id = call.conversation;
  let is_missed = call.direction == CallDirection::Missed;
  let is_ongoing = call.is_ongoing();
  let call = state.call;
  let has_calls = state.read(|backend| backend.has_calls());
  let is_busy = move || !has_calls || call.phase.get() != CallPhase::Idle;
  let call_back = move |_| {
    call.prepare(
      CallRequest {
        conversation: conversation_id,
        kind,
      },
      false,
    );
  };

  view! {
    <li class="call-item" class:missed=is_missed>
      <Icon name=IconName::Phone class="call-direction" />
      <span class="call-body">
        <span class="call-title" translate="no">{title}</span>
        <span class="call-detail">
          {move || i18n.t(direction_key)}
          " · "
          <time>{when}</time>
          {duration.map(|text| view! { " · " <span>{text}</span> })}
          {is_ongoing.then(|| view! { " · " <span>{move || i18n.t("calls-ongoing")}</span> })}
        </span>
      </span>
      <button
        class="btn btn-small"
        disabled=is_busy
        title=move || if has_calls { String::new() } else { i18n.t("calls-unavailable") }
        on:click=call_back
      >
        {move || i18n.t(if is_ongoing { "calls-join" } else { "calls-call-back" })}
      </button>
    </li>
  }
}

/// "3:05" or "1:02:03" for a call of that length.
fn format_duration(milliseconds: i64) -> String {
  let seconds = milliseconds.max(0) / 1000;
  let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
  if hours > 0 {
    format!("{hours}:{minutes:02}:{seconds:02}")
  } else {
    format!("{minutes}:{seconds:02}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn durations_read_as_clock_times() {
    assert_eq!(format_duration(0), "0:00");
    assert_eq!(format_duration(185_000), "3:05");
    assert_eq!(format_duration(3_723_000), "1:02:03");
    assert_eq!(format_duration(-5), "0:00");
  }
}
