//! Host controls of a meeting (mute all, remove someone, lock, end) and the list of answers to an
//! invitation. The server enforces who may do what (docs/meetings.md); these are the buttons.

use leptos::prelude::*;
use leptos::task::spawn_local;
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
  let notice = RwSignal::new(None::<String>);
  let me = state.me().id;
  // Hosts (the organizer and the meeting chat's admins) can't be removed (docs/meetings.md): they
  // get no "Remove".
  let participants = Memo::new(move |_| {
    state.read(|backend| {
      let hosts = backend
        .channel_roles(conversation)
        .into_iter()
        .filter(|member| member.role == ChannelRole::Admin)
        .map(|member| member.user)
        .collect::<Vec<_>>();
      backend
        .call_participants(conversation)
        .into_iter()
        .filter(|participant| participant.user != me && !hosts.contains(&participant.user))
        .filter_map(|participant| backend.user(participant.user))
        .collect::<Vec<_>>()
    })
  });
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
        <For each=move || participants.get() key=|user| user.id let:user>
          {
            let id: UserId = user.id;
            view! {
              <li>
                <span translate="no">{user.display_name.clone()}</span>
                <button class="btn btn-small" on:click=move |_| send(MeetingControlAction::RemoveParticipant { user_id: id })>
                  {move || i18n.t("host-remove")}
                </button>
              </li>
            }
          }
        </For>
      </ul>
      {move || notice.get().map(|message| view! { <p class="field-error" role="alert">{message}</p> })}
    </div>
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
