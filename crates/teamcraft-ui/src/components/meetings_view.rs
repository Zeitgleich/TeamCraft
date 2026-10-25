//! The meetings screen: upcoming meetings, the schedule dialog, RSVP, guest links and the lobby of
//! meetings the user hosts. The server owns all state (docs/meetings.md); [`MeetingsHub`] mirrors it.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{IdempotencyKey, LobbyEntry, MeetingId, MeetingResponse, MeetingStatus, RsvpStatus};

use crate::calls::{CallKind, CallRequest};
use crate::clock::{format_date, format_local_datetime_input, format_time, local_day, now_ms};
use crate::components::compact_back::CompactBack;
use crate::components::icons::{Icon, IconName};
use crate::components::meeting_controls::{HostControls, RsvpList};
use crate::components::modal::Modal;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::{I18n, use_i18n};
use crate::meetings::{
  ExternalInvitee, FormError, MeetingsHub, Repeat, ScheduleForm, hosts_lobby, is_over, my_rsvp,
};
use crate::model::{ConversationId, UserId};
use crate::reactive::{SetIfAlive, use_interval_while_visible};
use crate::state::{AppState, Overlay, use_app_state};

/// How often the list and the lobbies are reloaded while the screen is open and visible (guests
/// waiting in a lobby show up without a reload; `meeting_update` events cover the rest).
const REFRESH_MS: u64 = 10_000;
/// A first meeting suggestion starts at the next full hour, at least this far ahead.
const MIN_LEAD_MS: i64 = 10 * 60_000;
const MS_PER_HOUR: i64 = 3_600_000;
const DURATIONS: [u32; 6] = [15, 30, 45, 60, 90, 120];

#[component]
pub fn MeetingsView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let reload = move || spawn_local(async move { hub.refresh(me, now_ms()).await });
  reload();
  use_interval_while_visible(REFRESH_MS, reload);

  let is_starting = RwSignal::new(false);
  let start_now = move |_| {
    if is_starting.get_untracked() {
      return;
    }
    is_starting.set(true);
    spawn_local(async move {
      let client = hub.client();
      let key = Some(IdempotencyKey::random());
      if let Some(meeting) = hub.run(me, now_ms(), client.start_instant(None, key)).await {
        join_meeting(state, i18n, meeting.conversation_id);
      }
      is_starting.set_if_alive(false);
    });
  };

  view! {
    <section class="meetings-view" aria-label=move || i18n.t("meetings-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("meetings-title")}</h1>
        </div>
        <button class="btn" prop:disabled=move || is_starting.get() on:click=start_now>
          <Icon name=IconName::Video />
          <span>{move || i18n.t("meetings-start-now")}</span>
        </button>
        <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::ScheduleMeeting)>
          <Icon name=IconName::Plus />
          <span>{move || i18n.t("meetings-schedule")}</span>
        </button>
      </header>
      {move || {
        hub.maybe_error.get().map(|message| {
          view! { <div class="auth-alert meetings-error" role="alert">{move || i18n.t_with("meetings-error", &[("detail", &message)])}</div> }
        })
      }}
      <div class="meetings-list panel-scroll">
        <For
          each=move || meeting_days(&hub.meetings.get(), now_ms())
          key=|day| (day.day, day.meetings.iter().map(|meeting| (meeting.id, meeting.status.clone(), meeting.attendees.len())).collect::<Vec<_>>())
          let:day
        >
          <h2 class="meetings-day">{move || day_label(day.day, day.first_start, i18n)}</h2>
          <ul class="meetings-day-list">
            {day.meetings.into_iter().map(|meeting| view! { <MeetingCard meeting=meeting /> }).collect_view()}
          </ul>
        </For>
        <Show when=move || hub.is_loaded.get() && hub.meetings.get().is_empty() fallback=|| ()>
          <p class="empty-state">{move || i18n.t("meetings-empty")}</p>
        </Show>
      </div>
    </section>
  }
}

/// Meetings of one local day, for the day headings.
#[derive(Clone, Debug, PartialEq)]
struct MeetingDay {
  day: i64,
  first_start: i64,
  meetings: Vec<MeetingResponse>,
}

fn meeting_days(meetings: &[MeetingResponse], _now: i64) -> Vec<MeetingDay> {
  let mut days: Vec<MeetingDay> = Vec::new();
  for meeting in meetings {
    let day = local_day(meeting.starts_at.0);
    match days.last_mut().filter(|last| last.day == day) {
      Some(last) => last.meetings.push(meeting.clone()),
      None => days.push(MeetingDay {
        day,
        first_start: meeting.starts_at.0,
        meetings: vec![meeting.clone()],
      }),
    }
  }
  days
}

fn day_label(day: i64, first_start: i64, i18n: I18n) -> String {
  let today = local_day(now_ms());
  match day - today {
    0 => i18n.t("meetings-today"),
    1 => i18n.t("meetings-tomorrow"),
    _ => format_date(first_start, i18n.locale().tag()),
  }
}

#[component]
fn MeetingCard(meeting: MeetingResponse) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let locale = i18n.locale().tag();
  let id = meeting.id;
  let is_host = meeting.organizer_user_id == me;
  let is_cancelled = meeting.status == MeetingStatus::Cancelled;
  let over = is_over(&meeting, now_ms());
  let rsvp = my_rsvp(&meeting, me);
  let time_range = format!(
    "{} – {}",
    format_time(meeting.starts_at.0, locale),
    format_time(meeting.ends_at.0, locale)
  );
  let organizer = state
    .read(|backend| backend.user(meeting.organizer_user_id))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let attendees = attendee_summary(&meeting, state.read(|backend| backend.users()));
  let guest_link = RwSignal::new(None::<String>);
  let show_responses = RwSignal::new(false);
  let response_total = meeting.attendees.len();
  let attendees_for_list = meeting.attendees.clone();
  let conversation = meeting.conversation_id;
  // Host controls matter while the meeting runs (a little before the start counts, people dial in early).
  let is_live_host = is_host && !over && meeting.starts_at.0 - 15 * 60_000 <= now_ms();
  let meeting_for_join = meeting.clone();
  let has_recurrence = meeting.maybe_recurrence.is_some();
  let has_lobby = hosts_lobby(&meeting, me);

  let make_guest_link = move |_| {
    spawn_local(async move {
      match hub.client().guest_link(id, None).await {
        Ok(link) => guest_link.set_if_alive(Some(link.join_url)),
        Err(error) => hub.maybe_error.set_if_alive(Some(error.to_string())),
      }
    });
  };
  let cancel = move |_| {
    spawn_local(async move {
      hub.run(me, now_ms(), hub.client().cancel(id)).await;
    })
  };
  let answer = move |status: RsvpStatus| {
    spawn_local(async move {
      hub.run(me, now_ms(), hub.client().set_rsvp(id, status)).await;
    });
  };

  view! {
    <li class="meeting-card" class:meeting-over=over>
      <div class="meeting-time">
        <Icon name=IconName::Clock />
        <span>{time_range}</span>
        {has_recurrence.then(|| view! { <span class="badge">{move || i18n.t("meetings-recurring")}</span> })}
        {is_cancelled.then(|| view! { <span class="badge badge-guest">{move || i18n.t("meetings-cancelled")}</span> })}
      </div>
      <h3 class="meeting-title" translate="no">{meeting.title.clone()}</h3>
      <p class="meeting-meta muted-text" translate="no">
        {move || i18n.t_with("meetings-organizer", &[("name", &organizer)])}
        {" · "}
        {attendees.clone()}
      </p>
      <div class="meeting-actions">
        {(!over)
          .then(|| {
            let meeting = meeting_for_join.clone();
            view! {
              <button class="btn btn-primary" on:click=move |_| join_meeting(state, i18n, meeting.conversation_id)>
                {move || i18n.t("meetings-join")}
              </button>
            }
          })}
        {(!over && !is_host && rsvp.is_some())
          .then(|| {
            view! {
              <div class="segmented" role="group" aria-label=move || i18n.t("meetings-rsvp")>
                <SlidingIndicator />
                {[RsvpStatus::Accepted, RsvpStatus::Tentative, RsvpStatus::Declined]
                  .into_iter()
                  .map(|option| {
                    let key = format!("meetings-rsvp-{}", option.as_str());
                    let pressed = rsvp.as_ref() == Some(&option);
                    let choice = option.clone();
                    view! {
                      <button aria-pressed=pressed.to_string() on:click=move |_| answer(choice.clone())>
                        {move || i18n.t(&key)}
                      </button>
                    }
                  })
                  .collect_view()}
              </div>
            }
          })}
        {(!over && is_host)
          .then(|| {
            view! {
              <button class="btn" on:click=make_guest_link>{move || i18n.t("meetings-guest-link")}</button>
              <button class="btn btn-quiet" on:click=cancel>{move || i18n.t("meetings-cancel")}</button>
            }
          })}
      </div>
      {move || {
        guest_link.get().map(|link| {
          view! {
            <div class="meeting-link">
              <label class="muted-text" for=format!("guest-link-{id}")>{move || i18n.t("meetings-guest-link-hint")}</label>
              <input id=format!("guest-link-{id}") class="input mono" readonly prop:value=link />
            </div>
          }
        })
      }}
      <button class="btn btn-quiet btn-small" aria-expanded=move || show_responses.get().to_string() on:click=move |_| show_responses.update(|open| *open = !*open)>
        {move || i18n.t_with("meetings-responses", &[("count", &response_total.to_string())])}
      </button>
      <Show when=move || show_responses.get() fallback=|| ()>
        <RsvpList attendees=attendees_for_list.clone() />
      </Show>
      {is_live_host.then(|| view! { <HostControls meeting=id conversation=conversation /> })}
      {has_lobby.then(|| view! { <Lobby meeting=id /> })}
    </li>
  }
}

/// Guests waiting for admission into a meeting the user hosts.
#[component]
fn Lobby(meeting: MeetingId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let entries = Memo::new(move |_| {
    hub
      .lobbies
      .get()
      .into_iter()
      .find(|(id, _)| *id == meeting)
      .map(|(_, entries)| entries)
      .unwrap_or_default()
  });
  let decide = move |entry: &LobbyEntry, admit: bool| {
    let guest = entry.user.id;
    spawn_local(async move {
      let client = hub.client();
      let outcome = async move {
        if admit {
          client.admit(meeting, guest).await
        } else {
          client.deny(meeting, guest).await
        }
      };
      hub.run(me, now_ms(), outcome).await;
    });
  };
  view! {
    <Show when=move || !entries.get().is_empty() fallback=|| ()>
      <div class="lobby" role="region" aria-label=move || i18n.t("meetings-lobby")>
        <strong>{move || i18n.t("meetings-lobby")}</strong>
        <ul>
          {move || {
            entries
              .get()
              .into_iter()
              .map(|entry| {
                let name = entry.user.display_name.clone();
                let (admit_entry, deny_entry) = (entry.clone(), entry);
                view! {
                  <li class="lobby-entry">
                    <span translate="no">{name}</span>
                    <button class="btn btn-primary" on:click=move |_| decide(&admit_entry, true)>
                      {move || i18n.t("meetings-admit")}
                    </button>
                    <button class="btn" on:click=move |_| decide(&deny_entry, false)>
                      {move || i18n.t("meetings-deny")}
                    </button>
                  </li>
                }
              })
              .collect_view()
          }}
        </ul>
      </div>
    </Show>
  }
}

/// Members join through the meeting's chat conversation and its call (also from a reminder).
pub fn join_meeting(state: AppState, i18n: I18n, conversation: ConversationId) {
  state.open_conversation(conversation);
  if !state.read(|backend| backend.has_calls()) {
    state.show_toast(i18n.t("calls-unavailable"));
    return;
  }
  state.call.prepare(
    CallRequest {
      conversation,
      kind: CallKind::Group,
    },
    true,
  );
}

fn attendee_summary(meeting: &MeetingResponse, users: Vec<crate::model::User>) -> String {
  let names: Vec<String> = meeting
    .attendees
    .iter()
    .map(|attendee| {
      attendee
        .maybe_user_id
        .and_then(|id| {
          users
            .iter()
            .find(|user| user.id == id)
            .map(|user| user.display_name.clone())
        })
        .or_else(|| attendee.maybe_name.clone())
        .or_else(|| attendee.maybe_email.clone())
        .unwrap_or_default()
    })
    .filter(|name| !name.is_empty())
    .collect();
  names.join(", ")
}

// ---- The schedule dialog ----------------------------------------------------------------------------

#[component]
pub fn ScheduleMeetingDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let now = now_ms();
  let form = RwSignal::new(ScheduleForm::new(format_local_datetime_input(suggested_start(
    now,
  ))));
  let maybe_error = RwSignal::new(None::<FormError>);
  let is_saving = RwSignal::new(false);
  let others = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .users()
        .into_iter()
        .filter(|user| user.id != me && !user.is_bot)
        .collect::<Vec<_>>()
    })
  });
  let external_email = RwSignal::new(String::new());
  let external_name = RwSignal::new(String::new());

  let add_external = move |_| {
    let email = external_email.get_untracked().trim().to_owned();
    if email.is_empty() {
      return;
    }
    let name = external_name.get_untracked();
    form.update(|form| form.externals.push(ExternalInvitee { email, name }));
    external_email.set(String::new());
    external_name.set(String::new());
  };
  let submit = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if is_saving.get_untracked() {
      return;
    }
    let now = now_ms();
    match form.get_untracked().to_request(now) {
      Err(error) => maybe_error.set(Some(error)),
      Ok(request) => {
        maybe_error.set(None);
        is_saving.set(true);
        spawn_local(async move {
          let client = hub.client();
          if hub.run(me, now_ms(), client.create(&request)).await.is_some() {
            state.close_overlay();
          }
          is_saving.set_if_alive(false);
        });
      },
    }
  };

  view! {
    <Modal labelled_by="schedule-title" class="modal-wide">
      <form on:submit=submit>
        <header class="modal-head">
          <h2 id="schedule-title">{move || i18n.t("schedule-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body schedule-form">
          <div class="field">
            <label for="meeting-title">{move || i18n.t("schedule-name")}</label>
            <input
              id="meeting-title"
              class="input"
              data-autofocus
              prop:value=move || form.get().title
              on:input=move |ev| form.update(|form| form.title = event_target_value(&ev))
            />
          </div>
          <div class="field-row">
            <div class="field grow">
              <label for="meeting-start">{move || i18n.t("schedule-start")}</label>
              <input
                id="meeting-start"
                class="input"
                type="datetime-local"
                prop:value=move || form.get().starts_at
                on:input=move |ev| form.update(|form| form.starts_at = event_target_value(&ev))
              />
            </div>
            <div class="field">
              <label for="meeting-duration">{move || i18n.t("schedule-duration")}</label>
              <select
                id="meeting-duration"
                class="input"
                on:change=move |ev| {
                  if let Ok(minutes) = event_target_value(&ev).parse::<u32>() {
                    form.update(|form| form.duration_minutes = minutes);
                  }
                }
              >
                {DURATIONS
                  .into_iter()
                  .map(|minutes| {
                    view! {
                      <option value=minutes.to_string() selected=move || form.get().duration_minutes == minutes>
                        {move || i18n.t_with("schedule-minutes", &[("count", &minutes.to_string())])}
                      </option>
                    }
                  })
                  .collect_view()}
              </select>
            </div>
          </div>
          <div class="field">
            <label for="meeting-description">{move || i18n.t("schedule-description")}</label>
            <textarea
              id="meeting-description"
              class="input"
              rows="2"
              prop:value=move || form.get().description
              on:input=move |ev| form.update(|form| form.description = event_target_value(&ev))
            ></textarea>
          </div>
          <fieldset class="field">
            <legend>{move || i18n.t("schedule-invite-members")}</legend>
            <ul class="invite-list">
              <For each=move || others.get() key=|user| user.id let:user>
                {
                  let id: UserId = user.id;
                  view! {
                    <li>
                      <label class="checkbox">
                        <input
                          type="checkbox"
                          prop:checked=move || form.get().members.contains(&id)
                          on:change=move |ev| {
                            let checked = event_target_checked(&ev);
                            form.update(|form| {
                              form.members.retain(|member| *member != id);
                              if checked {
                                form.members.push(id);
                              }
                            });
                          }
                        />
                        <span translate="no">{user.display_name.clone()}</span>
                      </label>
                    </li>
                  }
                }
              </For>
            </ul>
          </fieldset>
          <fieldset class="field">
            <legend>{move || i18n.t("schedule-invite-external")}</legend>
            <ul class="invite-list">
              {move || {
                form
                  .get()
                  .externals
                  .into_iter()
                  .enumerate()
                  .map(|(index, external)| {
                    view! {
                      <li class="invite-external">
                        <span translate="no">{format!("{} <{}>", external.name, external.email)}</span>
                        <button
                          type="button"
                          class="icon-btn"
                          aria-label=move || i18n.t("schedule-remove")
                          on:click=move |_| form.update(|form| drop(form.externals.remove(index)))
                        >
                          <Icon name=IconName::Close />
                        </button>
                      </li>
                    }
                  })
                  .collect_view()
              }}
            </ul>
            <div class="field-row">
              <input
                id="external-email"
                class="input grow"
                type="email"
                placeholder=move || i18n.t("schedule-external-email")
                prop:value=move || external_email.get()
                on:input=move |ev| external_email.set(event_target_value(&ev))
              />
              <input
                id="external-name"
                class="input grow"
                placeholder=move || i18n.t("schedule-external-name")
                prop:value=move || external_name.get()
                on:input=move |ev| external_name.set(event_target_value(&ev))
              />
              <button type="button" class="btn" on:click=add_external>{move || i18n.t("schedule-add")}</button>
            </div>
          </fieldset>
          <div class="field-row">
            <div class="field grow">
              <label for="meeting-repeat">{move || i18n.t("schedule-repeat")}</label>
              <select
                id="meeting-repeat"
                class="input"
                on:change=move |ev| {
                  if let Some(repeat) = Repeat::from_slug(&event_target_value(&ev)) {
                    form.update(|form| form.repeat = repeat);
                  }
                }
              >
                {Repeat::ALL
                  .into_iter()
                  .map(|repeat| {
                    let key = format!("schedule-repeat-{}", repeat.slug());
                    view! {
                      <option value=repeat.slug() selected=move || form.get().repeat == repeat>
                        {move || i18n.t(&key)}
                      </option>
                    }
                  })
                  .collect_view()}
              </select>
            </div>
            <Show when=move || form.get().repeat != Repeat::Never fallback=|| ()>
              <div class="field">
                <label for="meeting-interval">{move || i18n.t("schedule-interval")}</label>
                <input
                  id="meeting-interval"
                  class="input"
                  type="number"
                  min="1"
                  prop:value=move || form.get().interval.to_string()
                  on:input=move |ev| {
                    if let Ok(interval) = event_target_value(&ev).parse::<u32>() {
                      form.update(|form| form.interval = interval);
                    }
                  }
                />
              </div>
              <div class="field">
                <label for="meeting-count">{move || i18n.t("schedule-count")}</label>
                <input
                  id="meeting-count"
                  class="input"
                  type="number"
                  min="1"
                  placeholder=move || i18n.t("schedule-count-open")
                  prop:value=move || form.get().maybe_count.map(|count| count.to_string()).unwrap_or_default()
                  on:input=move |ev| form.update(|form| form.maybe_count = event_target_value(&ev).parse::<u32>().ok())
                />
              </div>
            </Show>
          </div>
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=move || form.get().has_lobby
              on:change=move |ev| form.update(|form| form.has_lobby = event_target_checked(&ev))
            />
            <span>{move || i18n.t("schedule-lobby")}</span>
          </label>
          <p class="muted-text small">{move || i18n.t("schedule-lobby-hint")}</p>
          {move || {
            maybe_error.get().map(|error| {
              let detail = match &error {
                FormError::InvalidEmail(email) => email.clone(),
                _ => String::new(),
              };
              view! { <div class="auth-alert" role="alert">{i18n.t_with(error.key(), &[("email", &detail)])}</div> }
            })
          }}
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || is_saving.get()>
            {move || i18n.t("schedule-submit")}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// The next full hour that is at least [`MIN_LEAD_MS`] away.
fn suggested_start(now: i64) -> i64 {
  let earliest = now + MIN_LEAD_MS;
  earliest.div_euclid(MS_PER_HOUR) * MS_PER_HOUR + MS_PER_HOUR
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn the_suggested_start_is_a_full_hour_with_some_lead() {
    let now = 1_792_540_800_000 + 25 * 60_000;
    let start = suggested_start(now);
    assert_eq!(start % MS_PER_HOUR, 0);
    assert!(start - now >= MIN_LEAD_MS);
    assert!(start - now < MIN_LEAD_MS + MS_PER_HOUR);
  }

  #[test]
  fn meetings_are_grouped_by_local_day() {
    assert!(meeting_days(&[], 0).is_empty());
    let day = 1_792_540_800_000;
    let meetings = [
      fixture(1, day + MS_PER_HOUR),
      fixture(2, day + 3 * MS_PER_HOUR),
      fixture(3, day + 30 * MS_PER_HOUR),
    ];
    let days = meeting_days(&meetings, day);
    assert_eq!(days.len(), 2);
    assert_eq!(days[0].meetings.len(), 2);
    assert_eq!(days[1].meetings[0].id, meetings[2].id);
  }

  fn fixture(id: u64, starts_at: i64) -> MeetingResponse {
    MeetingResponse {
      id: id_from_raw(id),
      title: format!("Meeting {id}"),
      maybe_description: None,
      starts_at: teamcraft_proto::UnixMillis(starts_at),
      ends_at: teamcraft_proto::UnixMillis(starts_at + MS_PER_HOUR),
      kind: teamcraft_proto::MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: id_from_raw(1),
      conversation_id: id_from_raw(9),
      join_url: String::new(),
      maybe_recurrence: None,
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }
}
