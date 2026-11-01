//! The meetings screen: upcoming meetings, the schedule dialog, RSVP, the meeting's links and the
//! lobby of meetings the user hosts, and opening a meeting from its link. The server owns all state
//! (docs/meetings.md); [`MeetingsHub`] mirrors it.

use std::collections::HashSet;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_crypto::guest_link::code_of_fingerprint;
use teamcraft_proto::{
  CancelScope, IdempotencyKey, LobbyEntry, LobbyState, MeetingId, MeetingResponse, MeetingStatus, RsvpStatus,
  UnixMillis, UpdateMeetingRequest,
};

use crate::calls::{CallKind, CallRequest};
use crate::channel_access::is_guest;
use crate::clock::{format_date, format_local_datetime_input, format_time, local_day, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::{EmptyAction, EmptyState};
use crate::components::icons::{Icon, IconName};
use crate::components::meeting_controls::{HostControls, RsvpList};
use crate::components::message_actions::copy_text;
use crate::components::modal::Modal;
use crate::components::pending_button::PendingButton;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::feedback::PendingAction;
use crate::form_errors::{error_id, focus_first_invalid};
use crate::i18n::{I18n, use_i18n};
use crate::meetings::{
  ExternalInvitee, FormError, FormField, MeetingError, MeetingsHub, Repeat, STATUS_CONFLICT,
  STATUS_NOT_FOUND, ScheduleForm, SeriesEnd, WEEKDAYS, attending_users, hosts_lobby, is_over, my_rsvp,
  with_verifier,
};
use crate::model::{ConversationId, UserId};
use crate::reactive::{SetIfAlive, UpdateIfAlive, pause, use_interval_while_visible};
use crate::route::MeetingHandle;
use crate::state::{AppState, Overlay, ToastAction, use_app_state};

/// How often the list and the lobbies are reloaded while the screen is open and visible (guests
/// waiting in a lobby show up without a reload; `meeting_update` events cover the rest).
const REFRESH_MS: u64 = 10_000;
/// How often a member who knocked asks whether a host let them in, while they wait and the screen
/// is visible. The server also sends them `lobby_decision`, but the client core doesn't hand that
/// event to the UI yet, so asking is how the screen learns it.
const KNOCK_POLL_MS: u64 = 3_000;
/// A first meeting suggestion starts at the next full hour, at least this far ahead.
const MIN_LEAD_MS: i64 = 10 * 60_000;
const MS_PER_HOUR: i64 = 3_600_000;
/// Join is the main action from this long before a meeting starts.
const JOIN_LEAD_MS: i64 = 15 * 60_000;
/// Attendee pictures shown on a meeting; the names follow in text.
const MAX_FACES: usize = 4;
const DURATIONS: [u32; 6] = [15, 30, 45, 60, 90, 120];
/// The element of the schedule dialog whose first invalid field gets the focus.
const SCHEDULE_BODY_ID: &str = "schedule-form-body";

#[component]
pub fn MeetingsView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let reload = move || {
    spawn_local(async move {
      hub.refresh(me, now_ms()).await;
      crate::meetings::finish_pending_edits(state, hub);
    })
  };
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
      if let Some(meeting) = hub
        .run(me, now_ms(), client.start_instant(None, Vec::new(), key))
        .await
      {
        // The server makes the chat without encryption; an instant meeting invites nobody.
        // Gone only with the app; then the chat stays without encryption (nobody else founds it).
        state.try_write(|backend| backend.set_up_meeting_chat(meeting.conversation_id, Vec::new()));
        show_created(state, hub, meeting);
      }
      is_starting.set_if_alive(false);
    });
  };

  // A meeting link (`/meeting/{token}`, or `/meeting/{id}` from before #987) the app was opened
  // with, or that was followed later.
  let visit = RwSignal::new(LinkVisit::Idle);
  Effect::new(move |_| {
    if let Some(meeting) = state.maybe_meeting_link.get() {
      state.maybe_meeting_link.set(None);
      open_meeting_link(hub, visit, meeting);
    }
  });
  // Once the meeting's chat has loaded, the member is in: on to the pre-join screen.
  Effect::new(move |_| {
    if let LinkVisit::Entering(conversation) = visit.get()
      && state.read(|backend| backend.conversation(conversation)).is_some()
    {
      visit.set(LinkVisit::Idle);
      join_meeting(state, i18n, conversation);
    }
  });
  use_interval_while_visible(KNOCK_POLL_MS, move || {
    if let LinkVisit::Waiting(meeting) = visit.get_untracked() {
      spawn_local(async move { poll_knock(hub, visit, meeting).await });
    }
  });

  let can_create = !is_guest(state);
  view! {
    <section class="meetings-view page-view" aria-label=move || i18n.t("meetings-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("meetings-title")}</h1>
        </div>
        // Guests join meetings but create none (the server refuses them, docs/meetings.md).
        <Show when=move || can_create fallback=|| ()>
          <button
            class="btn"
            title=move || i18n.t("meetings-start-now-hint")
            prop:disabled=move || is_starting.get()
            on:click=start_now
          >
            <Icon name=IconName::Video />
            <span>{move || i18n.t("meetings-start-now")}</span>
          </button>
          <button class="btn btn-primary" on:click=move |_| state.overlay.set(Overlay::ScheduleMeeting)>
            <Icon name=IconName::Plus />
            <span>{move || i18n.t("meetings-schedule")}</span>
          </button>
        </Show>
      </header>
      {move || {
        hub.maybe_error.get().map(|message| {
          view! { <div class="auth-alert meetings-error" role="alert">{move || i18n.t_with("meetings-error", &[("detail", &message)])}</div> }
        })
      }}
      <LinkVisitBanner visit=visit />
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
          <EmptyState
            icon=IconName::Calendar
            title_key="meetings-empty-title"
            body_key="meetings-empty"
            maybe_action=can_create.then(|| EmptyAction {
              label_key: "meetings-schedule",
              on_click: Callback::new(move |()| state.overlay.set(Overlay::ScheduleMeeting)),
            })
          />
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
  let start_time = format_time(meeting.starts_at.0, locale);
  let end_time = format_time(meeting.ends_at.0, locale);
  let now = now_ms();
  let is_live = !over && meeting.starts_at.0 <= now;
  // Dialling in a little early is normal, so Join leads from a quarter of an hour before.
  let is_joinable = !over && meeting.starts_at.0 - JOIN_LEAD_MS <= now;
  let users = state.read(|backend| backend.users());
  let faces: Vec<crate::model::User> = meeting
    .attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user_id)
    .filter_map(|id| users.iter().find(|user| user.id == id).cloned())
    .take(MAX_FACES)
    .collect();
  let organizer = state
    .read(|backend| backend.user(meeting.organizer_user_id))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let attendees = attendee_summary(&meeting, state.read(|backend| backend.users()));
  let join_url = meeting.join_url.clone();
  // "Copy link" carries the organizer's fingerprint as this device has it pinned (e2ee.md §4.6),
  // never the server's; made once per meeting so the click can copy at once.
  if !over {
    verify_link(state, hub, meeting.conversation_id, meeting.id);
  }
  let shows_guest_links = RwSignal::new(false);
  let show_responses = RwSignal::new(false);
  let response_total = meeting.attendees.len();
  let attendees_for_list = meeting.attendees.clone();
  let conversation = meeting.conversation_id;
  // Host controls matter while the meeting runs (a little before the start counts, people dial in early).
  let is_live_host = is_host && is_joinable;
  let meeting_for_join = meeting.clone();
  let has_recurrence = meeting.maybe_recurrence.is_some();
  let has_lobby = hosts_lobby(&meeting, me);
  let starts_at = meeting.starts_at;
  let is_occurrence_cancelled = meeting.is_occurrence_cancelled;
  // A cancellation can be taken back while what it cancelled is still to come.
  let can_restore = is_host && is_cancelled && meeting.ends_at.0 > now;
  // The organizer starts the meeting itself, also early (issue #1006): its invitees are rung.
  let join_key = if is_host && !is_live {
    "meetings-start"
  } else {
    "meetings-join"
  };
  let (restoring, hiding) = (PendingAction::new(), PendingAction::new());

  let cancel = move |_| state.overlay.set(Overlay::CancelMeeting(id, starts_at.0));
  let restore = move |_| {
    restoring.spawn(async move {
      let maybe_occurrence = is_occurrence_cancelled.then_some(starts_at);
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
  let answer = move |status: RsvpStatus| {
    spawn_local(async move {
      hub.run(me, now_ms(), hub.client().set_rsvp(id, status)).await;
    });
  };

  view! {
    <li class="meeting-card" class:meeting-over=over class:meeting-live=is_live class:meeting-cancelled=is_cancelled>
      <div class="meeting-time">
        <span class="meeting-start">{start_time}</span>
        <span class="meeting-end">{end_time}</span>
      </div>
      <div class="meeting-main">
      <div class="meeting-head">
        <h3 class="meeting-title" translate="no">{meeting.title.clone()}</h3>
        {is_live.then(|| view! { <span class="badge badge-live">{move || i18n.t("meetings-live")}</span> })}
        {has_recurrence.then(|| view! { <span class="meeting-flag" title=move || i18n.t("meetings-recurring")><Icon name=IconName::Rotate /></span> })}
        {is_cancelled.then(|| view! { <span class="badge badge-cancelled">{move || i18n.t("meetings-cancelled")}</span> })}
      </div>
      <p class="meeting-meta" translate="no">
        {move || i18n.t_with("meetings-organizer", &[("name", &organizer)])}
      </p>
      <div class="meeting-people">
        <span class="avatar-stack">
          {faces.into_iter().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> }).collect_view()}
        </span>
        <span class="meeting-attendees" translate="no">{attendees.clone()}</span>
      </div>
      <div class="meeting-actions">
        {(!over)
          .then(|| {
            let meeting = meeting_for_join.clone();
            view! {
              <button class="btn" class:btn-primary=is_joinable on:click=move |_| {
                if is_host {
                  // A meeting from before 1.0.3 has a chat without encryption: the organizer's
                  // device founds its group the first time it opens the meeting (nothing happens
                  // when the group is there already).
                  let invitees = attending_users(&meeting);
                  state.try_write(|backend| backend.set_up_meeting_chat(meeting.conversation_id, invitees));
                }
                join_meeting(state, i18n, meeting.conversation_id)
              }>
                {move || i18n.t(join_key)}
              </button>
            }
          })}
        {(!over).then(|| view! { <MeetingLinkButton meeting=id conversation=conversation join_url=join_url.clone() /> })}
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
              <button
                class="btn"
                aria-expanded=move || shows_guest_links.get().to_string()
                on:click=move |_| shows_guest_links.update(|open| *open = !*open)
              >
                {move || i18n.t("meetings-invite-guests")}
              </button>
              <button class="btn" on:click=move |_| state.overlay.set(Overlay::EditMeeting(id))>
                {move || i18n.t("meetings-edit")}
              </button>
              <button class="btn btn-quiet" on:click=cancel>{move || i18n.t("meetings-cancel")}</button>
            }
          })}
        {can_restore
          .then(|| {
            view! {
              <PendingButton pending=restoring class="btn" on_click=restore>
                {move || i18n.t("meetings-restore")}
              </PendingButton>
            }
          })}
        {(is_host && is_cancelled && !is_occurrence_cancelled)
          .then(|| {
            view! {
              <PendingButton pending=hiding class="btn btn-quiet" on_click=hide>
                {move || i18n.t("meetings-remove")}
              </PendingButton>
            }
          })}
      </div>
      <Show when=move || shows_guest_links.get() fallback=|| ()>
        <GuestLinks meeting=id conversation=conversation />
      </Show>
      <button class="btn btn-quiet btn-small meeting-responses" aria-expanded=move || show_responses.get().to_string() on:click=move |_| show_responses.update(|open| *open = !*open)>
        {move || i18n.t_with("meetings-responses", &[("count", &response_total.to_string())])}
      </button>
      <Show when=move || show_responses.get() fallback=|| ()>
        <RsvpList attendees=attendees_for_list.clone() />
      </Show>
      {is_live_host.then(|| view! { <HostControls meeting=id conversation=conversation /> })}
      {has_lobby.then(|| view! { <Lobby meeting=id /> })}
      </div>
    </li>
  }
}

/// Guests waiting for admission into a meeting the user hosts: on the meeting's card and on the
/// call screen while the host is in the meeting's call.
#[component]
pub fn Lobby(meeting: MeetingId) -> impl IntoView {
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
    let maybe_chat = hub.meetings.with_untracked(|all| {
      all
        .iter()
        .find(|item| item.id == meeting)
        .map(|item| (item.conversation_id, item.ends_at.0))
    });
    let is_guest_user = entry.user.kind == teamcraft_proto::UserKind::Guest;
    let maybe_knock_fingerprint = entry.maybe_knock_fingerprint.clone();
    spawn_local(async move {
      let client = hub.client();
      let outcome = async move {
        if admit {
          client.admit(meeting, guest).await
        } else {
          client.deny(meeting, guest).await
        }
      };
      let is_done = hub.run(me, now_ms(), outcome).await.is_some();
      if admit
        && is_done
        && let Some((chat, ends_at)) = maybe_chat
      {
        // The host's own decision lets the admitted person's devices into the chat's encryption
        // (e2ee.md §4.6, "Meeting guests").
        let_in(
          state,
          chat,
          guest,
          is_guest_user,
          maybe_knock_fingerprint,
          ends_at,
        );
      }
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
                // A guest's device is let in once their page set it up and reported its key's
                // fingerprint; the host may compare the key code with the guest's screen first
                // (e2ee.md §4.6, "Meeting guests").
                let is_guest = entry.user.kind == teamcraft_proto::UserKind::Guest;
                let is_guest_unproven = is_guest && (!entry.has_device || entry.maybe_knock_fingerprint.is_none());
                let guest_id = entry.user.id;
                if is_guest && !is_guest_unproven {
                  prepare_key_code(state, hub, guest_id);
                }
                let (admit_entry, deny_entry) = (entry.clone(), entry);
                view! {
                  <li class="lobby-entry">
                    <span translate="no">{name}</span>
                    {is_guest_unproven.then(|| view! {
                      <span class="muted-text small">{move || i18n.t("meetings-guest-device-pending")}</span>
                    })}
                    {(is_guest && !is_guest_unproven).then(|| view! {
                      <span class="muted-text small" title=move || i18n.t("meetings-guest-code-hint")>
                        {move || i18n.t("meetings-guest-code")}
                        " "
                        <span class="mono" translate="no">
                          {move || hub.key_codes.with(|codes| codes.get(&guest_id).cloned()).unwrap_or_else(|| i18n.t("meetings-verifying"))}
                        </span>
                      </span>
                    })}
                    <button
                      class="btn btn-primary"
                      prop:disabled=is_guest_unproven
                      on:click=move |_| decide(&admit_entry, true)
                    >
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

// ---- The meeting's links ----------------------------------------------------------------------------

/// Copies `text` when clicked and then says "Copied"; a refused write shows an error.
#[component]
fn CopyButton(text: String, label_key: &'static str, #[prop(optional)] is_primary: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_copied = RwSignal::new(false);
  let copy = move |_| {
    let failed = i18n.t("action-copy-failed");
    // Started inside the click: browsers only allow clipboard writes from a user action.
    let copying = copy_text(&text);
    spawn_local(async move {
      if copying.await {
        is_copied.set_if_alive(true);
      } else {
        state.show_error(failed, String::new());
      }
    });
  };
  view! {
    <button type="button" class="btn" class:btn-primary=is_primary on:click=copy>
      <Icon name=IconName::Copy />
      <span aria-live="polite">{move || i18n.t(if is_copied.get() { "meetings-link-copied" } else { label_key })}</span>
    </button>
  }
}

/// A link in a read-only field (focus selects it, for copying by hand) with a copy button.
#[component]
fn LinkField(link: String, field_id: String, label_key: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  let select_all = |ev: leptos::ev::FocusEvent| event_target::<web_sys::HtmlInputElement>(&ev).select();
  view! {
    <div class="meeting-link-row">
      <label class="visually-hidden" for=field_id.clone()>{move || i18n.t(label_key)}</label>
      <input id=field_id class="input mono grow" readonly translate="no" prop:value=link.clone() on:focus=select_all />
      <CopyButton text=link label_key="meetings-copy-link" />
    </div>
  }
}

/// The organizer's extra links and code: a personal one-time link for one guest, making the
/// meeting's link anew (the old one stops working, invitees get the new one), and the organizer's
/// code, which guests see on their page and may compare. The meeting link itself, which guests use
/// too, is "Copy link". The one-time link stays in a field in case the browser refused the copy.
#[component]
fn GuestLinks(meeting: MeetingId, conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let maybe_one_time = RwSignal::new(None::<String>);
  let (renewing, making) = (PendingAction::new(), PendingAction::new());
  verify_link(state, hub, conversation, meeting);
  let organizer_code = move || {
    hub
      .verified_links
      .with(|links| links.get(&meeting).map(|fragment| fragment_code(fragment)))
  };

  let renew = move |_| {
    let renewed = i18n.t("meetings-link-renewed");
    renewing.spawn(async move {
      hub
        .client()
        .renew_link(meeting)
        .await
        .map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      if state.is_alive() {
        state.show_toast(renewed);
      }
      Ok(())
    });
  };
  let copy_one_time = move |_| {
    making.spawn(async move {
      let link = hub
        .client()
        .guest_link(meeting, None)
        .await
        .map_err(|error| error.to_failure())?;
      // Never with the server's fingerprint: without this device's own, no link (#987 review).
      let Some(fragment) = verified_fragment(state, hub, conversation, meeting).await else {
        return Err(ActionFailure::other(
          "this device could not verify the meeting link".to_owned(),
        ));
      };
      let join_url = with_verifier(&link.join_url, &fragment);
      copy_fetched(state, i18n, join_url.clone()).await;
      maybe_one_time.set_if_alive(Some(join_url));
      Ok(())
    });
  };

  view! {
    <div class="meeting-guest-links" role="group" aria-label=move || i18n.t("meetings-invite-guests")>
      <div class="meeting-actions">
        <PendingButton pending=making class="btn" on_click=copy_one_time>
          <Icon name=IconName::Copy />
          <span>{move || i18n.t("meetings-guest-link")}</span>
        </PendingButton>
        <PendingButton pending=renewing class="btn btn-quiet" on_click=renew>
          <Icon name=IconName::Rotate />
          <span>{move || i18n.t("meetings-new-link")}</span>
        </PendingButton>
      </div>
      <p class="muted-text small">{move || i18n.t("meetings-new-link-hint")}</p>
      {move || organizer_code().map(|code| view! {
        <p class="muted-text small">
          {move || i18n.t("meetings-organizer-code")}
          " "
          <span class="mono" translate="no">{code}</span>
        </p>
      })}
      {move || {
        maybe_one_time.get().map(|link| {
          view! {
            <div class="meeting-link">
              <LinkField link=link field_id=format!("one-time-link-{meeting}") label_key="meetings-one-time-link-label" />
              <p class="muted-text small">{move || i18n.t("meetings-guest-link-hint")}</p>
            </div>
          }
        })
      }}
    </div>
  }
}

/// "Copy link" of a meeting: the server's link with the organizer's fingerprint as this device has
/// it pinned. Until the core made that, the button waits ("Verifying…"): a link with the server's
/// fingerprint is never copied (#987 review). The text is made inside the click, which browsers
/// require for clipboard writes.
#[component]
fn MeetingLinkButton(meeting: MeetingId, conversation: ConversationId, join_url: String) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let is_copied = RwSignal::new(false);
  let maybe_fragment = move || hub.verified_links.with(|links| links.get(&meeting).cloned());
  let copy = move |_| {
    let Some(fragment) = maybe_fragment() else {
      // Still verifying, or it failed: try again.
      hub.verifying.update_value(|verifying| {
        verifying.remove(&meeting);
      });
      verify_link(state, hub, conversation, meeting);
      return;
    };
    let failed = i18n.t("action-copy-failed");
    let copying = copy_text(&with_verifier(&join_url, &fragment));
    spawn_local(async move {
      if copying.await {
        is_copied.set_if_alive(true);
      } else {
        state.show_error(failed, String::new());
      }
    });
  };
  view! {
    <button type="button" class="btn" aria-disabled=move || maybe_fragment().is_none().to_string() on:click=copy>
      <Icon name=IconName::Copy />
      <span aria-live="polite">
        {move || {
          let key = match (maybe_fragment().is_some(), is_copied.get()) {
            (false, _) => "meetings-verifying",
            (true, true) => "meetings-link-copied",
            (true, false) => "meetings-copy-link",
          };
          i18n.t(key)
        }}
      </span>
    </button>
  }
}

/// Lets an admitted person's devices into the meeting chat's encryption group from this device:
/// a guest's own device (organizer's devices only, with the key their page reported, until the end
/// of their access), or a member's.
fn let_in(
  state: AppState,
  chat: ConversationId,
  user: UserId,
  is_guest_user: bool,
  maybe_knock_fingerprint: Option<String>,
  ends_at_ms: i64,
) {
  state.try_write(|backend| {
    if is_guest_user {
      // Without a reported key there is nothing to admit; the lobby offers no "admit" then.
      if let Some(fingerprint) = maybe_knock_fingerprint {
        backend.let_guest_into_meeting_chat(chat, user, fingerprint, ends_at_ms + GUEST_ACCESS_GRACE_MS);
      }
    } else {
      backend.let_into_meeting_chat(chat, user);
    }
  });
}

/// Whether the organizer's device lets a lobby entry in by itself: only when the organizer chose
/// "no lobby" on this device (`is_open_by_own_record`; the server's `has_lobby` counts for nothing)
/// and the person came with the meeting link, with a device whose key they reported.
pub fn admits_by_itself(is_open_by_own_record: bool, entry: &LobbyEntry) -> bool {
  let is_guest = entry.user.kind == teamcraft_proto::UserKind::Guest;
  is_open_by_own_record
    && entry.is_by_link
    && entry.has_device
    && (!is_guest || entry.maybe_knock_fingerprint.is_some())
}

/// In a meeting the organizer opened without a lobby, the organizer's device lets in by itself
/// whoever knocked with the meeting link ([`admits_by_itself`]), and says so each time with a
/// "Remove" button (e2ee.md §4.6, "Meeting guests"). Runs on every reload of the lobbies (a
/// `lobby_join` event or a meeting change); `started` keeps one admission per person in flight.
pub fn admit_link_arrivals(
  state: AppState,
  i18n: I18n,
  hub: MeetingsHub,
  started: StoredValue<HashSet<(MeetingId, UserId)>, LocalStorage>,
) {
  let me = state.me().id;
  for (meeting, entries) in hub.lobbies.get_untracked() {
    let Some((chat, ends_at)) = hub.meetings.with_untracked(|all| {
      all
        .iter()
        .find(|item| item.id == meeting && item.organizer_user_id == me)
        .map(|item| (item.conversation_id, item.ends_at.0))
    }) else {
      continue;
    };
    let is_open = state.read(|backend| backend.is_open_by_own_record(meeting));
    for entry in entries
      .into_iter()
      .filter(|entry| admits_by_itself(is_open, entry))
    {
      let user = entry.user.id;
      let is_new = started
        .try_update_value(|started| started.insert((meeting, user)))
        .unwrap_or(false);
      if !is_new {
        continue;
      }
      let is_guest_user = entry.user.kind == teamcraft_proto::UserKind::Guest;
      let joined = i18n.t_with("meetings-joined-by-link", &[("name", &entry.user.display_name)]);
      let fingerprint = entry.maybe_knock_fingerprint.clone();
      spawn_local(async move {
        match hub.client().admit(meeting, user).await {
          Ok(()) => {
            let_in(state, chat, user, is_guest_user, fingerprint, ends_at);
            if state.is_alive() {
              state.show_toast_with_action(
                joined,
                ToastAction::RemoveFromMeeting {
                  conversation: chat,
                  user,
                },
              );
            }
          },
          Err(error) => {
            tracing::warn!(%error, "could not let in somebody who came with the meeting link");
            // The next reload of the lobbies tries again.
            started.try_update_value(|started| started.remove(&(meeting, user)));
          },
        }
      });
    }
  }
}

/// How long after a meeting's end the server keeps a guest's session (`guest_grace_secs`, default
/// two hours): the host's device removes the guest from the encryption group then at the latest.
const GUEST_ACCESS_GRACE_MS: i64 = 2 * 60 * 60_000;
/// How long the core gets to pin a key for a link's fragment or a guest's key code.
const VERIFY_WAIT: Duration = Duration::from_secs(8);
const VERIFY_POLL: Duration = Duration::from_millis(100);

/// Has the core make the meeting link's fragment from this device's pinned organizer key, once per
/// meeting; it lands in `hub.verified_links`.
fn verify_link(state: AppState, hub: MeetingsHub, conversation: ConversationId, meeting: MeetingId) {
  let is_new = hub
    .verifying
    .try_update_value(|verifying| verifying.insert(meeting))
    .unwrap_or(false);
  if is_new {
    spawn_local(async move {
      verified_fragment(state, hub, conversation, meeting).await;
    });
  }
}

/// The meeting link's fragment as this device makes it (`v=<pinned fingerprint>`), waiting up to
/// [`VERIFY_WAIT`] for the core; `None` if it can't (then no link is handed out).
async fn verified_fragment(
  state: AppState,
  hub: MeetingsHub,
  conversation: ConversationId,
  meeting: MeetingId,
) -> Option<String> {
  if let Some(fragment) = hub
    .verified_links
    .with_untracked(|links| links.get(&meeting).cloned())
  {
    return Some(fragment);
  }
  state.try_write(|backend| backend.prepare_guest_link(conversation, meeting));
  let polls = VERIFY_WAIT.as_millis() / VERIFY_POLL.as_millis();
  for _ in 0..polls {
    if !state.is_alive() {
      break;
    }
    if let Some(fragment) = state.read(|backend| backend.guest_link_fragment(meeting)) {
      hub.verified_links.update_if_alive(|links| {
        links.insert(meeting, fragment.clone());
      });
      return Some(fragment);
    }
    pause(VERIFY_POLL).await;
  }
  None
}

/// Has the core pin a waiting guest's key and make its key code, once per guest; it lands in
/// `hub.key_codes` for the lobby entry.
fn prepare_key_code(state: AppState, hub: MeetingsHub, guest: UserId) {
  let is_new = hub
    .coding
    .try_update_value(|coding| coding.insert(guest))
    .unwrap_or(false);
  if !is_new {
    return;
  }
  state.try_write(|backend| backend.prepare_guest_key_code(guest));
  spawn_local(async move {
    let polls = VERIFY_WAIT.as_millis() / VERIFY_POLL.as_millis();
    for _ in 0..polls {
      if !state.is_alive() {
        return;
      }
      if let Some(code) = state.read(|backend| backend.guest_key_code(guest)) {
        hub.key_codes.update_if_alive(|codes| {
          codes.insert(guest, code);
        });
        return;
      }
      pause(VERIFY_POLL).await;
    }
    // A later lobby reload asks again.
    hub.coding.try_update_value(|coding| coding.remove(&guest));
  });
}

/// The organizer's code from a link fragment (`v=<fingerprint>`): what the guest's page shows.
fn fragment_code(fragment: &str) -> String {
  code_of_fingerprint(fragment.strip_prefix("v=").unwrap_or(fragment))
}

/// Copies a link that had to be fetched first and says so. The click that asked for it may be too
/// long ago for the browser to allow the write; the link is then in its field to copy by hand.
async fn copy_fetched(state: AppState, i18n: I18n, link: String) {
  let copied = i18n.t("meetings-link-copied");
  if copy_text(&link).await && state.is_alive() {
    state.show_brief_toast(copied);
  }
}

/// Opens the dialog with the link of a meeting that was just scheduled or started.
fn show_created(state: AppState, hub: MeetingsHub, meeting: MeetingResponse) {
  hub.maybe_created.set_if_alive(Some(meeting));
  if state.is_alive() {
    state.overlay.set_if_alive(Overlay::MeetingCreated);
  }
}

/// Right after scheduling or "Meet now": the meeting's link to copy and send, the guest links for
/// its host, and joining it.
#[component]
pub fn MeetingCreatedDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let Some(meeting) = hub.maybe_created.get_untracked() else {
    state.close_overlay();
    return ().into_any();
  };
  let conversation = meeting.conversation_id;
  let join = move |_| {
    state.close_overlay();
    join_meeting(state, i18n, conversation);
  };
  view! {
    <Modal labelled_by="meeting-created-title">
      <header class="modal-head">
        <h2 id="meeting-created-title">{move || i18n.t("meetings-created-title")}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body meeting-created">
        <h3 class="meeting-title" translate="no">{meeting.title.clone()}</h3>
        <p class="muted-text">{move || i18n.t("meetings-created-hint")}</p>
        <LinkField link=meeting.join_url.clone() field_id="meeting-created-link".to_owned() label_key="meetings-link" />
        <p class="muted-text small">{move || i18n.t("meetings-link-hint")}</p>
        <h4 class="meeting-created-guests">{move || i18n.t("meetings-invite-guests")}</h4>
        <GuestLinks meeting=meeting.id conversation=conversation />
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
        <button type="button" class="btn btn-primary" data-autofocus on:click=join>{move || i18n.t("meetings-join")}</button>
      </footer>
    </Modal>
  }
  .into_any()
}

/// What to cancel of a meeting: of a repeating one this occurrence, it and all following, or the
/// whole series (issue #1006: never all of them without asking). Everybody invited is told.
#[component]
pub fn CancelMeetingDialog(meeting: MeetingId, occurrence: i64) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let Some(found) = hub.meetings.with_untracked(|all| {
    all
      .iter()
      .find(|item| item.id == meeting && item.starts_at.0 == occurrence)
      .cloned()
  }) else {
    state.close_overlay();
    return ().into_any();
  };
  let is_series = found.maybe_recurrence.is_some();
  let scope = RwSignal::new(if is_series {
    CancelScope::Occurrence
  } else {
    CancelScope::Series
  });
  let pending = PendingAction::new();
  let locale = i18n.locale().tag();
  let when = format!(
    "{}, {}",
    format_date(occurrence, locale),
    format_time(occurrence, locale)
  );
  let confirm = move |_| {
    let chosen = scope.get_untracked();
    pending.spawn(async move {
      hub
        .client()
        .cancel(meeting, chosen, UnixMillis(occurrence))
        .await
        .map_err(|error| error.to_failure())?;
      hub.refresh(me, now_ms()).await;
      state.close_overlay();
      Ok(())
    });
  };
  let options = [
    (CancelScope::Occurrence, "meetings-cancel-occurrence"),
    (CancelScope::Following, "meetings-cancel-following"),
    (CancelScope::Series, "meetings-cancel-series"),
  ];
  view! {
    <Modal labelled_by="cancel-meeting-title">
      <header class="modal-head">
        <h2 id="cancel-meeting-title">{move || i18n.t(if is_series { "meetings-cancel-title-series" } else { "meetings-cancel-title" })}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p>
          <strong translate="no">{found.title.clone()}</strong>
          <br />
          <span class="muted-text">{when}</span>
        </p>
        {is_series.then(|| view! {
          <fieldset class="radio-group">
            <legend class="visually-hidden">{move || i18n.t("meetings-cancel-title-series")}</legend>
            {options
              .into_iter()
              .map(|(option, key)| {
                let checked = option.clone();
                view! {
                  <label class="radio">
                    <input
                      type="radio"
                      name="cancel-scope"
                      value=option.as_str().to_owned()
                      prop:checked=move || scope.get() == checked
                      on:change=move |_| scope.set(option.clone())
                    />
                    <span>{move || i18n.t(key)}</span>
                  </label>
                }
              })
              .collect_view()}
          </fieldset>
        })}
        <p class="muted-text small">{move || i18n.t("meetings-cancel-hint")}</p>
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button type="button" class="btn" data-autofocus on:click=move |_| state.close_overlay()>
          {move || i18n.t("meetings-cancel-keep")}
        </button>
        <PendingButton pending=pending class="btn btn-danger" on_click=confirm>
          {move || i18n.t("meetings-cancel-confirm")}
        </PendingButton>
      </footer>
    </Modal>
  }
  .into_any()
}

// ---- Opening a meeting from its link ----------------------------------------------------------------

/// Where opening a meeting's link (`/meeting/{id}`) stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkVisit {
  Idle,
  Opening,
  /// Not in the meeting yet: knocked, waits for a host.
  Waiting(MeetingId),
  /// In the meeting; joins once its chat has loaded.
  Entering(ConversationId),
  Denied,
  /// Over or cancelled.
  Ended,
  Failed,
}

/// Opens the meeting a link names: straight in for its members, a knock for everybody else.
fn open_meeting_link(hub: MeetingsHub, visit: RwSignal<LinkVisit>, handle: MeetingHandle) {
  visit.set(LinkVisit::Opening);
  spawn_local(async move {
    let client = hub.client();
    let meeting = match handle {
      MeetingHandle::Id(meeting) => meeting,
      // The token names the meeting and knocks at once (members of its chat are just in).
      MeetingHandle::Token(token) => {
        let next = match client.knock_by_link(token.as_str()).await {
          Ok(knocked) => after_knock(hub, knocked.meeting_id, knocked.state).await,
          // An unknown token (made anew, or mistyped) is a failed visit, as is any other refusal.
          Err(error) => visit_failure(&error),
        };
        visit.set_if_alive(next);
        return;
      },
    };
    let next = match client.get(meeting).await {
      Ok(found) if is_over(&found, now_ms()) => LinkVisit::Ended,
      Ok(found) => LinkVisit::Entering(found.conversation_id),
      Err(error) if error.is_status(STATUS_NOT_FOUND) => match client.knock(meeting).await {
        Ok(lobby_state) => after_knock(hub, meeting, lobby_state).await,
        Err(error) => visit_failure(&error),
      },
      Err(error) => visit_failure(&error),
    };
    visit.set_if_alive(next);
  });
}

/// Asks again whether a host decided about the knock.
async fn poll_knock(hub: MeetingsHub, visit: RwSignal<LinkVisit>, meeting: MeetingId) {
  let next = match hub.client().my_lobby_state(meeting).await {
    Ok(lobby_state) => after_knock(hub, meeting, lobby_state).await,
    // Offline for a moment: keep waiting, the next tick asks again.
    Err(error) if error.to_failure().is_unreachable() => LinkVisit::Waiting(meeting),
    Err(error) => visit_failure(&error),
  };
  // The member may have stopped waiting in the meantime.
  if visit.try_get_untracked() == Some(LinkVisit::Waiting(meeting)) {
    visit.set_if_alive(next);
  }
}

async fn after_knock(hub: MeetingsHub, meeting: MeetingId, lobby_state: LobbyState) -> LinkVisit {
  match lobby_state {
    LobbyState::Waiting => LinkVisit::Waiting(meeting),
    LobbyState::Admitted => match hub.client().get(meeting).await {
      Ok(found) => LinkVisit::Entering(found.conversation_id),
      Err(error) => visit_failure(&error),
    },
    LobbyState::Denied => LinkVisit::Denied,
    LobbyState::Unknown(_) => LinkVisit::Failed,
  }
}

fn visit_failure(error: &MeetingError) -> LinkVisit {
  tracing::warn!(%error, "could not open the meeting link");
  if error.is_status(STATUS_CONFLICT) {
    LinkVisit::Ended
  } else {
    LinkVisit::Failed
  }
}

/// What opening a meeting link is doing, above the list.
#[component]
fn LinkVisitBanner(visit: RwSignal<LinkVisit>) -> impl IntoView {
  let i18n = use_i18n();
  let stop = move |_| visit.set(LinkVisit::Idle);
  move || {
    match visit.get() {
    LinkVisit::Idle => ().into_any(),
    LinkVisit::Opening | LinkVisit::Entering(_) => view! {
      <div class="meetings-visit" role="status">
        <span class="spinner"></span>
        <span>{move || i18n.t("meetings-visit-opening")}</span>
        <button type="button" class="btn btn-quiet btn-small" on:click=stop>{move || i18n.t("cancel")}</button>
      </div>
    }
    .into_any(),
    LinkVisit::Waiting(_) => view! {
      <div class="meetings-visit lobby" role="status">
        <span class="spinner"></span>
        <div class="grow">
          <strong>{move || i18n.t("meetings-visit-waiting-title")}</strong>
          <p>{move || i18n.t("meetings-visit-waiting")}</p>
        </div>
        <button type="button" class="btn" on:click=stop>{move || i18n.t("meetings-visit-cancel")}</button>
      </div>
    }
    .into_any(),
    outcome => {
      let key = match outcome {
        LinkVisit::Denied => "meetings-visit-denied",
        LinkVisit::Ended => "meetings-visit-ended",
        _ => "meetings-visit-failed",
      };
      view! {
        <div class="auth-alert meetings-visit" role="alert">
          <span class="grow">{move || i18n.t(key)}</span>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=stop>
            <Icon name=IconName::Close />
          </button>
        </div>
      }
      .into_any()
    },
  }
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
pub fn ScheduleMeetingDialog(
  /// The meeting being edited; without it the dialog schedules a new one.
  #[prop(optional)]
  maybe_edit: Option<MeetingId>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let now = now_ms();
  let maybe_before = maybe_edit.and_then(|id| {
    hub
      .meetings
      .with_untracked(|all| all.iter().find(|meeting| meeting.id == id).cloned())
  });
  let is_edit = maybe_before.is_some();
  // The notetaker can be asked into the meeting's calls (e2ee.md §8.2) when the verified policy
  // has it on and lets this user ask it in. Reactive: the policy may be verified only after the
  // dialog opened.
  let notetaker = Memo::new(move |_| {
    state.read(|backend| {
      let policy = backend.notetaker_policy()?;
      let me = backend.me();
      let may_ask = crate::notetaker::may_ask_in(
        &policy,
        !me.is_guest && !me.is_bot,
        backend.can_manage_integration_sources(),
      );
      policy.active_bot().filter(|_| may_ask)
    })
  });
  let form = RwSignal::new(match &maybe_before {
    Some(before) => ScheduleForm::from_meeting(before, me).with_notetaker(notetaker.get_untracked()),
    None => ScheduleForm::new(format_local_datetime_input(suggested_start(now))),
  });
  // A notetaker known only after the dialog opened is its entry, not an invisible member (it is a
  // bot: the people list leaves it out).
  Effect::new(move |_| {
    if let Some(bot) = notetaker.get()
      && form.with_untracked(|form| form.members.contains(&bot))
    {
      form.update(|form| *form = form.clone().with_notetaker(Some(bot)));
    }
  });
  // Whether the saved meeting has the notetaker, as the policy names it when saving.
  let had_notetaker = {
    let before = maybe_before.clone();
    move |maybe_bot: Option<UserId>| {
      maybe_bot.is_some_and(|bot| {
        before.as_ref().is_some_and(|before| {
          before
            .attendees
            .iter()
            .any(|attendee| attendee.maybe_user_id == Some(bot))
        })
      })
    }
  };
  let mut durations = DURATIONS.to_vec();
  if !durations.contains(&form.get_untracked().duration_minutes) {
    durations.push(form.get_untracked().duration_minutes);
    durations.sort_unstable();
  }
  let errors = RwSignal::new(Vec::<FormError>::new());
  // After a refused submit the errors follow the fields as they are corrected.
  let was_refused = RwSignal::new(false);
  let is_saving = RwSignal::new(false);
  // A series stays a series when edited (the server ends one by cancelling the following meetings).
  let is_saved_series = maybe_before
    .as_ref()
    .is_some_and(|before| before.maybe_recurrence.is_some());
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
  let validate = {
    let before = maybe_before.clone();
    move |form: &ScheduleForm| -> Vec<FormError> {
      let outcome = match &before {
        Some(before) => form.to_update_request(now_ms(), before).map(|_| ()),
        None => form.to_request(now_ms()).map(|_| ()),
      };
      outcome.err().unwrap_or_default()
    }
  };
  Effect::new(move |_| {
    let current = form.get();
    if was_refused.get_untracked() {
      errors.set(validate(&current));
    }
  });
  let refuse = move |found: Vec<FormError>| {
    errors.set(found);
    was_refused.set(true);
    focus_first_invalid(SCHEDULE_BODY_ID);
  };
  let submit = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if is_saving.get_untracked() {
      return;
    }
    let now = now_ms();
    if let Some(before) = maybe_before.clone() {
      match form.get_untracked().to_update_request(now, &before) {
        Err(found) => refuse(found),
        Ok(request) => {
          errors.set(Vec::new());
          is_saving.set(true);
          let conversation = before.conversation_id;
          let has_notetaker = form.get_untracked().maybe_notetaker.is_some();
          let had_notetaker = had_notetaker(notetaker.get_untracked());
          if had_notetaker && !has_notetaker {
            // Sent while the notetaker is still in the chat's group, before the edit takes it out.
            state.write(|backend| {
              backend.request_notetaker(conversation, teamcraft_proto::NotetakerRequest::Stop)
            });
          }
          spawn_local(async move {
            if save_meeting_edit(state, hub, me, before, request).await {
              state.close_overlay();
            }
            if has_notetaker && !had_notetaker {
              state.try_write(|backend| backend.request_notetaker(conversation, every_call()));
            }
            is_saving.set_if_alive(false);
          });
        },
      }
      return;
    }
    match form.get_untracked().to_request(now) {
      Err(found) => refuse(found),
      Ok(request) => {
        errors.set(Vec::new());
        is_saving.set(true);
        let has_notetaker = form.get_untracked().maybe_notetaker.is_some();
        spawn_local(async move {
          let client = hub.client();
          if let Some(meeting) = hub.run(me, now_ms(), client.create(&request)).await {
            // The server makes the chat without encryption: the users invited here go into its
            // group (from this request, never from the server's member list).
            let invitees = crate::meetings::invited_users(&request);
            // Gone only with the app; then the chat stays without encryption (nobody else founds it).
            state.try_write(|backend| backend.set_up_meeting_chat(meeting.conversation_id, invitees));
            // The lobby setting as the organizer chose it here, never as the server reports it,
            // decides whether this device lets guests of the link in by itself (#987 review).
            let has_lobby = request.has_lobby;
            state.try_write(|backend| backend.note_meeting_lobby(meeting.id, has_lobby));
            // Queued behind the chat's setup, so it goes into the group the notetaker is in.
            if has_notetaker {
              state.try_write(|backend| backend.request_notetaker(meeting.conversation_id, every_call()));
            }
            show_created(state, hub, meeting);
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
          <h2 id="schedule-title">{move || i18n.t(if is_edit { "schedule-edit-title" } else { "schedule-title" })}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body schedule-form" id=SCHEDULE_BODY_ID>
          <div class="field">
            <label for="meeting-title">{move || i18n.t("schedule-name")}</label>
            <input
              id="meeting-title"
              class="input"
              data-autofocus
              aria-invalid=invalid(errors, FormField::Title)
              aria-describedby=described(errors, FormField::Title)
              prop:value=move || form.get().title
              on:input=move |ev| form.update(|form| form.title = event_target_value(&ev))
            />
            <FieldError errors=errors field=FormField::Title />
          </div>
          <div class="field-row">
            <div class="field grow">
              <label for="meeting-start">{move || i18n.t("schedule-start")}</label>
              <input
                id="meeting-start"
                class="input"
                type="datetime-local"
                aria-invalid=invalid(errors, FormField::Start)
                aria-describedby=described(errors, FormField::Start)
                prop:value=move || form.get().starts_at
                on:input=move |ev| form.update(|form| form.starts_at = event_target_value(&ev))
              />
              <FieldError errors=errors field=FormField::Start />
            </div>
            <div class="field">
              <label for="meeting-duration">{move || i18n.t("schedule-duration")}</label>
              <select
                id="meeting-duration"
                class="input"
                aria-invalid=invalid(errors, FormField::Duration)
                aria-describedby=described(errors, FormField::Duration)
                on:change=move |ev| {
                  if let Ok(minutes) = event_target_value(&ev).parse::<u32>() {
                    form.update(|form| form.duration_minutes = minutes);
                  }
                }
              >
                {durations
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
              <FieldError errors=errors field=FormField::Duration />
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
              // The notetaker first, marked as such: invited like a member (e2ee.md §8.2).
              {move || notetaker.get().map(|bot| view! {
                <li class="invite-notetaker">
                  <label class="checkbox">
                    <input
                      type="checkbox"
                      prop:checked=move || form.get().maybe_notetaker.is_some()
                      on:change=move |ev| {
                        let is_ticked = event_target_checked(&ev);
                        form.update(|form| form.maybe_notetaker = is_ticked.then_some(bot));
                      }
                    />
                    <Icon name=IconName::Compose />
                    <span>{move || i18n.t("schedule-notetaker")}</span>
                  </label>
                  <p class="muted-text small">{move || i18n.t("schedule-notetaker-hint")}</p>
                </li>
              })}
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
          <fieldset
            class="field"
            id=FormField::Externals.id()
            tabindex="-1"
            aria-invalid=invalid(errors, FormField::Externals)
            aria-describedby=described(errors, FormField::Externals)
          >
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
            <FieldError errors=errors field=FormField::Externals />
          </fieldset>
          <RepeatFields form=form errors=errors is_saved_series=is_saved_series />
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=move || form.get().has_lobby
              on:change=move |ev| form.update(|form| form.has_lobby = event_target_checked(&ev))
            />
            <span>{move || i18n.t("schedule-lobby")}</span>
          </label>
          <p class="muted-text small">{move || i18n.t("schedule-lobby-hint")}</p>
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary" prop:disabled=move || is_saving.get()>
            {move || i18n.t(if is_edit { "schedule-save" } else { "schedule-submit" })}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// How the meeting repeats: every N days, weeks or months, on which days, and until when.
#[component]
fn RepeatFields(
  form: RwSignal<ScheduleForm>,
  errors: RwSignal<Vec<FormError>>,
  is_saved_series: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let repeat = Memo::new(move |_| form.with(|form| form.repeat));
  let is_repeating = move || repeat.get() != Repeat::Never;
  let number = |ev: &leptos::ev::Event| event_target_value(ev).trim().parse::<u32>().unwrap_or(0);
  view! {
    <fieldset class="field repeat-fields">
      <legend>{move || i18n.t("schedule-repeat")}</legend>
      <select
        id="meeting-repeat"
        class="input"
        aria-label=move || i18n.t("schedule-repeat")
        on:change=move |ev| {
          if let Some(choice) = Repeat::from_slug(&event_target_value(&ev)) {
            form.update(|form| form.set_repeat(choice));
          }
        }
      >
        {Repeat::ALL
          .into_iter()
          .map(|choice| {
            let key = format!("schedule-repeat-{}", choice.slug());
            view! {
              <option
                value=choice.slug()
                selected=move || repeat.get() == choice
                disabled=is_saved_series && choice == Repeat::Never
              >
                {move || i18n.t(&key)}
              </option>
            }
          })
          .collect_view()}
      </select>
      <Show when=is_repeating fallback=|| ()>
        <div class="repeat-row">
          <label for="meeting-interval">{move || i18n.t("schedule-every")}</label>
          <input
            id="meeting-interval"
            class="input narrow"
            type="number"
            inputmode="numeric"
            min="1"
            max="100"
            aria-invalid=invalid(errors, FormField::Interval)
            aria-describedby=described(errors, FormField::Interval)
            prop:value=move || form.with(|form| form.interval.to_string())
            on:input=move |ev| {
              let value = number(&ev);
              form.update(|form| form.interval = value);
            }
          />
          <span class="field-unit">
            {move || {
              let count = form.with(|form| form.interval) as usize;
              i18n.t_count(&format!("schedule-unit-{}", repeat.get().slug()), count)
            }}
          </span>
        </div>
        <FieldError errors=errors field=FormField::Interval />
        <Show when=move || repeat.get().has_weekdays() fallback=|| ()>
          <div class="repeat-row">
            <span id="meeting-weekdays-label">{move || i18n.t("schedule-on-days")}</span>
            <div
              id=FormField::Weekdays.id()
              class="set-chips"
              role="group"
              tabindex="-1"
              aria-labelledby="meeting-weekdays-label"
              aria-invalid=invalid(errors, FormField::Weekdays)
              aria-describedby=described(errors, FormField::Weekdays)
            >
              {WEEKDAYS
                .into_iter()
                .enumerate()
                .map(|(index, day)| {
                  let key = format!("weekday-{}", day.as_str());
                  view! {
                    <button
                      type="button"
                      class="set-chip"
                      aria-pressed=move || form.with(|form| form.weekdays[index]).to_string()
                      on:click=move |_| form.update(|form| form.weekdays[index] = !form.weekdays[index])
                    >
                      {move || i18n.t(&key)}
                    </button>
                  }
                })
                .collect_view()}
            </div>
          </div>
          <FieldError errors=errors field=FormField::Weekdays />
        </Show>
        <fieldset class="radio-group repeat-ends">
          <legend>{move || i18n.t("schedule-ends")}</legend>
          {SeriesEnd::ALL
            .into_iter()
            .map(|end| {
              let key = format!("schedule-ends-{}", end.slug());
              let is_chosen = move || form.with(|form| form.ends == end);
              view! {
                <div class="repeat-end">
                  <label class="radio">
                    <input
                      type="radio"
                      name="meeting-ends"
                      value=end.slug()
                      prop:checked=is_chosen
                      on:change=move |_| form.update(|form| form.ends = end)
                    />
                    <span>{move || i18n.t(&key)}</span>
                  </label>
                  {match end {
                    SeriesEnd::Never => ().into_any(),
                    SeriesEnd::AfterCount => view! {
                      <input
                        id=FormField::Count.id()
                        class="input narrow"
                        type="number"
                        inputmode="numeric"
                        min="1"
                        max="1000"
                        aria-label=move || i18n.t("schedule-ends-count")
                        aria-invalid=invalid(errors, FormField::Count)
                        aria-describedby=described(errors, FormField::Count)
                        // Typing into it chooses this end.
                        on:focus=move |_| form.update(|form| form.ends = end)
                        prop:value=move || form.with(|form| form.count.to_string())
                        on:input=move |ev| {
                          let value = number(&ev);
                          form.update(|form| form.count = value);
                        }
                      />
                      <span class="field-unit">
                        {move || i18n.t_count("schedule-unit-meetings", form.with(|form| form.count) as usize)}
                      </span>
                    }
                    .into_any(),
                    SeriesEnd::OnDate => view! {
                      <input
                        id=FormField::Until.id()
                        class="input"
                        type="date"
                        aria-label=move || i18n.t("schedule-ends-date")
                        aria-invalid=invalid(errors, FormField::Until)
                        aria-describedby=described(errors, FormField::Until)
                        // Typing into it chooses this end.
                        on:focus=move |_| form.update(|form| form.ends = end)
                        prop:value=move || form.with(|form| form.until.clone())
                        on:input=move |ev| form.update(|form| form.until = event_target_value(&ev))
                      />
                    }
                    .into_any(),
                  }}
                </div>
              }
            })
            .collect_view()}
        </fieldset>
        <FieldError errors=errors field=FormField::Count />
        <FieldError errors=errors field=FormField::Until />
      </Show>
    </fieldset>
  }
}

/// The message of the first error of `field`, where `aria-describedby` points to it.
#[component]
fn FieldError(errors: RwSignal<Vec<FormError>>, field: FormField) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_error = move || errors.with(|all| all.iter().find(|error| error.field() == field).cloned());
  view! {
    <p class="field-error" id=error_id(field.id())>
      {move || {
        maybe_error().map(|error| {
          let detail = match &error {
            FormError::InvalidEmail(email) => email.clone(),
            _ => String::new(),
          };
          i18n.t_with(error.key(), &[("email", &detail)])
        })
      }}
    </p>
  }
}

/// `aria-invalid` of a field: set while it has an error.
fn invalid(errors: RwSignal<Vec<FormError>>, field: FormField) -> impl Fn() -> Option<&'static str> + Copy {
  move || {
    errors
      .with(|all| all.iter().any(|error| error.field() == field))
      .then_some("true")
  }
}

/// `aria-describedby` of a field: its message while it has an error.
fn described(errors: RwSignal<Vec<FormError>>, field: FormField) -> impl Fn() -> Option<String> + Copy {
  move || invalid(errors, field)().map(|_| error_id(field.id()))
}

/// The notetaker's standing request for a meeting's calls (e2ee.md §8.2).
fn every_call() -> teamcraft_proto::NotetakerRequest {
  teamcraft_proto::NotetakerRequest::Join {
    scope: teamcraft_proto::NotetakerScope::EveryCall,
  }
}

/// Saves an edit and, only once the server took it, brings the meeting chat's encryption group in
/// line with the new invitee list (security-relevant: added users get access to later messages,
/// removed ones are taken out with a key-rotating commit; e2ee.md §4.6, "Meeting chats"). The list
/// comes from this request, never from the server's member list. Whether the server took it.
pub(crate) async fn save_meeting_edit(
  state: AppState,
  hub: MeetingsHub,
  me: UserId,
  before: MeetingResponse,
  request: UpdateMeetingRequest,
) -> bool {
  use crate::meetings::{clear_pending_edit, invitee_change, note_pending_edit, saved_change};
  // Diffed against the meeting as the server has it now, not the possibly stale list on screen.
  let before = hub.client().get(before.id).await.unwrap_or(before);
  // Written down before the save: if the app dies after the server took it but before the
  // chat's group lost the removed users, the next start finishes the removal.
  if let Some((_, removed)) = invitee_change(&before, &request) {
    note_pending_edit(before.id, removed, now_ms());
  }
  let Some(saved) = hub
    // async-ok: `MeetingsHub::run` is a plain method, not a signal.
    .run(me, now_ms(), hub.client().update(before.id, &request))
    .await
  else {
    // Not cleared: a timeout or a lost answer may hide a save the server took. The record stays
    // for `reconcile_pending_edits`, which removes the users who are off the list by then and
    // drops the record when they still attend (the save did not happen).
    return false;
  };
  if let Some(has_lobby) = request.maybe_has_lobby {
    state.try_write(|backend| backend.note_meeting_lobby(saved.id, has_lobby));
  }
  // From the saved meeting, which is what the server holds now.
  let (invitees, removed) = saved_change(&before, &saved);
  state.try_write(|backend| backend.update_meeting_chat_members(saved.conversation_id, invitees, removed));
  clear_pending_edit(before.id);
  true
}

/// The next full hour that is at least [`MIN_LEAD_MS`] away.
fn suggested_start(now: i64) -> i64 {
  let earliest = now + MIN_LEAD_MS;
  earliest.div_euclid(MS_PER_HOUR) * MS_PER_HOUR + MS_PER_HOUR
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_the_organizers_own_record_lets_the_device_admit_by_itself() {
    let guest = lobby_entry("guest", true, true, Some(&"ab".repeat(32)));
    // The server says "no lobby" and "came with the link": without the organizer's own record of
    // "no lobby" nobody is let in unattended (#987 review).
    assert!(!admits_by_itself(false, &guest));
    assert!(admits_by_itself(true, &guest));
    // An old link, a guest without a device or without a reported key: always a click.
    assert!(!admits_by_itself(
      true,
      &lobby_entry("guest", false, true, Some(&"ab".repeat(32)))
    ));
    assert!(!admits_by_itself(
      true,
      &lobby_entry("guest", true, false, Some(&"ab".repeat(32)))
    ));
    assert!(!admits_by_itself(true, &lobby_entry("guest", true, true, None)));
    assert!(admits_by_itself(true, &lobby_entry("member", true, true, None)));
  }

  fn lobby_entry(
    kind: &str,
    is_by_link: bool,
    has_device: bool,
    maybe_fingerprint: Option<&str>,
  ) -> LobbyEntry {
    serde_json::from_value(serde_json::json!({
      "user": {"id": "77", "display_name": "Gus", "kind": kind},
      "requested_at": 1,
      "is_by_link": is_by_link,
      "has_device": has_device,
      "knock_fingerprint": maybe_fingerprint,
    }))
    .unwrap()
  }
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
      is_occurrence_cancelled: false,
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }
}
