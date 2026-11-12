//! The body of the meetings page (#956): the views, the meeting that runs or comes next with its
//! Join, the invitations still waiting for an answer, and one compact row per meeting under
//! sticky day headings. A row carries one main action; the rest is in its menu, the detail opens
//! in place. What is shown and in which order is `meeting_list`'s.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{MeetingId, MeetingResponse, RsvpStatus};

use crate::clock::{format_date, format_time, local_day, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::empty_state::{EmptyAction, EmptyState};
use crate::components::icons::{Icon, IconName};
use crate::components::link_menu::move_focus;
use crate::components::meeting_controls::{HostControls, RsvpList};
use crate::components::meetings_view::{GuestLinks, Lobby, MeetingLinkButton, join_meeting};
use crate::components::pending_button::PendingButton;
use crate::components::sheet::PopoverBackdrop;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::meeting_list::{
  DayLabel, MeetingState, MeetingsTab, PAGE_ROWS, day_label, duration_minutes, is_hosted_by, is_joinable,
  meeting_days, meeting_state, now_or_next, pending_answers, series_summary,
};
use crate::meetings::{MeetingsHub, attending_users, hosts_lobby, is_over, my_rsvp};
use crate::model::{SpaceId, TimestampMs};
use crate::reactive::{SetIfAlive, use_interval_while_visible};
use crate::state::{Overlay, use_app_state};

/// How often "in 12 min" and the running state are brought up to date while the page is visible.
const TICK_MS: u64 = 30_000;
/// Attendee pictures on a row; the count follows in text.
const MAX_FACES: usize = 3;
const ANSWERS: [RsvpStatus; 3] = [RsvpStatus::Accepted, RsvpStatus::Tentative, RsvpStatus::Declined];

/// A row's identity across reloads: one occurrence of one meeting.
type RowId = (MeetingId, TimestampMs);

/// The rows whose detail is open, shared by the page so "Details" in the now/next area opens the
/// row below.
#[derive(Clone, Copy)]
struct OpenRows(RwSignal<HashSet<RowId>>);

/// The views, the now/next area, the answers section and the day groups.
#[component]
pub(crate) fn MeetingsBody(
  /// The upcoming meetings of the page's scope (`meetings_in_scope`).
  meetings: Memo<Vec<MeetingResponse>>,
  scope: Memo<Option<SpaceId>>,
  can_create: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  // The server's clock, so a wrong local clock doesn't say "in 3 hours" for a running meeting.
  let server_now = move || hub.clock.get_untracked().now(now_ms());
  let now = RwSignal::new(server_now());
  use_interval_while_visible(TICK_MS, move || now.set(server_now()));
  // Each reload moves the clock along too.
  Effect::new(move |_| {
    meetings.track();
    now.set(server_now());
  });

  let tab = RwSignal::new(MeetingsTab::Upcoming);
  let rows = RwSignal::new(PAGE_ROWS);
  let open_rows = OpenRows(RwSignal::new(HashSet::new()));
  provide_context(open_rows);
  let choose = move |chosen: MeetingsTab| {
    tab.set(chosen);
    rows.set(PAGE_ROWS);
    if chosen == MeetingsTab::Past {
      spawn_local(async move { hub.refresh_past(server_now()).await });
    }
  };
  let past = Memo::new(move |_| {
    let maybe_space = scope.get();
    hub.maybe_past.with(|maybe_past| {
      maybe_past.as_ref().map(|all| {
        all
          .iter()
          .filter(|meeting| maybe_space.is_none() || meeting.maybe_space == maybe_space)
          .cloned()
          .collect::<Vec<_>>()
      })
    })
  });
  let shown = Memo::new(move |_| {
    let now = now.get();
    let tab = tab.get();
    match tab {
      MeetingsTab::Past => past.with(|maybe_past| maybe_past.as_deref().map(|all| tab.select(all, me, now))),
      _ => hub
        .is_loaded
        .get()
        .then(|| meetings.with(|all| tab.select(all, me, now))),
    }
  });
  let days = Memo::new(move |_| {
    shown.with(|maybe_shown| {
      maybe_shown
        .as_deref()
        .map(|all| meeting_days(all, rows.get()))
        .unwrap_or_default()
    })
  });
  let has_more =
    move || shown.with(|maybe_shown| maybe_shown.as_ref().is_some_and(|all| all.len() > rows.get()));
  let is_empty = move || shown.with(|maybe_shown| maybe_shown.as_ref().is_some_and(Vec::is_empty));
  let is_loading = move || shown.with(Option::is_none);
  let hero = Memo::new(move |_| {
    (tab.get() == MeetingsTab::Upcoming)
      .then(|| meetings.with(|all| now_or_next(all, now.get())))
      .flatten()
  });
  let answers = Memo::new(move |_| {
    matches!(tab.get(), MeetingsTab::Upcoming | MeetingsTab::Invited)
      .then(|| meetings.with(|all| pending_answers(all, me, now.get())))
      .unwrap_or_default()
  });
  let reach = move || {
    shown.with(|maybe_shown| {
      maybe_shown
        .as_ref()
        .and_then(|all| all.last())
        .map(|last| format_date(last.starts_at.0, i18n.locale().tag()))
    })
  };

  let tab_button = move |value: MeetingsTab| {
    view! {
      <button aria-pressed=move || (tab.get() == value).to_string() on:click=move |_| choose(value)>
        {move || i18n.t(value.label_key())}
      </button>
    }
  };

  view! {
    <div class="meetings-toolbar">
      <div class="segmented meetings-tabs" role="group" aria-label=move || i18n.t("meetings-views")>
        <SlidingIndicator />
        {MeetingsTab::ALL.into_iter().map(tab_button).collect_view()}
      </div>
    </div>
    <div class="meetings-list panel-scroll">
      <div class="meetings-layout" class:has-aside=move || hero.with(Option::is_some) || answers.with(|all| !all.is_empty())>
        <div class="meetings-aside">
          {move || hero.get().map(|meeting| view! { <NowNext meeting=meeting now=now /> })}
          <Show when=move || answers.with(|all| !all.is_empty()) fallback=|| ()>
            <NeedsAnswer answers=answers />
          </Show>
        </div>
        <div class="meetings-main" aria-busy=move || is_loading().to_string()>
          <For each=move || days.get() key=|day| (day.day, day.meetings.iter().map(row_key).collect::<Vec<_>>()) let:day>
            <section class="page-day meetings-day-group" aria-label=move || day_heading(day.day, day.first_start, now.get(), i18n)>
              <h2 class="page-day-title meetings-day-title" aria-hidden="true">
                {move || day_heading(day.day, day.first_start, now.get(), i18n)}
              </h2>
              <ul class="meeting-rows">
                {day.meetings.into_iter().map(|meeting| view! { <MeetingRow meeting=meeting now=now /> }).collect_view()}
              </ul>
            </section>
          </For>
          <Show when=has_more fallback=|| ()>
            <button class="btn btn-quiet meetings-more" on:click=move |_| rows.update(|count| *count += PAGE_ROWS)>
              {move || i18n.t("meetings-show-more")}
            </button>
          </Show>
          // How far the server's list reaches; the other views filter it, so it says nothing there.
          <Show when=move || !has_more() && tab.get() == MeetingsTab::Upcoming fallback=|| ()>
            {move || reach().map(|date| view! { <p class="meetings-reach">{i18n.t_with("meetings-reach", &[("date", &date)])}</p> })}
          </Show>
          <Show when=is_loading fallback=|| ()>
            <p class="meetings-loading" role="status">{move || i18n.t("meetings-loading")}</p>
          </Show>
          <Show when=is_empty fallback=|| ()>
            {move || {
              let current = tab.get();
              let can_schedule = can_create && matches!(current, MeetingsTab::Upcoming | MeetingsTab::Hosted);
              view! {
                <EmptyState
                  icon=IconName::Calendar
                  title_key=current.empty_title_key()
                  body_key=current.empty_body_key(scope.get().is_some())
                  maybe_action=can_schedule.then(|| EmptyAction {
                    label_key: "meetings-schedule",
                    on_click: Callback::new(move |()| state.overlay.set(Overlay::ScheduleMeeting)),
                  })
                />
              }
            }}
          </Show>
        </div>
      </div>
    </div>
  }
}

/// The meeting that runs, or the next one: its state in words and a large Join.
#[component]
fn NowNext(meeting: MeetingResponse, now: RwSignal<TimestampMs>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me().id;
  let OpenRows(open_rows) = expect_context::<OpenRows>();
  let locale = i18n.locale().tag();
  let row: RowId = (meeting.id, meeting.starts_at.0);
  let span = format!(
    "{}–{}",
    format_time(meeting.starts_at.0, locale),
    format_time(meeting.ends_at.0, locale)
  );
  let starts_at = meeting.starts_at.0;
  let title = meeting.title.clone();
  let organizer = organizer_line(state, &meeting, me, i18n);
  let meeting_for_join = meeting.clone();
  let is_host = meeting.organizer_user_id == me;
  let when = move || {
    let now = now.get();
    match meeting_state(&meeting, now) {
      MeetingState::Running => i18n.t_with("meetings-hero-running", &[("time", &span)]),
      MeetingState::StartsSoon { minutes } => i18n.t_count_with(
        "meetings-hero-soon",
        usize::try_from(minutes).unwrap_or(1),
        &[("time", &span)],
      ),
      _ => {
        let day = day_heading(local_day(starts_at), starts_at, now, i18n);
        format!("{day}, {span}")
      },
    }
  };
  let is_running = move || starts_at <= now.get();
  let meeting_for_click = StoredValue::new(meeting_for_join);
  let can_join = move || meeting_for_click.with_value(|meeting| is_joinable(meeting, now.get()));
  let join = move |_| meeting_for_click.with_value(|meeting| open_meeting(state, i18n, meeting, is_host));
  let show_details = move |_| {
    open_rows.update(|open| {
      open.insert(row);
    });
    scroll_to_row(row);
  };

  view! {
    <section class="meetings-hero" class:is-running=is_running aria-labelledby="meetings-hero-title">
      <p class="meetings-hero-eyebrow">
        {move || i18n.t(if is_running() { "meetings-hero-now" } else { "meetings-hero-next" })}
      </p>
      <h2 class="meetings-hero-title" id="meetings-hero-title" translate="no">{title}</h2>
      <p class="meetings-hero-when">{when}</p>
      <p class="meetings-hero-people" translate="no">{organizer}</p>
      <div class="meetings-hero-actions">
        <Show
          when=can_join
          fallback=move || view! { <button class="btn" on:click=show_details>{move || i18n.t("meetings-details")}</button> }
        >
          <button class="btn btn-primary btn-large" on:click=join>
            <Icon name=IconName::Video />
            <span>{move || i18n.t(if is_host && !is_running() { "meetings-start" } else { "meetings-join" })}</span>
          </button>
        </Show>
      </div>
    </section>
  }
}

/// Invitations of others the user hasn't answered: one per meeting, with the three answers.
#[component]
fn NeedsAnswer(answers: Memo<Vec<crate::meeting_list::PendingAnswer>>) -> impl IntoView {
  let i18n = use_i18n();
  let count = Signal::derive(move || answers.with(Vec::len));
  view! {
    <section class="meetings-answers" aria-labelledby="meetings-answers-title">
      <h2 class="meetings-section-title" id="meetings-answers-title">
        {move || i18n.t("meetings-needs-answer")}
        <span class="meetings-section-count">{move || count.get().to_string()}</span>
      </h2>
      <ul class="meetings-answer-list">
        <For each=move || answers.get() key=|answer| (answer.meeting.id, answer.meeting.starts_at.0, answer.more_occurrences) let:answer>
          <AnswerRow answer=answer />
        </For>
      </ul>
    </section>
  }
}

#[component]
fn AnswerRow(answer: crate::meeting_list::PendingAnswer) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let meeting = answer.meeting;
  let id = meeting.id;
  let locale = i18n.locale().tag();
  let when = format!(
    "{}, {}",
    format_date(meeting.starts_at.0, locale),
    format_time(meeting.starts_at.0, locale)
  );
  let maybe_series = series_summary(&meeting);
  let more = answer.more_occurrences;
  let organizer = organizer_line(state, &meeting, me, i18n);
  let pending = PendingAction::new();
  let answer_with = move |status: RsvpStatus| {
    pending.spawn(async move {
      hub
        .client()
        .set_rsvp(id, status)
        .await
        .map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      Ok(())
    });
  };
  view! {
    <li class="meetings-answer">
      <div class="meetings-answer-text">
        <span class="meetings-answer-title" translate="no">{meeting.title.clone()}</span>
        <span class="meetings-answer-meta">
          {when}
          {maybe_series.map(|series| {
            let (key, count) = (series.key(), series.interval as usize);
            view! {
              <span class="meetings-answer-series">
                <Icon name=IconName::Repeat />
                {move || i18n.t_count(key, count)}
              </span>
            }
          })}
        </span>
        <span class="meetings-answer-meta" translate="no">{organizer}</span>
        {(more > 0).then(|| view! { <span class="meetings-answer-meta">{move || i18n.t_count("meetings-answer-covers", more)}</span> })}
      </div>
      <div class="meetings-answer-buttons" role="group" aria-label=move || i18n.t("meetings-rsvp")>
        {ANSWERS
          .into_iter()
          .map(|status| {
            let key = format!("meetings-rsvp-{}", status.as_str());
            let is_primary = status == RsvpStatus::Accepted;
            let class = if is_primary { "btn btn-primary btn-small" } else { "btn btn-small" };
            view! {
              <PendingButton pending=pending class=class on_click=move |_| answer_with(status.clone())>
                {move || i18n.t(&key)}
              </PendingButton>
            }
          })
          .collect_view()}
      </div>
    </li>
  }
}

/// One meeting occurrence: time and length, title, state, people, the main action and the menu;
/// the detail opens below it.
#[component]
fn MeetingRow(meeting: MeetingResponse, now: RwSignal<TimestampMs>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let OpenRows(open_rows) = expect_context::<OpenRows>();
  let me = state.me().id;
  let locale = i18n.locale().tag();
  let id = meeting.id;
  let row: RowId = (id, meeting.starts_at.0);
  let detail_id = format!("meeting-detail-{id}-{}", meeting.starts_at.0);
  let is_host = meeting.organizer_user_id == me;
  let is_hosting = is_hosted_by(&meeting, me);
  let rsvp = my_rsvp(&meeting, me);
  let start = format_time(meeting.starts_at.0, locale);
  let length = duration_label(duration_minutes(&meeting), i18n);
  let maybe_series = series_summary(&meeting);
  let has_lobby = hosts_lobby(&meeting, me);
  let faces: Vec<crate::model::User> = state.read(|backend| {
    meeting
      .attendees
      .iter()
      .filter_map(|attendee| attendee.maybe_user_id)
      .filter_map(|user| backend.user(user))
      .take(MAX_FACES)
      .collect()
  });
  let people = meeting.attendees.len();
  let subtitle = organizer_line(state, &meeting, me, i18n);
  let meeting = StoredValue::new(meeting);
  let row_state = Memo::new(move |_| meeting.with_value(|meeting| meeting_state(meeting, now.get())));
  let is_over_now = move || {
    matches!(
      row_state.get(),
      MeetingState::Ended | MeetingState::Cancelled { .. }
    )
  };
  let can_join = Memo::new(move |_| meeting.with_value(|meeting| is_joinable(meeting, now.get())));
  // A host's running meeting opens with its host controls.
  if is_hosting
    && meeting.with_value(|meeting| meeting_state(meeting, now.get_untracked())) == MeetingState::Running
  {
    open_rows.update(|open| {
      open.insert(row);
    });
  }
  let is_open = Memo::new(move |_| open_rows.with(|open| open.contains(&row)));
  let toggle = move |_| {
    open_rows.update(|open| {
      if !open.remove(&row) {
        open.insert(row);
      }
    })
  };
  let waiting = Memo::new(move |_| {
    hub.lobbies.with(|lobbies| {
      lobbies
        .iter()
        .find(|(meeting, _)| *meeting == id)
        .map_or(0, |(_, entries)| entries.len())
    })
  });
  let join = move |_| meeting.with_value(|meeting| open_meeting(state, i18n, meeting, is_host));
  let shows_guest_links = RwSignal::new(false);

  view! {
    <li
      class="meeting-row"
      class:is-open=move || is_open.get()
      class:is-running=move || row_state.get() == MeetingState::Running
      class:is-over=move || row_state.get() == MeetingState::Ended
      class:is-cancelled=move || matches!(row_state.get(), MeetingState::Cancelled { .. })
      data-meeting-row=format!("{id}-{}", row.1)
    >
      <div class="meeting-row-line">
        <button
          type="button"
          class="meeting-row-toggle"
          aria-expanded=move || is_open.get().to_string()
          aria-controls=detail_id.clone()
          on:click=toggle
        >
          <span class="meeting-row-time">
            <span class="meeting-row-start">{start}</span>
            <span class="meeting-row-length">{length}</span>
          </span>
          <span class="meeting-row-text">
            <span class="meeting-row-title" translate="no">
              {meeting.with_value(|meeting| meeting.title.clone())}
            </span>
            <span class="meeting-row-sub">
              <StateChip state=row_state waiting=waiting />
              {maybe_series.map(|series| {
                let (key, count) = (series.key(), series.interval as usize);
                view! {
                  <span class="meeting-row-series" title=move || i18n.t_count(key, count)>
                    <Icon name=IconName::Repeat />
                    <span class="visually-hidden">{move || i18n.t_count(key, count)}</span>
                  </span>
                }
              })}
              <span class="meeting-row-who" translate="no">{subtitle}</span>
            </span>
          </span>
          <Icon name=IconName::ChevronDown />
        </button>
        <span class="avatar-stack meeting-row-faces" aria-hidden="true">
          {faces.into_iter().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> }).collect_view()}
          {(people > MAX_FACES).then(|| view! { <span class="meeting-row-more">{format!("+{}", people - MAX_FACES)}</span> })}
        </span>
        <div class="meeting-row-actions">
          <Show when=move || can_join.get() fallback=|| ()>
            <button class="btn btn-primary btn-small" on:click=join>
              {move || {
                let key = if is_host && row_state.get() != MeetingState::Running { "meetings-start" } else { "meetings-join" };
                i18n.t(key)
              }}
            </button>
          </Show>
          // The answer matters while the meeting is still to come.
          {move || (!is_host && !is_over_now())
            .then(|| rsvp.clone())
            .flatten()
            .map(|answer| view! { <RsvpMenu meeting=id answer=answer /> })}
          <RowMenu meeting=meeting row=row now=now shows_guest_links=shows_guest_links />
        </div>
      </div>
      <Show when=move || is_open.get() fallback=|| ()>
        <MeetingDetail meeting=meeting now=now shows_guest_links=shows_guest_links detail_id=format!("meeting-detail-{id}-{}", row.1) />
      </Show>
      {has_lobby.then(|| view! { <Lobby meeting=id /> })}
    </li>
  }
}

/// The meeting's state in a few words, where it says something: running, soon, ended,
/// cancelled, guests waiting.
#[component]
fn StateChip(state: Memo<MeetingState>, waiting: Memo<usize>) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    let waiting = waiting.get();
    let (class, text) = match state.get() {
      _ if waiting > 0 => (
        "meeting-chip is-waiting",
        i18n.t_count("meetings-state-waiting", waiting),
      ),
      MeetingState::Running => ("meeting-chip is-running", i18n.t("meetings-live")),
      MeetingState::StartsSoon { minutes } => (
        "meeting-chip is-soon",
        i18n.t_count("meetings-state-soon", usize::try_from(minutes).unwrap_or(1)),
      ),
      MeetingState::Ended => ("meeting-chip", i18n.t("meetings-state-ended")),
      MeetingState::Cancelled { is_holiday: true } => {
        ("meeting-chip is-cancelled", i18n.t("meetings-holiday"))
      },
      MeetingState::Cancelled { is_holiday: false } => {
        ("meeting-chip is-cancelled", i18n.t("meetings-cancelled"))
      },
      MeetingState::Later => return None,
    };
    Some(view! { <span class=class>{text}</span> })
  }
}

/// The user's answer as one control: the button says the current answer, the menu changes it.
#[component]
fn RsvpMenu(meeting: MeetingId, answer: RsvpStatus) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let is_open = RwSignal::new(false);
  let trigger = NodeRef::<html::Button>::new();
  let menu = NodeRef::<html::Div>::new();
  let pending = PendingAction::new();
  let current = answer.clone();
  let (icon, key, tone) = match answer {
    RsvpStatus::Accepted => (IconName::Check, "meetings-rsvp-state-accepted", "is-accepted"),
    RsvpStatus::Tentative => (IconName::Help, "meetings-rsvp-state-tentative", "is-tentative"),
    RsvpStatus::Declined => (IconName::Close, "meetings-rsvp-state-declined", "is-declined"),
    _ => (IconName::Clock, "meetings-rsvp-state-none", "is-none"),
  };
  let choose = move |status: RsvpStatus| {
    is_open.set(false);
    pending.spawn(async move {
      hub
        .client()
        .set_rsvp(meeting, status)
        .await
        .map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      Ok(())
    });
  };
  focus_first_item(is_open, menu);
  view! {
    <div class="menu-anchor">
      <button
        type="button"
        class=format!("btn btn-small rsvp-button {tone}")
        node_ref=trigger
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || format!("{}: {}", i18n.t("meetings-rsvp"), i18n.t(key))
        prop:disabled=move || pending.is_pending()
        on:click=move |_| is_open.update(|open| *open = !*open)
      >
        <Icon name=icon />
        <span>{move || i18n.t(key)}</span>
        <Icon name=IconName::ChevronDown />
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <PopoverBackdrop on_close=Callback::new(move |()| is_open.set(false)) popover=menu />
        <div
          class="popover menu-popover"
          role="menu"
          node_ref=menu
          aria-label=move || i18n.t("meetings-rsvp")
          on:keydown=move |ev| menu_keydown(ev, menu, is_open, trigger)
        >
          {ANSWERS
            .into_iter()
            .map(|status| {
              let is_current = status == current;
              let label = format!("meetings-rsvp-state-{}", status.as_str());
              view! {
                <button
                  type="button"
                  class="menu-item"
                  role="menuitemradio"
                  aria-checked=is_current.to_string()
                  on:click=move |_| choose(status.clone())
                >
                  <span class="menu-check">{is_current.then(|| view! { <Icon name=IconName::Check /> })}</span>
                  <span class="menu-label">{move || i18n.t(&label)}</span>
                </button>
              }
            })
            .collect_view()}
        </div>
      </Show>
    </div>
  }
}

/// A row's other actions: copy the link, invite guests, edit, cancel, restore, remove.
#[component]
fn RowMenu(
  meeting: StoredValue<MeetingResponse>,
  row: RowId,
  now: RwSignal<TimestampMs>,
  shows_guest_links: RwSignal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let OpenRows(open_rows) = expect_context::<OpenRows>();
  let me = state.me().id;
  let is_open = RwSignal::new(false);
  let trigger = NodeRef::<html::Button>::new();
  let menu = NodeRef::<html::Div>::new();
  let (
    id,
    conversation,
    starts_at,
    is_host,
    is_hosting,
    is_cancelled,
    is_occurrence_cancelled,
    is_holiday,
    ends_at,
  ) = meeting.with_value(|meeting| {
    (
      meeting.id,
      meeting.conversation_id,
      meeting.starts_at.0,
      meeting.organizer_user_id == me,
      is_hosted_by(meeting, me),
      meeting.status == teamcraft_proto::MeetingStatus::Cancelled,
      meeting.is_occurrence_cancelled,
      meeting.is_occurrence_holiday,
      meeting.ends_at.0,
    )
  });
  let is_over_now = move || meeting.with_value(|meeting| is_over(meeting, now.get()));
  // A cancellation can be taken back while what it cancelled is still to come.
  let can_restore = move || is_host && is_cancelled && ends_at > now.get();
  let (restoring, hiding) = (PendingAction::new(), PendingAction::new());
  let restore = move |_| {
    restoring.spawn(async move {
      let maybe_occurrence = is_occurrence_cancelled.then_some(teamcraft_proto::UnixMillis(starts_at));
      hub
        .client()
        .restore(id, maybe_occurrence)
        .await
        .map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      Ok(())
    });
  };
  let hide = move |_| {
    hiding.spawn(async move {
      hub.client().hide(id).await.map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      Ok(())
    });
  };
  let invite_guests = move |_| {
    shows_guest_links.set(true);
    open_rows.update(|open| {
      open.insert(row);
    });
  };
  let copy_link = move |_| {
    let link = meeting.with_value(|meeting| meeting.join_url.clone());
    copy_meeting_link(state, hub, i18n, id, conversation, link);
  };
  focus_first_item(is_open, menu);

  view! {
    <div class="menu-anchor">
      <button
        type="button"
        class="icon-btn"
        node_ref=trigger
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || i18n.t("meetings-more")
        data-tip=move || i18n.t("meetings-more")
        on:click=move |_| is_open.update(|open| *open = !*open)
      >
        <Icon name=IconName::More />
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <PopoverBackdrop on_close=Callback::new(move |()| is_open.set(false)) popover=menu />
        // A chosen item closes the menu (its click bubbles up here).
        <div
          class="popover menu-popover"
          role="menu"
          node_ref=menu
          aria-label=move || i18n.t("meetings-more")
          on:keydown=move |ev| menu_keydown(ev, menu, is_open, trigger)
          on:click=move |_| is_open.set(false)
        >
          <button type="button" class="menu-item" role="menuitem" on:click=move |_| open_rows.update(|open| {
            if !open.remove(&row) {
              open.insert(row);
            }
          })>
            <Icon name=IconName::Info />
            <span class="menu-label">{move || i18n.t("meetings-details")}</span>
          </button>
          {move || (!is_over_now()).then(|| view! {
            <button type="button" class="menu-item" role="menuitem" on:click=copy_link>
              <Icon name=IconName::Copy />
              <span class="menu-label">{move || i18n.t("meetings-copy-link")}</span>
            </button>
          })}
          {move || (!is_over_now() && is_hosting).then(|| view! {
            <button type="button" class="menu-item" role="menuitem" on:click=invite_guests>
              <Icon name=IconName::UserPlus />
              <span class="menu-label">{move || i18n.t("meetings-invite-guests")}</span>
            </button>
          })}
          {move || (!is_over_now() && is_host).then(|| view! {
            <button type="button" class="menu-item" role="menuitem" on:click=move |_| state.overlay.set(Overlay::EditMeeting(id))>
              <Icon name=IconName::Compose />
              <span class="menu-label">{move || i18n.t("meetings-edit")}</span>
            </button>
            <div class="menu-sep" role="separator"></div>
            <button type="button" class="menu-item is-danger" role="menuitem" on:click=move |_| state.overlay.set(Overlay::CancelMeeting(id, starts_at))>
              <Icon name=IconName::Close />
              <span class="menu-label">{move || i18n.t("meetings-cancel")}</span>
            </button>
          })}
          {move || can_restore().then(|| view! {
            <button type="button" class="menu-item" role="menuitem" prop:disabled=move || restoring.is_pending() on:click=restore>
              <Icon name=IconName::Undo />
              <span class="menu-label">{move || i18n.t(if is_holiday { "meetings-restore-holiday" } else { "meetings-restore" })}</span>
            </button>
          })}
          {(is_host && is_cancelled && !is_occurrence_cancelled).then(|| view! {
            <button type="button" class="menu-item is-danger" role="menuitem" prop:disabled=move || hiding.is_pending() on:click=hide>
              <Icon name=IconName::Trash />
              <span class="menu-label">{move || i18n.t("meetings-remove")}</span>
            </button>
          })}
        </div>
      </Show>
    </div>
  }
}

/// What a row shows when opened: when, the series, the description, the link, the responses,
/// and for hosts the guest links and, while it runs, the host controls.
#[component]
fn MeetingDetail(
  meeting: StoredValue<MeetingResponse>,
  now: RwSignal<TimestampMs>,
  shows_guest_links: RwSignal<bool>,
  detail_id: String,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let me = state.me().id;
  let locale = i18n.locale().tag();
  let meeting = meeting.get_value();
  let id = meeting.id;
  let conversation = meeting.conversation_id;
  let is_hosting = is_hosted_by(&meeting, me);
  let is_over_now = is_over(&meeting, now.get_untracked());
  let is_live_host = is_hosting && is_joinable(&meeting, now.get_untracked());
  let when = format!(
    "{}, {}–{}",
    format_date(meeting.starts_at.0, locale),
    format_time(meeting.starts_at.0, locale),
    format_time(meeting.ends_at.0, locale)
  );
  let maybe_series = series_summary(&meeting);
  let maybe_description = meeting
    .maybe_description
    .clone()
    .filter(|text| !text.trim().is_empty());
  let responses = meeting.attendees.len();
  let attendees = meeting.attendees.clone();
  let join_url = meeting.join_url.clone();

  view! {
    <div class="meeting-detail" id=detail_id>
      <dl class="meeting-facts">
        <dt>{move || i18n.t("meetings-when")}</dt>
        <dd>{when}</dd>
        {maybe_series.map(|series| {
          let (key, count) = (series.key(), series.interval as usize);
          view! {
            <dt>{move || i18n.t("meetings-recurring")}</dt>
            <dd>{move || i18n.t_count(key, count)}</dd>
          }
        })}
      </dl>
      {maybe_description.map(|text| view! { <p class="meeting-description" translate="no">{text}</p> })}
      {(!is_over_now).then(|| view! {
        <div class="meeting-detail-actions">
          <MeetingLinkButton meeting=id conversation=conversation join_url=join_url.clone() />
          {is_hosting.then(|| view! {
            <button
              type="button"
              class="btn"
              aria-expanded=move || shows_guest_links.get().to_string()
              on:click=move |_| shows_guest_links.update(|open| *open = !*open)
            >
              <Icon name=IconName::UserPlus />
              <span>{move || i18n.t("meetings-invite-guests")}</span>
            </button>
          })}
        </div>
      })}
      <Show when=move || shows_guest_links.get() && is_hosting && !is_over_now fallback=|| ()>
        <GuestLinks meeting=id conversation=conversation />
      </Show>
      {is_live_host.then(|| view! { <HostControls meeting=id conversation=conversation /> })}
      <h3 class="meeting-detail-heading">{move || i18n.t_with("meetings-responses", &[("count", &responses.to_string())])}</h3>
      <RsvpList attendees=attendees />
    </div>
  }
}

/// Opens the meeting's pre-join screen. The organizer's device founds the chat's encryption group
/// first where a meeting from before 1.0.3 has none (nothing happens when the group is there).
fn open_meeting(state: crate::state::AppState, i18n: I18n, meeting: &MeetingResponse, is_host: bool) {
  if is_host {
    // The server's attendees: only users the organizer's signed list names go in (#1726); anybody
    // else knocks at the lobby.
    let invitees = crate::meetings::ChatInvitees::listed(attending_users(meeting));
    state.try_write(|backend| backend.set_up_meeting_chat(meeting.conversation_id, invitees));
  }
  join_meeting(state, i18n, meeting.conversation_id);
}

/// "Copy link" from a row's menu: the link with the organizer's fingerprint as this device pinned
/// it (e2ee.md §4.6), never the server's; while that is being made it says so and asks again.
fn copy_meeting_link(
  state: crate::state::AppState,
  hub: MeetingsHub,
  i18n: I18n,
  meeting: MeetingId,
  conversation: crate::model::ConversationId,
  join_url: String,
) {
  use crate::components::meetings_view::verify_link;
  let Some(fragment) = hub
    .verified_links
    .with_untracked(|links| links.get(&meeting).cloned())
  else {
    hub.verifying.update_value(|verifying| {
      verifying.remove(&meeting);
    });
    verify_link(state, hub, conversation, meeting);
    state.show_brief_toast(i18n.t("meetings-verifying"));
    return;
  };
  let copying =
    crate::components::message_actions::copy_text(&crate::meetings::with_verifier(&join_url, &fragment));
  let (copied, failed) = (i18n.t("meetings-link-copied-toast"), i18n.t("action-copy-failed"));
  spawn_local(async move {
    if copying.await {
      state.show_brief_toast(copied);
    } else {
      state.show_error(failed, String::new());
    }
  });
}

/// "Organized by Anna · 5 people", or "You host · 5 people".
fn organizer_line(
  state: crate::state::AppState,
  meeting: &MeetingResponse,
  me: crate::model::UserId,
  i18n: I18n,
) -> String {
  let host = if meeting.organizer_user_id == me {
    i18n.t("meetings-you-host")
  } else {
    let name = state
      .read(|backend| backend.user(meeting.organizer_user_id))
      .map(|user| user.display_name)
      .unwrap_or_default();
    i18n.t_with("meetings-organizer", &[("name", &name)])
  };
  match meeting.attendees.len() {
    0 => host,
    count => format!("{host} · {}", i18n.t_count("meetings-people", count)),
  }
}

/// "45 min", "1 h", "1 h 30 min".
fn duration_label(minutes: i64, i18n: I18n) -> String {
  let (hours, rest) = (minutes / 60, minutes % 60);
  match (hours, rest) {
    (0, minutes) => i18n.t_with("meetings-duration-minutes", &[("minutes", &minutes.to_string())]),
    (hours, 0) => i18n.t_with("meetings-duration-hours", &[("hours", &hours.to_string())]),
    (hours, minutes) => i18n.t_with(
      "meetings-duration-both",
      &[("hours", &hours.to_string()), ("minutes", &minutes.to_string())],
    ),
  }
}

/// "Today · Wednesday, November 12", "Friday, November 14".
fn day_heading(day: i64, first_start: TimestampMs, now: TimestampMs, i18n: I18n) -> String {
  let date = format_date(first_start, i18n.locale().tag());
  let relative = match day_label(day, local_day(now)) {
    DayLabel::Yesterday => "meetings-yesterday",
    DayLabel::Today => "meetings-today",
    DayLabel::Tomorrow => "meetings-tomorrow",
    DayLabel::Weekday | DayLabel::Date => return date,
  };
  format!("{} · {date}", i18n.t(relative))
}

/// A row's key for the lists: it changes whenever anything shown on the row changes.
fn row_key(meeting: &MeetingResponse) -> (MeetingId, TimestampMs, u64) {
  let mut hasher = DefaultHasher::new();
  serde_json::to_string(meeting)
    .unwrap_or_default()
    .hash(&mut hasher);
  (meeting.id, meeting.starts_at.0, hasher.finish())
}

fn scroll_to_row(row: RowId) {
  let selector = format!("[data-meeting-row=\"{}-{}\"]", row.0, row.1);
  // After the detail rendered, so the row scrolls into view with it.
  request_animation_frame(move || {
    if let Some(element) = document().query_selector(&selector).ok().flatten() {
      let options = web_sys::ScrollIntoViewOptions::new();
      options.set_block(web_sys::ScrollLogicalPosition::Nearest);
      if !crate::motion::prefers_reduced_motion() {
        options.set_behavior(web_sys::ScrollBehavior::Smooth);
      }
      element.scroll_into_view_with_scroll_into_view_options(&options);
    }
  });
}

/// The first item takes the focus when a menu opens, so the keyboard goes on from there.
fn focus_first_item(is_open: RwSignal<bool>, menu: NodeRef<html::Div>) {
  use wasm_bindgen::JsCast;
  Effect::new(move |_| {
    if is_open.get()
      && let Some(first) = menu
        .get()
        .and_then(|menu| menu.query_selector(".menu-item").ok().flatten())
      && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
    {
      drop(first.focus());
    }
  });
}

fn menu_keydown(
  ev: web_sys::KeyboardEvent,
  menu: NodeRef<html::Div>,
  is_open: RwSignal<bool>,
  trigger: NodeRef<html::Button>,
) {
  match ev.key().as_str() {
    "Escape" => {
      ev.stop_propagation();
      ev.prevent_default();
      is_open.set(false);
      if let Some(trigger) = trigger.get_untracked() {
        drop(trigger.focus());
      }
    },
    "ArrowDown" | "ArrowUp" => {
      ev.prevent_default();
      move_focus(menu, ev.key() == "ArrowDown");
    },
    "Tab" => is_open.set_if_alive(false),
    _ => {},
  }
}
