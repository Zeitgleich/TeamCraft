//! "Add people" in a DM or group DM call (docs/meetings.md, "A call becomes a meeting"). The people
//! picked here don't join the DM, which keeps its members and its history: the call moves into a
//! new meeting room with everybody who is in it and the added people, and everybody's app follows.

use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_proto::IdempotencyKey;

use crate::clock::now_ms;
use crate::components::pending_button::PendingButton;
use crate::components::people_picker::PeoplePicker;
use crate::components::workspace_dialogs::{Dialog, FailureNote};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::{ConversationId, User, UserId};
use crate::state::use_app_state;

#[component]
pub fn AddToCallDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_hub = use_context::<MeetingsHub>();
  let chosen = RwSignal::new(Vec::<User>::new());
  // One key per dialog: a retry after a lost answer gets the same meeting room, not a second one.
  let key = IdempotencyKey::random();
  // Who is in the call as the media layer shows them (devices verified against the DM's group);
  // the core keeps only members of that group.
  let in_call = Signal::derive(move || {
    let me = state.me().id;
    let mut people: HashSet<UserId> = state.read(|backend| {
      backend
        .call_participants(conversation)
        .into_iter()
        .map(|participant| participant.user)
        .collect()
    });
    people.insert(me);
    people
  });
  // Two steps, each with its failure in the dialog: the meeting room (REST), then the move (the
  // core founds the room's group, the server moves the call, the DM's group hears of it). A retry
  // after a failed move reuses the room.
  let creating = PendingAction::new();
  let moving = PendingAction::new();
  let maybe_room = RwSignal::new(None::<ConversationId>);
  let is_pending = move || creating.is_pending() || moving.is_pending();
  let move_into = move |room: ConversationId, in_call: Vec<UserId>, added: Vec<UserId>| {
    moving.run_and_then(
      || state.write(|backend| backend.move_call_to_meeting(conversation, room, in_call, added)),
      move || {
        let _ = state.overlay.try_set(crate::state::Overlay::None);
      },
    );
  };
  let add = move || {
    let Some(hub) = maybe_hub else { return };
    let picked: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    let me = state.me().id;
    let Some(MovePlan {
      in_call,
      added,
      invitees,
    }) = MovePlan::new(&in_call.get_untracked(), &picked, me)
    else {
      return;
    };
    if let Some(room) = maybe_room.get_untracked() {
      move_into(room, in_call, added);
      return;
    }
    let title = i18n.t("call-meeting-title");
    let client = hub.client();
    creating.spawn(async move {
      let meeting = client
        .start_instant(Some(title), invitees, Some(key))
        .await
        .map_err(|error| error.to_failure())?;
      if maybe_room.try_set(Some(meeting.conversation_id)).is_some() {
        // The dialog is gone (closed meanwhile): nothing is moved without it.
        return Ok(());
      }
      move_into(meeting.conversation_id, in_call, added);
      // The call screen shows the host controls of the new meeting.
      hub.refresh(me, now_ms()).await;
      Ok(())
    });
  };
  view! {
    <Dialog title_key="call-add-people">
      <form on:submit=move |ev: leptos::ev::SubmitEvent| { ev.prevent_default(); add(); }>
        <PeoplePicker
          chosen=chosen
          chosen_label_key="call-add-chosen"
          allows_guest_accounts=true
          already_in=in_call
          already_in_key="call-already-in"
          on_submit=Callback::new(move |()| add())
        />
        <p class="hint">{move || i18n.t("call-add-hint")}</p>
        <FailureNote pending=creating />
        <FailureNote pending=moving />
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button
            type="submit"
            class="btn btn-primary"
            prop:disabled=move || is_pending() || chosen.with(Vec::is_empty)
            aria-busy=move || is_pending().then_some("true")
          >
            <Show when=is_pending fallback=|| ()>
              <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
            </Show>
            {move || {
              let has_failed = creating.maybe_failure().is_some() || moving.maybe_failure().is_some();
              if has_failed { i18n.t("data-retry") } else { i18n.t("call-add") }
            }}
          </button>
        </footer>
      </form>
    </Dialog>
  }
}

/// Somebody else moved the DM call this device is in into a meeting room: who did it and whom they
/// added, from their encrypted announcement, and a tap to follow. Nothing moves before the tap.
#[component]
pub fn CallMoveBanner() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let pending = PendingAction::new();
  let me = state.me().id;
  let name_of = move |user: UserId| {
    state
      .read(|backend| backend.user(user))
      .map_or_else(|| i18n.t("left-in-group-unknown"), |user| user.display_name)
  };
  move || {
    let offer = call.move_offer.get().filter(|offer| offer.by != me)?;
    let added = offer
      .added
      .iter()
      .map(|user| name_of(*user))
      .collect::<Vec<_>>()
      .join(", ");
    let text = i18n.t_with(
      "call-move-offer",
      &[("name", &name_of(offer.by)), ("added", &added)],
    );
    let (from, to) = (offer.from, offer.to);
    Some(view! {
      <div class="call-move-banner" role="status">
        <span>{text}</span>
        <PendingButton pending=pending class="btn btn-primary btn-small" on_click=move |_| state.follow_call_move(from, to, pending)>
          {move || i18n.t("call-move-join")}
        </PendingButton>
      </div>
    })
  }
}

/// Who goes where when people are added to a call.
#[derive(Debug, PartialEq, Eq)]
struct MovePlan {
  /// Everybody in the call, the user included, sorted.
  in_call: Vec<UserId>,
  /// The picked people who aren't in the call yet, in the order picked: they ring.
  added: Vec<UserId>,
  /// The meeting's invitees: everybody but the user, who organizes it.
  invitees: Vec<UserId>,
}

impl MovePlan {
  /// `None` when nobody new was picked.
  fn new(in_call: &HashSet<UserId>, picked: &[UserId], me: UserId) -> Option<Self> {
    let mut added: Vec<UserId> = Vec::new();
    for user in picked {
      if !in_call.contains(user) && *user != me && !added.contains(user) {
        added.push(*user);
      }
    }
    if added.is_empty() {
      return None;
    }
    let mut in_call: Vec<UserId> = in_call.iter().copied().collect();
    in_call.sort_unstable();
    let invitees = in_call
      .iter()
      .chain(added.iter())
      .copied()
      .filter(|user| *user != me)
      .collect();
    Some(Self {
      in_call,
      added,
      invitees,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn the_meeting_invites_the_people_in_the_call_and_rings_only_the_added_ones() {
    let (me, bob, carol, dave): (UserId, UserId, UserId, UserId) =
      (id_from_raw(1), id_from_raw(2), id_from_raw(3), id_from_raw(4));
    let in_call = HashSet::from([me, bob]);

    let plan = MovePlan::new(&in_call, &[dave, bob, carol, dave], me).unwrap();

    assert_eq!(plan.added, vec![dave, carol], "Bob is in the call already");
    assert_eq!(plan.in_call, vec![me, bob]);
    assert_eq!(plan.invitees, vec![bob, dave, carol]);
  }

  #[test]
  fn nobody_new_moves_nothing() {
    let (me, bob): (UserId, UserId) = (id_from_raw(1), id_from_raw(2));
    let in_call = HashSet::from([me, bob]);
    assert_eq!(MovePlan::new(&in_call, &[bob, me], me), None);
    assert_eq!(MovePlan::new(&in_call, &[], me), None);
  }
}
