//! Host controls of a meeting (mute all, remove someone, lock, end; the organizer's co-hosts and
//! handover, #1720) and the list of answers to an invitation. The server enforces who may do what
//! (docs/meetings.md), the organizer's device signs who hosts; these are the buttons.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::meeting_hosts::{
  HostsChangeState, MeetingHostsChange, MeetingRole, handing_over, making_co_host, removing_co_host, role_of,
};
use teamcraft_proto::{
  ChannelRole, ErrorCode, MeetingAttendeeResponse, MeetingControlAction, MeetingId, RsvpStatus,
};

use crate::clock::now_ms;
use crate::components::pending_button::PendingButton;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{ConversationId, UserId};
use crate::reactive::SetIfAlive;
use crate::state::{AppState, use_app_state};

/// Ends the meeting for everyone (#1931): stops its call if one runs and closes it for its guests,
/// whether or not the host is in the call. The one implementation behind the host controls, the
/// call screen's hang-up menu and the tray. Afterwards (and when the server no longer knows the
/// meeting) the list is reloaded, so the row shows it ended or drops it. A meeting that doesn't
/// repeat is over for good: this device's records of it go (#1939); a series goes on.
pub async fn end_for_everyone(
  state: AppState,
  hub: MeetingsHub,
  meeting: MeetingId,
  me: UserId,
) -> Result<(), ActionFailure> {
  let is_one_off = hub.meetings.with_untracked(|all| {
    all
      .iter()
      .any(|item| item.id == meeting && item.maybe_recurrence.is_none())
  });
  let result = control(hub, meeting, MeetingControlAction::End).await;
  if result.is_ok() && is_one_off {
    state.try_write(|backend| backend.forget_meeting_records(meeting));
  }
  if result.is_ok() || result.as_ref().is_err_and(is_gone) {
    hub.refresh(me, now_ms()).await;
  }
  result
}

/// The meeting whose call `conversation` is, if `me` may end it for everyone: as its organizer or
/// a co-host (the server checks again, docs/meetings.md "Hosts"). Ad-hoc calls have none: their
/// hang-up only leaves. Reactive.
pub fn meeting_to_end(hub: MeetingsHub, conversation: ConversationId, me: UserId) -> Option<MeetingId> {
  hub
    .hosted_meeting_for(conversation, me)
    .or_else(|| hub.cohosted_meeting_for(conversation))
}

/// Asks before ending the meeting for everyone from the call (#1930): the hang-up menu's "End for
/// everyone…" and the tray's. Says what happens; the focus starts on Cancel, Escape cancels and
/// the focus goes back to `return_focus`. Once the server took it this device leaves too, also if
/// the SFU's word is late.
#[component]
pub fn EndPrompt(
  meeting: MeetingId,
  is_open: RwSignal<bool>,
  #[prop(optional)] return_focus: Option<NodeRef<html::Button>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let me = state.me().id;
  let call = state.call;
  let ending = PendingAction::new();
  let cancel = NodeRef::<html::Button>::new();
  Effect::new(move |_| {
    if let Some(button) = cancel.get() {
      drop(button.focus());
    }
  });
  let close = move || {
    is_open.set(false);
    if let Some(trigger) = return_focus.and_then(|node| node.get_untracked()) {
      drop(trigger.focus());
    }
  };
  let end = move |_| {
    ending.spawn(async move {
      end_for_everyone(state, hub, meeting, me).await?;
      is_open.set_if_alive(false);
      call.leave();
      Ok(())
    });
  };
  view! {
    <div
      class="leave-prompt end-prompt"
      role="alertdialog"
      aria-labelledby="end-prompt-title"
      aria-describedby="end-prompt-body"
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
          ev.stop_propagation();
          ev.prevent_default();
          close();
        }
      }
    >
      <h2 id="end-prompt-title">{move || i18n.t("call-end-title")}</h2>
      <p id="end-prompt-body">{move || i18n.t("host-end-explain")}</p>
      <div class="leave-prompt-choices">
        <PendingButton pending=ending class="btn btn-small btn-danger" on_click=end>
          {move || i18n.t("call-end-do")}
        </PendingButton>
        <button class="btn btn-small" node_ref=cancel on:click=move |_| close()>{move || i18n.t("cancel")}</button>
      </div>
    </div>
  }
}

/// Sends one host control; a refusal becomes a failure the toast words by its `error_code`.
async fn control(
  hub: MeetingsHub,
  meeting: MeetingId,
  action: MeetingControlAction,
) -> Result<(), ActionFailure> {
  hub
    .client()
    .control(meeting, action)
    .await
    .map_err(|error| error.to_failure())
}

/// Whether the server no longer knows the meeting (cancelled and removed, or never visible).
fn is_gone(failure: &ActionFailure) -> bool {
  failure.kind == FailureKind::Server(ErrorCode::MeetingNotFound)
}

/// Buttons for the host of a running meeting. `conversation` names the meeting's call: muting,
/// locking and removing act on it and need it running; ending works without it (#1931).
#[component]
pub fn HostControls(meeting: MeetingId, conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let is_locked = RwSignal::new(false);
  let is_confirming_end = RwSignal::new(false);
  let call = state.call;
  let me = state.me().id;
  // A call runs in the meeting: somebody is in it, as the call roster says, or this device is.
  let has_call = Memo::new(move |_| {
    call.is_in_call_for(conversation) || state.read(|backend| backend.huddle(conversation).is_some())
  });
  let (muting, locking, removing, ending) = (
    PendingAction::new(),
    PendingAction::new(),
    PendingAction::new(),
    PendingAction::new(),
  );
  let the_meeting = Memo::new(move |_| {
    hub
      .meetings
      .with(|all| all.iter().find(|item| item.id == meeting).cloned())
  });
  let is_organizer = Memo::new(move |_| {
    the_meeting.with(|maybe| maybe.as_ref().is_some_and(|item| item.organizer_user_id == me))
  });
  // Everybody else in the call with their part in the meeting. Hosts (the organizer, co-hosts and
  // the meeting chat's admins) can't be removed (docs/meetings.md): they get no "Remove".
  let participants = Memo::new(move |_| {
    let roles = the_meeting.get();
    state.read(|backend| {
      let admins = backend
        .channel_roles(conversation)
        .into_iter()
        .filter(|member| member.role == ChannelRole::Admin)
        .map(|member| member.user)
        .collect::<Vec<_>>();
      backend
        .call_participants(conversation)
        .into_iter()
        .filter(|participant| participant.user != me)
        .filter_map(|participant| backend.user(participant.user))
        .map(|user| {
          let role = roles
            .as_ref()
            .map_or(MeetingRole::Participant, |item| role_of(item, user.id));
          let is_admin = admins.contains(&user.id);
          (user, role, is_admin)
        })
        .collect::<Vec<_>>()
    })
  });
  // Whom a "Hand over" waits to be confirmed for.
  let handing_to = RwSignal::new(None::<UserId>);
  let change_hosts = move |make: fn(&teamcraft_proto::MeetingResponse, UserId) -> MeetingHostsChange,
                           user: UserId| {
    if let Some(item) = the_meeting.get_untracked() {
      let change = make(&item, user);
      state.write(|backend| backend.change_meeting_hosts(change));
    }
  };
  // How the last change of the hosts went on this device; a saved one shows in the list once the
  // server's `meeting_update` reloaded it.
  let hosts_change = Memo::new(move |_| state.read(|backend| backend.meeting_hosts_change(conversation)));
  let send = move |pending: PendingAction, action: MeetingControlAction| {
    pending.spawn(control(hub, meeting, action));
  };
  let toggle_lock = move |_| {
    let lock = !is_locked.get_untracked();
    is_locked.set(lock);
    let action = if lock {
      MeetingControlAction::Lock
    } else {
      MeetingControlAction::Unlock
    };
    locking.spawn(async move {
      let result = control(hub, meeting, action).await;
      if result.is_err() {
        // The server didn't take it: the button shows what holds.
        let _ = is_locked.try_set(!lock);
      }
      result
    });
  };
  let end = move |_| {
    is_confirming_end.set(false);
    ending.spawn(end_for_everyone(state, hub, meeting, me));
  };

  view! {
    <div class="host-controls" role="group" aria-label=move || i18n.t("host-controls")>
      <h3>{move || i18n.t("host-controls")}</h3>
      <div class="host-buttons">
        // Muting and locking act on the call: without one they are off, and say why.
        <button
          class="btn btn-small"
          prop:disabled=move || !has_call.get() || muting.is_pending()
          aria-describedby=move || (!has_call.get()).then_some("host-no-call")
          on:click=move |_| send(muting, MeetingControlAction::MuteAll)
        >
          {move || i18n.t("host-mute-all")}
        </button>
        <button
          class="btn btn-small"
          aria-pressed=move || is_locked.get().to_string()
          prop:disabled=move || !has_call.get() || locking.is_pending()
          aria-describedby=move || (!has_call.get()).then_some("host-no-call")
          on:click=toggle_lock
        >
          {move || if is_locked.get() { i18n.t("host-unlock") } else { i18n.t("host-lock") }}
        </button>
        <Show
          when=move || is_confirming_end.get()
          fallback=move || view! {
            <button class="btn btn-small btn-quiet" prop:disabled=move || ending.is_pending() on:click=move |_| is_confirming_end.set(true)>
              {move || i18n.t("host-end")}
            </button>
          }
        >
          <div class="host-confirm" role="group" aria-labelledby="host-end-explain">
            <p id="host-end-explain">{move || i18n.t("host-end-explain")}</p>
            <button class="btn btn-small btn-danger" on:click=end>{move || i18n.t("host-end-confirm")}</button>
            <button class="btn btn-small" on:click=move |_| is_confirming_end.set(false)>{move || i18n.t("cancel")}</button>
          </div>
        </Show>
      </div>
      <Show when=move || !has_call.get() fallback=|| ()>
        <p class="muted-text" id="host-no-call">{move || i18n.t("host-no-call")}</p>
      </Show>
      <ul class="host-participants">
        <For each=move || participants.get() key=|(user, role, is_admin)| (user.id, *role, *is_admin) let:entry>
          {
            let (user, role, is_admin) = entry;
            let id: UserId = user.id;
            let name = user.display_name.clone();
            let is_host = role != MeetingRole::Participant || is_admin;
            // Guests and bots host nothing: the server refuses them, so the menu doesn't offer it.
            let may_host = !user.is_guest && !user.is_bot;
            view! {
              <li>
                <span class="host-participant-name" translate="no">{name.clone()}</span>
                {role_badge(role).map(|key| view! { <span class="badge badge-host">{move || i18n.t(key)}</span> })}
                <span class="host-participant-actions">
                  {(!is_host).then(|| view! {
                    <button
                      class="btn btn-small"
                      prop:disabled=move || removing.is_pending()
                      on:click=move |_| send(removing, MeetingControlAction::RemoveParticipant { user_id: id })
                    >
                      {move || i18n.t("host-remove")}
                    </button>
                  })}
                  <Show when=move || is_organizer.get() && may_host fallback=|| ()>
                    {match role {
                      MeetingRole::CoHost => view! {
                        <button class="btn btn-small" on:click=move |_| change_hosts(removing_co_host, id)>
                          {move || i18n.t("host-remove-co-host")}
                        </button>
                      }
                      .into_any(),
                      _ => view! {
                        <button class="btn btn-small" on:click=move |_| change_hosts(making_co_host, id)>
                          {move || i18n.t("host-make-co-host")}
                        </button>
                      }
                      .into_any(),
                    }}
                    <button class="btn btn-small btn-quiet" on:click=move |_| handing_to.set(Some(id))>
                      {move || i18n.t("host-hand-over")}
                    </button>
                  </Show>
                </span>
                <Show when=move || handing_to.get() == Some(id) fallback=|| ()>
                  {
                    let name = name.clone();
                    view! {
                      <div class="host-confirm" role="group">
                        <p>{move || i18n.t_with("host-hand-over-confirm", &[("name", &name)])}</p>
                        <button
                          class="btn btn-small btn-primary"
                          on:click=move |_| {
                            change_hosts(handing_over, id);
                            handing_to.set(None);
                          }
                        >
                          {move || i18n.t("host-hand-over-do")}
                        </button>
                        <button class="btn btn-small" on:click=move |_| handing_to.set(None)>{move || i18n.t("cancel")}</button>
                      </div>
                    }
                  }
                </Show>
              </li>
            }
          }
        </For>
      </ul>
      {move || match hosts_change.get() {
        Some(HostsChangeState::Saving) => Some(view! { <p class="muted-text" role="status">{i18n.t("host-hosts-saving")}</p> }.into_any()),
        Some(HostsChangeState::Failed { reason }) => Some(
          view! { <p class="field-error" role="alert">{i18n.t_with("host-hosts-failed", &[("detail", &reason)])}</p> }.into_any(),
        ),
        _ => None,
      }}
    </div>
  }
}

/// The others in the call of `conversation` who could take its meeting over: no guests, no bots
/// (the server refuses them as hosts).
pub fn handover_candidates(state: AppState, conversation: ConversationId) -> Vec<crate::model::User> {
  let me = state.me().id;
  state.read(|backend| {
    backend
      .call_participants(conversation)
      .into_iter()
      .filter_map(|participant| backend.user(participant.user))
      .filter(|user| !user.is_guest && !user.is_bot && user.id != me)
      .collect()
  })
}

/// How many people of the call the leave prompt offers to hand the meeting to.
const MAX_HANDOVER_CHOICES: usize = 5;

/// Leaving the call of a meeting `me` organizes while people wait in its lobby and nobody else
/// hosts it: whom the meeting can go to (#1720).
#[derive(Clone, Debug, PartialEq)]
pub struct HandoverOffer {
  pub meeting: teamcraft_proto::MeetingResponse,
  pub waiting: usize,
  pub candidates: Vec<crate::model::User>,
}

/// The offer to make when `me` leaves the call of `conversation`, if any
/// ([`teamcraft_client_api::meeting_hosts::should_offer_handover`]). `candidates`: the other people
/// in the call who could host (no guests, no bots).
pub fn handover_offer(
  hub: MeetingsHub,
  conversation: ConversationId,
  me: UserId,
  candidates: Vec<crate::model::User>,
) -> Option<HandoverOffer> {
  let meeting = hub.meetings.with_untracked(|all| {
    all
      .iter()
      .find(|meeting| meeting.conversation_id == conversation && meeting.organizer_user_id == me)
      .cloned()
  })?;
  let waiting = hub.lobbies.with_untracked(|lobbies| {
    lobbies
      .iter()
      .find(|(id, _)| *id == meeting.id)
      .map_or(0, |(_, entries)| entries.len())
  });
  let ids: Vec<UserId> = candidates.iter().map(|user| user.id).collect();
  teamcraft_client_api::meeting_hosts::should_offer_handover(&meeting, me, waiting, &ids).then(|| {
    HandoverOffer {
      meeting,
      waiting,
      candidates: candidates.into_iter().take(MAX_HANDOVER_CHOICES).collect(),
    }
  })
}

/// Asks the organizer who leaves with people waiting whom to hand the meeting to: each choice
/// signs the handover (the organizer stays co-host) and leaves; "Leave anyway" just leaves.
#[component]
pub fn LeavePrompt(offer: HandoverOffer, prompt: RwSignal<Option<HandoverOffer>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let waiting = offer.waiting;
  let meeting = offer.meeting.clone();
  view! {
    <div class="leave-prompt" role="alertdialog" aria-labelledby="leave-prompt-title" aria-describedby="leave-prompt-body">
      <h2 id="leave-prompt-title">{move || i18n.t("host-leave-title")}</h2>
      <p id="leave-prompt-body">{move || i18n.t_count("host-leave-body", waiting)}</p>
      <div class="leave-prompt-choices">
        {offer
          .candidates
          .into_iter()
          .map(|user| {
            let meeting = meeting.clone();
            let (id, name) = (user.id, user.display_name.clone());
            view! {
              <button
                class="btn btn-small btn-primary"
                on:click=move |_| {
                  let change = handing_over(&meeting, id);
                  state.write(|backend| backend.change_meeting_hosts(change));
                  prompt.set(None);
                  call.leave();
                }
              >
                <span translate="no">{move || i18n.t_with("host-leave-hand-over", &[("name", &name)])}</span>
              </button>
            }
          })
          .collect_view()}
        <button
          class="btn btn-small"
          on:click=move |_| {
            prompt.set(None);
            call.leave();
          }
        >
          {move || i18n.t("host-leave-anyway")}
        </button>
        <button class="btn btn-small btn-quiet" data-autofocus on:click=move |_| prompt.set(None)>{move || i18n.t("cancel")}</button>
      </div>
    </div>
  }
}

/// The badge of a part in the meeting: the organizer is "Host", a co-host "Co-host".
pub fn role_badge(role: MeetingRole) -> Option<&'static str> {
  match role {
    MeetingRole::Organizer => Some("host-role-organizer"),
    MeetingRole::CoHost => Some("host-role-co-host"),
    MeetingRole::Participant => None,
  }
}

/// How many invitees gave which answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RsvpCounts {
  pub accepted: usize,
  pub tentative: usize,
  pub declined: usize,
  pub awaiting: usize,
}

pub fn count_rsvps(attendees: &[MeetingAttendeeResponse]) -> RsvpCounts {
  let mut counts = RsvpCounts::default();
  for attendee in attendees {
    match attendee.rsvp {
      RsvpStatus::Accepted => counts.accepted += 1,
      RsvpStatus::Tentative => counts.tentative += 1,
      RsvpStatus::Declined => counts.declined += 1,
      _ => counts.awaiting += 1,
    }
  }
  counts
}

/// The answers to an invitation, grouped by answer.
#[component]
pub fn RsvpList(attendees: Vec<MeetingAttendeeResponse>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let people = state.read(|backend| backend.users());
  let name_of = move |attendee: &MeetingAttendeeResponse| -> String {
    attendee
      .maybe_user_id
      .and_then(|id| {
        people
          .iter()
          .find(|user| user.id == id)
          .map(|user| user.display_name.clone())
      })
      .or_else(|| attendee.maybe_name.clone())
      .or_else(|| attendee.maybe_email.clone())
      .unwrap_or_default()
  };
  let counts = count_rsvps(&attendees);
  let groups: Vec<(&'static str, Vec<String>)> = [
    ("meetings-rsvp-accepted", RsvpStatus::Accepted),
    ("meetings-rsvp-tentative", RsvpStatus::Tentative),
    ("meetings-rsvp-declined", RsvpStatus::Declined),
    ("meetings-rsvp-awaiting", RsvpStatus::NeedsAction),
  ]
  .into_iter()
  .map(|(key, status)| {
    let names = attendees
      .iter()
      .filter(|attendee| match status {
        RsvpStatus::NeedsAction => !matches!(
          attendee.rsvp,
          RsvpStatus::Accepted | RsvpStatus::Tentative | RsvpStatus::Declined
        ),
        ref wanted => attendee.rsvp == *wanted,
      })
      .map(&name_of)
      .filter(|name| !name.is_empty())
      .collect::<Vec<_>>();
    (key, names)
  })
  .filter(|(_, names)| !names.is_empty())
  .collect();
  view! {
    <div class="rsvp-list">
      <p class="muted-text">
        {move || i18n.t_with("meetings-rsvp-summary", &[
          ("accepted", &counts.accepted.to_string()),
          ("tentative", &counts.tentative.to_string()),
          ("declined", &counts.declined.to_string()),
          ("awaiting", &counts.awaiting.to_string()),
        ])}
      </p>
      {groups
        .into_iter()
        .map(|(key, names)| {
          view! {
            <h4>{move || i18n.t(key)}</h4>
            <ul>{names.into_iter().map(|name| view! { <li translate="no">{name}</li> }).collect_view()}</ul>
          }
        })
        .collect_view()}
    </div>
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::feedback::failure_message_key;
  use crate::meetings::MeetingError;

  /// What a refusal of a host control turns into: the toast's key and whether the row is gone.
  fn refused(status: u16, code: &str) -> (&'static str, bool) {
    let failure = MeetingError::Refused {
      status,
      code: code.to_owned(),
    }
    .to_failure();
    (failure_message_key(&failure), is_gone(&failure))
  }

  #[test]
  fn refused_host_controls_are_worded_never_raw() {
    // #1931: "the server refused the request (404): call_not_found" reached the UI as it was.
    assert_eq!(refused(404, "call_not_found"), ("error-call-not-running", false));
    assert_eq!(refused(404, "meeting_not_found"), ("error-meeting-gone", true));
    assert_eq!(refused(409, "conflict"), ("error-conflict", false));
    assert_eq!(refused(403, "forbidden"), ("error-forbidden", false));
    assert_eq!(refused(503, "unavailable"), ("error-server-trouble", false));
  }

  fn attendee(rsvp: RsvpStatus) -> MeetingAttendeeResponse {
    MeetingAttendeeResponse {
      maybe_user_id: None,
      maybe_email: Some("a@example.com".into()),
      maybe_name: None,
      rsvp,
    }
  }

  #[test]
  fn answers_are_counted_and_unknown_ones_are_awaiting() {
    let counts = count_rsvps(&[
      attendee(RsvpStatus::Accepted),
      attendee(RsvpStatus::Accepted),
      attendee(RsvpStatus::Declined),
      attendee(RsvpStatus::NeedsAction),
    ]);
    assert_eq!(
      counts,
      RsvpCounts {
        accepted: 2,
        tentative: 0,
        declined: 1,
        awaiting: 1
      }
    );
  }
}
