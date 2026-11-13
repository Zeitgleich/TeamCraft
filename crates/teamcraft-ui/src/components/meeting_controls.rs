//! Host controls of a meeting (mute all, remove someone, lock, end; the organizer's co-hosts and
//! handover, #1720) and the list of answers to an invitation. The server enforces who may do what
//! (docs/meetings.md), the organizer's device signs who hosts; these are the buttons.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::meeting_hosts::{
  HostsChangeState, MeetingHostsChange, MeetingRole, handing_over, making_co_host, removing_co_host, role_of,
};
use teamcraft_proto::{ChannelRole, MeetingAttendeeResponse, MeetingControlAction, MeetingId, RsvpStatus};

use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{ConversationId, UserId};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// Buttons for the host of a running meeting. `conversation` names the call whose participants can
/// be removed.
#[component]
pub fn HostControls(meeting: MeetingId, conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let is_locked = RwSignal::new(false);
  let is_confirming_end = RwSignal::new(false);
  // The tray's "End for everyone…" for this call: the confirmation is shown right away.
  let call = state.call;
  Effect::new(move |_| {
    if call.is_end_for_everyone_asked.get() && call.is_in_call_for(conversation) {
      is_confirming_end.set(true);
      call.is_end_for_everyone_asked.set(false);
    }
  });
  let notice = RwSignal::new(None::<String>);
  let me = state.me().id;
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
  let send = move |action: MeetingControlAction| {
    spawn_local(async move {
      match hub.client().control(meeting, action).await {
        Ok(()) => notice.set_if_alive(None),
        Err(error) => notice.set_if_alive(Some(error.to_string())),
      }
    });
  };

  view! {
    <div class="host-controls" role="group" aria-label=move || i18n.t("host-controls")>
      <h3>{move || i18n.t("host-controls")}</h3>
      <div class="host-buttons">
        <button class="btn btn-small" on:click=move |_| send(MeetingControlAction::MuteAll)>{move || i18n.t("host-mute-all")}</button>
        <button
          class="btn btn-small"
          aria-pressed=move || is_locked.get().to_string()
          on:click=move |_| {
            let lock = !is_locked.get_untracked();
            is_locked.set(lock);
            send(if lock { MeetingControlAction::Lock } else { MeetingControlAction::Unlock });
          }
        >
          {move || if is_locked.get() { i18n.t("host-unlock") } else { i18n.t("host-lock") }}
        </button>
        <Show
          when=move || is_confirming_end.get()
          fallback=move || view! { <button class="btn btn-small btn-quiet" on:click=move |_| is_confirming_end.set(true)>{move || i18n.t("host-end")}</button> }
        >
          <button class="btn btn-small btn-primary" on:click=move |_| { send(MeetingControlAction::End); is_confirming_end.set(false); }>
            {move || i18n.t("host-end-confirm")}
          </button>
          <button class="btn btn-small" on:click=move |_| is_confirming_end.set(false)>{move || i18n.t("cancel")}</button>
        </Show>
      </div>
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
                    <button class="btn btn-small" on:click=move |_| send(MeetingControlAction::RemoveParticipant { user_id: id })>
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
      {move || notice.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
    </div>
  }
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

/// The other people in the call of `conversation` who could take the meeting over: no guests, no
/// bots, not `me`.
pub fn handover_candidates(
  state: crate::state::AppState,
  conversation: ConversationId,
  me: UserId,
) -> Vec<crate::model::User> {
  state.read(|backend| {
    backend
      .call_participants(conversation)
      .into_iter()
      .filter_map(|participant| backend.user(participant.user))
      .filter(|user| !user.is_guest && !user.is_bot && user.id != me)
      .collect()
  })
}

/// Leaves the call of `conversation`, or asks the organizer whom to hand the meeting over to first
/// (into `prompt`) when people wait in its lobby and nobody else hosts it.
pub fn leave_or_ask(
  state: crate::state::AppState,
  maybe_hub: Option<MeetingsHub>,
  conversation: ConversationId,
  prompt: RwSignal<Option<HandoverOffer>>,
) {
  let me = state.me().id;
  let candidates = handover_candidates(state, conversation, me);
  match maybe_hub.and_then(|hub| handover_offer(hub, conversation, me, candidates)) {
    Some(offer) => prompt.set(Some(offer)),
    None => state.call.leave(),
  }
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
