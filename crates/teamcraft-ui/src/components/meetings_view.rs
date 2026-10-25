//! The meetings screen: upcoming meetings, the schedule dialog, RSVP, the meeting's links and the
//! lobby of meetings the user hosts, and opening a meeting from its link. The server owns all state
//! (docs/meetings.md); [`MeetingsHub`] mirrors it.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{
  IdempotencyKey, LobbyEntry, LobbyState, MeetingId, MeetingResponse, MeetingStatus, RsvpStatus,
};

use crate::calls::{CallKind, CallRequest};
use crate::clock::{format_date, format_local_datetime_input, format_time, local_day, now_ms};
use crate::components::compact_back::CompactBack;
use crate::components::icons::{Icon, IconName};
use crate::components::meeting_controls::{HostControls, RsvpList};
use crate::components::message_actions::copy_text;
use crate::components::modal::Modal;
use crate::components::pending_button::PendingButton;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::meetings::{
  ExternalInvitee, FormError, MeetingError, MeetingsHub, Repeat, STATUS_CONFLICT, STATUS_NOT_FOUND,
  ScheduleForm, hosts_lobby, is_over, my_rsvp,
};
use crate::model::{ConversationId, UserId};
use crate::reactive::{SetIfAlive, use_interval_while_visible};
use crate::state::{AppState, Overlay, use_app_state};

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
        show_created(state, hub, meeting);
      }
      is_starting.set_if_alive(false);
    });
  };

  // A meeting link (`/meeting/{id}`) the app was opened with, or that was followed later.
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
  let join_url = meeting.join_url.clone();
  let shows_guest_links = RwSignal::new(false);
  let show_responses = RwSignal::new(false);
  let response_total = meeting.attendees.len();
  let attendees_for_list = meeting.attendees.clone();
  let conversation = meeting.conversation_id;
  // Host controls matter while the meeting runs (a little before the start counts, people dial in early).
  let is_live_host = is_host && !over && meeting.starts_at.0 - 15 * 60_000 <= now_ms();
  let meeting_for_join = meeting.clone();
  let has_recurrence = meeting.maybe_recurrence.is_some();
  let has_lobby = hosts_lobby(&meeting, me);

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
        {(!over).then(|| view! { <CopyButton text=join_url.clone() label_key="meetings-copy-link" /> })}
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
              <button class="btn btn-quiet" on:click=cancel>{move || i18n.t("meetings-cancel")}</button>
            }
          })}
      </div>
      <Show when=move || shows_guest_links.get() fallback=|| ()>
        <GuestLinks meeting=id />
      </Show>
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

/// The links a host hands to people without an account: the meeting's shared guest link (many
/// guests, each waits in the lobby, revocable) and personal one-time links. Each button makes the
/// link and copies it in one click; the link stays in a field in case the browser refused the copy.
#[component]
fn GuestLinks(meeting: MeetingId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let maybe_shared = RwSignal::new(None::<String>);
  let maybe_one_time = RwSignal::new(None::<String>);
  let (sharing, revoking, making) = (PendingAction::new(), PendingAction::new(), PendingAction::new());

  let copy_shared = move |_| {
    sharing.spawn(async move {
      let link = hub
        .client()
        .shared_guest_link(meeting)
        .await
        .map_err(|error| error.to_failure())?;
      copy_fetched(state, i18n, link.join_url.clone()).await;
      maybe_shared.set_if_alive(Some(link.join_url));
      Ok(())
    });
  };
  let revoke_shared = move |_| {
    let revoked = i18n.t("meetings-guest-link-revoked");
    revoking.spawn(async move {
      hub
        .client()
        .revoke_shared_guest_link(meeting)
        .await
        .map_err(|error| error.to_failure())?;
      maybe_shared.set_if_alive(None);
      if state.is_alive() {
        state.show_toast(revoked);
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
      copy_fetched(state, i18n, link.join_url.clone()).await;
      maybe_one_time.set_if_alive(Some(link.join_url));
      Ok(())
    });
  };

  view! {
    <div class="meeting-guest-links" role="group" aria-label=move || i18n.t("meetings-invite-guests")>
      <div class="meeting-actions">
        <PendingButton pending=sharing class="btn" on_click=copy_shared>
          <Icon name=IconName::Copy />
          <span>{move || i18n.t("meetings-shared-guest-link")}</span>
        </PendingButton>
        <PendingButton pending=making class="btn" on_click=copy_one_time>
          <Icon name=IconName::Copy />
          <span>{move || i18n.t("meetings-guest-link")}</span>
        </PendingButton>
      </div>
      {move || {
        maybe_shared.get().map(|link| {
          view! {
            <div class="meeting-link">
              <LinkField link=link field_id=format!("shared-guest-link-{meeting}") label_key="meetings-shared-guest-link-label" />
              <p class="muted-text small">{move || i18n.t("meetings-shared-guest-link-hint")}</p>
              <div>
                <PendingButton pending=revoking class="btn btn-quiet btn-small" on_click=revoke_shared>
                  {move || i18n.t("meetings-revoke-guest-link")}
                </PendingButton>
              </div>
            </div>
          }
        })
      }}
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
        <GuestLinks meeting=meeting.id />
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
fn open_meeting_link(hub: MeetingsHub, visit: RwSignal<LinkVisit>, meeting: MeetingId) {
  visit.set(LinkVisit::Opening);
  spawn_local(async move {
    let client = hub.client();
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
          if let Some(meeting) = hub.run(me, now_ms(), client.create(&request)).await {
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
