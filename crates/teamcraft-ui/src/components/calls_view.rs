//! The call history: missed, incoming and outgoing calls, by day, with a button to call back.

use leptos::prelude::*;
use teamcraft_client_api::calls::{CallDirection, CallHistoryItem};

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::clock::{format_time, local_day};
use crate::components::avatar::AvatarSize;
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::page::{ConversationPortrait, day_heading};
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::use_i18n;
use crate::model::{ConversationKind, TimestampMs};
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
  let days = Memo::new(move |_| {
    let mut all = state.read(|backend| backend.call_history());
    if filter.get() == CallFilter::Missed {
      all.retain(|call| call.direction == CallDirection::Missed);
    }
    call_days(all)
  });
  let filter_button = move |value: CallFilter, label: &'static str| {
    view! {
      <button aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value)>
        {move || i18n.t(label)}
      </button>
    }
  };

  view! {
    <section class="calls-view page-view" aria-label=move || i18n.t("calls-title")>
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
      <div class="calls-list panel-scroll">
        <For
          each=move || days.get()
          key=|day| (day.day, day.calls.iter().map(|call| (call.id.clone(), call.maybe_ended_at)).collect::<Vec<_>>())
          let:day
        >
          <section class="page-day" aria-label=move || day_heading(day.day, day.first_started_at, i18n)>
            <h2 class="page-day-title" aria-hidden="true">{move || day_heading(day.day, day.first_started_at, i18n)}</h2>
            <ul class="page-rows">
              {day.calls.into_iter().map(|call| view! { <CallRow call=call /> }).collect_view()}
            </ul>
          </section>
        </For>
        <Show when=move || days.with(Vec::is_empty) fallback=|| ()>
          <EmptyState icon=IconName::Phone title_key="calls-empty-title" body_key="calls-empty" />
        </Show>
      </div>
    </section>
  }
}

/// The calls of one local day, newest first, for the day headings.
#[derive(Clone, Debug, PartialEq)]
struct CallDay {
  day: i64,
  first_started_at: TimestampMs,
  calls: Vec<CallHistoryItem>,
}

fn call_days(mut calls: Vec<CallHistoryItem>) -> Vec<CallDay> {
  calls.sort_by_key(|call| std::cmp::Reverse(call.started_at));
  let mut days: Vec<CallDay> = Vec::new();
  for call in calls {
    let day = local_day(call.started_at);
    match days.last_mut().filter(|last| last.day == day) {
      Some(last) => last.calls.push(call),
      None => days.push(CallDay {
        day,
        first_started_at: call.started_at,
        calls: vec![call],
      }),
    }
  }
  days
}

/// One call, like a phone's recents: portrait, name, direction and length, time, call back.
#[component]
fn CallRow(call: CallHistoryItem) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let Some(conversation) = state.read(|backend| backend.conversation(call.conversation)) else {
    return ().into_any();
  };
  let title = conversation.name.clone();
  let kind = if conversation.kind == ConversationKind::Direct {
    CallKind::Direct
  } else {
    CallKind::Group
  };
  let (direction_key, direction_class) = match call.direction {
    CallDirection::Missed => ("calls-direction-missed", "is-missed"),
    CallDirection::Incoming => ("calls-direction-incoming", "is-incoming"),
    CallDirection::Outgoing => ("calls-direction-outgoing", "is-outgoing"),
  };
  let duration = call.duration_ms().map(format_duration);
  let when = format_time(call.started_at, locale);
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
  let button_label = {
    let title = title.clone();
    move || {
      let key = if is_ongoing {
        "calls-join"
      } else {
        "calls-call-back"
      };
      format!("{} · {title}", i18n.t(key))
    }
  };

  view! {
    <li class="page-row call-item" class:missed=is_missed>
      <button class="page-row-main" on:click=move |_| state.open_conversation(conversation_id)>
        <ConversationPortrait conversation=conversation size=AvatarSize::Large />
        <span class="page-row-text">
          <span class="page-row-title" translate="no">{title}</span>
          <span class="page-row-detail">
            <Icon name=IconName::ArrowDown class=direction_class />
            {move || i18n.t(direction_key)}
            {duration.map(|text| view! { " · " <span>{text}</span> })}
            {is_ongoing.then(|| view! { " · " <span class="call-live">{move || i18n.t("calls-ongoing")}</span> })}
          </span>
        </span>
        <time class="page-row-time">{when}</time>
      </button>
      <button
        class="icon-btn page-row-action"
        class:is-primary=is_ongoing
        disabled=is_busy
        aria-label=button_label.clone()
        title=move || if has_calls { button_label() } else { i18n.t("calls-unavailable") }
        on:click=call_back
      >
        <Icon name=if is_ongoing { IconName::Video } else { IconName::Phone } />
      </button>
    </li>
  }
  .into_any()
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
