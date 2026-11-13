//! "Add people" in a call. In a DM or group DM call (docs/meetings.md, "A call becomes a meeting")
//! the people picked here don't join the DM, which keeps its members and its history: the call
//! moves into a new meeting room with everybody who is in it and the added people, and everybody's
//! app follows. In a meeting's call (#1141) the organizer invites them to the meeting, which lets
//! them into its chat and call; anybody else hands out the meeting's link, since only the organizer
//! changes who is invited.

use std::collections::HashSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{IdempotencyKey, MeetingId};

use crate::clock::now_ms;
use crate::components::meetings_view::{MeetingLinkButton, save_meeting_edit};
use crate::components::pending_button::PendingButton;
use crate::components::people_picker::PeoplePicker;
use crate::components::workspace_dialogs::{Dialog, FailureNote};
use crate::feedback::{ActionFailure, PendingAction};
use crate::i18n::use_i18n;
use crate::meetings::{MeetingsHub, adding_invitees, attending_users};
use crate::model::{ConversationId, User, UserId};
use crate::state::{AppState, use_app_state};

/// What "Add people" does in the call of a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddPeople {
  /// A DM or group DM: the call moves into a new meeting room with the added people.
  MoveToMeeting,
  /// A meeting the user organizes: the people picked become its invitees.
  Invite(MeetingId),
  /// Somebody else's meeting: only its organizer invites, so its link is handed out instead.
  ShareLink(MeetingId),
}

impl AddPeople {
  /// What "Add people" does: `is_direct_like` for a DM or group DM, `maybe_meeting` (its ID and its
  /// organizer) for a meeting's chat. A guest adds nobody.
  pub fn of(
    is_direct_like: bool,
    maybe_meeting: Option<(MeetingId, UserId)>,
    me: UserId,
    is_guest: bool,
  ) -> Option<Self> {
    if is_guest {
      return None;
    }
    if is_direct_like {
      return Some(Self::MoveToMeeting);
    }
    let (meeting, organizer) = maybe_meeting?;
    Some(if organizer == me {
      Self::Invite(meeting)
    } else {
      Self::ShareLink(meeting)
    })
  }

  /// [`Self::of`] for the call on `conversation`, reactive.
  pub fn for_call(
    state: AppState,
    maybe_hub: Option<MeetingsHub>,
    conversation: ConversationId,
  ) -> Option<Self> {
    let me = state.me();
    let is_direct_like = state
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conversation| conversation.kind.is_direct_like());
    let maybe_meeting = maybe_hub.and_then(|hub| {
      hub.meetings.with(|all| {
        all
          .iter()
          .find(|meeting| meeting.conversation_id == conversation)
          .map(|meeting| (meeting.id, meeting.organizer_user_id))
      })
    });
    Self::of(is_direct_like, maybe_meeting, me.id, me.is_guest)
  }
}

#[component]
pub fn AddToCallDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let maybe_hub = use_context::<MeetingsHub>();
  // Decided once: the dialog doesn't change its kind while it is open.
  match untrack(|| AddPeople::for_call(state, maybe_hub, conversation)) {
    Some(AddPeople::Invite(meeting)) => view! { <InviteToMeetingDialog meeting=meeting /> }.into_any(),
    Some(AddPeople::ShareLink(meeting)) => {
      view! { <ShareMeetingLinkDialog meeting=meeting conversation=conversation /> }.into_any()
    },
    Some(AddPeople::MoveToMeeting) | None => {
      view! { <MoveCallDialog conversation=conversation /> }.into_any()
    },
  }
}

/// The organizer of the meeting adds people during its call: they become invitees (the meeting's
/// update reaches them, by mail too), which puts them into its chat and its encryption group, so they
/// can join the call from the meeting (`save_meeting_edit`, e2ee.md §4.6).
#[component]
fn InviteToMeetingDialog(meeting: MeetingId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let chosen = RwSignal::new(Vec::<User>::new());
  let invited = Signal::derive(move || {
    let mut people: HashSet<UserId> = hub.meetings.with(|all| {
      all
        .iter()
        .find(|found| found.id == meeting)
        .map(attending_users)
        .unwrap_or_default()
        .into_iter()
        .collect()
    });
    people.insert(state.me().id);
    people
  });
  let pending = PendingAction::new();
  let invite = move || {
    let picked: Vec<UserId> = chosen.with_untracked(|list| list.iter().map(|user| user.id).collect());
    if picked.is_empty() {
      return;
    }
    let me = state.me().id;
    let Some(before) = hub
      .meetings
      .with_untracked(|all| all.iter().find(|found| found.id == meeting).cloned())
    else {
      return;
    };
    let names = chosen.with_untracked(|list| {
      list
        .iter()
        .map(|user| user.display_name.clone())
        .collect::<Vec<_>>()
        .join(", ")
    });
    let done = i18n.t_with("call-invited", &[("names", &names)]);
    pending.spawn(async move {
      let request = adding_invitees(&before, me, &picked);
      if !save_meeting_edit(state, hub, me, before, request).await {
        return Err(ActionFailure::other("the meeting's invitees were not saved"));
      }
      let _ = state.overlay.try_set(crate::state::Overlay::None);
      if state.is_alive() {
        state.show_toast(done);
      }
      Ok(())
    });
  };
  view! {
    <Dialog title_key="call-add-people">
      <form on:submit=move |ev: leptos::ev::SubmitEvent| { ev.prevent_default(); invite(); }>
        <PeoplePicker
          chosen=chosen
          chosen_label_key="call-add-chosen"
          allows_guest_accounts=true
          already_in=invited
          already_in_key="call-already-invited"
          on_submit=Callback::new(move |()| invite())
        />
        <p class="hint">{move || i18n.t("call-invite-hint")}</p>
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
            {move || if pending.maybe_failure().is_some() { i18n.t("data-retry") } else { i18n.t("call-invite") }}
          </button>
        </footer>
      </form>
    </Dialog>
  }
}

/// Somebody else's meeting: only its organizer changes who is invited, so the dialog says that and
/// hands out the meeting's link. Members who open it ask to join, and the organizer lets them in.
#[component]
fn ShareMeetingLinkDialog(meeting: MeetingId, conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let hub = expect_context::<MeetingsHub>();
  let maybe_join_url = hub.meetings.with_untracked(|all| {
    all
      .iter()
      .find(|found| found.id == meeting)
      .map(|found| found.join_url.clone())
  });
  view! {
    <Dialog title_key="call-add-people">
      <p>{move || i18n.t("call-share-link-hint")}</p>
      <footer class="modal-foot">
        {maybe_join_url.map(|join_url| view! { <MeetingLinkButton meeting=meeting conversation=conversation join_url=join_url /> })}
        <span class="grow"></span>
        <button type="button" class="btn btn-primary" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
      </footer>
    </Dialog>
  }
}

/// A DM or group DM call: the call moves into a new meeting room with the added people.
#[component]
fn MoveCallDialog(conversation: ConversationId) -> impl IntoView {
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
/// added, from their encrypted announcement. The device follows by itself (`follow_moved_calls`);
/// the banner shows while it does, and stays with its button when following failed.
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
  // The notetaker as the verified policy names it (e2ee.md §8.2, "In every call").
  let maybe_notetaker =
    Memo::new(move |_| state.read(|backend| backend.notetaker_policy().and_then(|policy| policy.maybe_bot)));
  move || {
    let offer = call.move_offer.get().filter(|offer| !offer.is_requested_here)?;
    let text = if is_notetaker_move(&offer.added, maybe_notetaker.get()) {
      // Where the audio will go, before the tap that takes the user there.
      i18n.t_with("call-move-offer-notetaker", &[("name", &name_of(offer.by))])
    } else {
      let added = offer
        .added
        .iter()
        .map(|user| name_of(*user))
        .collect::<Vec<_>>()
        .join(", ");
      i18n.t_with(
        "call-move-offer",
        &[("name", &name_of(offer.by)), ("added", &added)],
      )
    };
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

/// The move added nobody but the notetaker: "Add notetaker" in a DM call.
fn is_notetaker_move(added: &[UserId], maybe_notetaker: Option<UserId>) -> bool {
  maybe_notetaker.is_some_and(|bot| added == [bot])
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

  /// #1141: a DM's call moves, a meeting's organizer invites, anybody else in a meeting shares its
  /// link; a guest and a channel's call add nobody.
  #[test]
  fn who_adds_people_how() {
    let (me, organizer): (UserId, UserId) = (id_from_raw(1), id_from_raw(2));
    let meeting: MeetingId = id_from_raw(9);
    assert_eq!(
      AddPeople::of(true, None, me, false),
      Some(AddPeople::MoveToMeeting)
    );
    assert_eq!(
      AddPeople::of(false, Some((meeting, me)), me, false),
      Some(AddPeople::Invite(meeting))
    );
    assert_eq!(
      AddPeople::of(false, Some((meeting, organizer)), me, false),
      Some(AddPeople::ShareLink(meeting))
    );
    assert_eq!(AddPeople::of(false, Some((meeting, organizer)), me, true), None);
    assert_eq!(AddPeople::of(true, None, me, true), None);
    assert_eq!(AddPeople::of(false, None, me, false), None, "a channel's call");
  }

  #[test]
  fn a_move_that_adds_only_the_notetaker_says_so() {
    let (bot, carol): (UserId, UserId) = (id_from_raw(9), id_from_raw(3));
    assert!(is_notetaker_move(&[bot], Some(bot)));
    assert!(!is_notetaker_move(&[bot, carol], Some(bot)), "people too");
    assert!(!is_notetaker_move(&[carol], Some(bot)));
    assert!(!is_notetaker_move(&[bot], None), "no notetaker in the policy");
  }

  #[test]
  fn nobody_new_moves_nothing() {
    let (me, bob): (UserId, UserId) = (id_from_raw(1), id_from_raw(2));
    let in_call = HashSet::from([me, bob]);
    assert_eq!(MovePlan::new(&in_call, &[bob, me], me), None);
    assert_eq!(MovePlan::new(&in_call, &[], me), None);
  }
}
