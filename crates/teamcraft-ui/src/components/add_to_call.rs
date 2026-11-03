//! "Add people" in a DM or group DM call (docs/meetings.md, "A call becomes a meeting"). The people
//! picked here don't join the DM, which keeps its members and its history: the call moves into a
//! new meeting room with everybody who is in it and the added people, and everybody's app follows.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
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
  // The dialog stays open until the core answered (the room, its encryption group, the move);
  // a failure shows here, and the retry with the same key gets the same room.
  let pending = PendingAction::new();
  let add = move || {
    let picked: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    let me = state.me().id;
    let Some(MovePlan { in_call, added }) = MovePlan::new(&in_call.get_untracked(), &picked, me) else {
      return;
    };
    let title = i18n.t("call-meeting-title");
    pending.run_and_then(
      || state.write(|backend| backend.add_people_to_call(conversation, in_call, added, Some(title), key)),
      move || {
        let _ = state.overlay.try_set(crate::state::Overlay::None);
        // The call screen shows the host controls of the new meeting.
        if let Some(hub) = maybe_hub {
          spawn_local(async move { hub.refresh(me, now_ms()).await });
        }
      },
    );
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
        <FailureNote pending=pending />
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button
            type="submit"
            class="btn btn-primary"
            prop:disabled=move || pending.is_pending() || chosen.with(Vec::is_empty)
            aria-busy=move || pending.is_pending().then_some("true")
          >
            <Show when=move || pending.is_pending() fallback=|| ()>
              <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
            </Show>
            {move || if pending.maybe_failure().is_some() { i18n.t("data-retry") } else { i18n.t("call-add") }}
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
  let name_of = move |user: UserId| {
    state
      .read(|backend| backend.user(user))
      .map_or_else(|| i18n.t("left-in-group-unknown"), |user| user.display_name)
  };
  move || {
    let offer = call.move_offer.get().filter(|offer| !offer.is_requested_here)?;
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
    Some(Self { in_call, added })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn the_added_people_are_the_picked_ones_not_in_the_call() {
    let (me, bob, carol, dave): (UserId, UserId, UserId, UserId) =
      (id_from_raw(1), id_from_raw(2), id_from_raw(3), id_from_raw(4));
    let in_call = HashSet::from([me, bob]);

    let plan = MovePlan::new(&in_call, &[dave, bob, carol, dave], me).unwrap();

    assert_eq!(plan.added, vec![dave, carol], "Bob is in the call already");
    assert_eq!(plan.in_call, vec![me, bob]);
  }

  #[test]
  fn nobody_new_moves_nothing() {
    let (me, bob): (UserId, UserId) = (id_from_raw(1), id_from_raw(2));
    let in_call = HashSet::from([me, bob]);
    assert_eq!(MovePlan::new(&in_call, &[bob, me], me), None);
    assert_eq!(MovePlan::new(&in_call, &[], me), None);
  }
}
