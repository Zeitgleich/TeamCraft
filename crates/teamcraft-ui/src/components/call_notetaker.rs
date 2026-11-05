//! The notetaker in the call screens (e2ee.md §8.2, "Everybody sees it"): a notice for as long as
//! it is in the call, which says where the audio goes and lets those who may send it away, and the
//! state behind the call screen's button and badge. "Add notetaker" also works where the notetaker
//! is no member of the conversation yet: it adds it first, as adding people does, and asks it in
//! once its membership commit is through, so the request is sent to a group it is in. Where the
//! user can't add it (a DM or group DM, which takes nobody more, a channel whose members the user
//! doesn't manage, a meeting the user doesn't organize), the call moves into a meeting room with
//! everybody in it and the notetaker, which is asked in there (e2ee.md §8.2, "In every call").

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{
  ConversationId, HistoryPolicy, IdempotencyKey, MeetingId, NotetakerRequest, NotetakerScope,
};

use crate::channel_access::ChannelAccess;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::meetings::MeetingsHub;
use crate::model::UserId;
use crate::notetaker::{NotetakerInCall, NotetakerRefusal, notetaker_in_call};
use crate::state::{AppState, use_app_state};

/// How long "joins in a moment" shows after asking, if the notetaker doesn't come.
const ASKED_NOTICE: Duration = Duration::from_secs(90);

/// How the user can add the notetaker to the conversation of the call, where it is no member yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddRoute {
  /// A channel of a space where the user manages members: the bot goes in as from the channel's
  /// integrations, into the space first (as a guest) where the user may put it there.
  Channel { joins_space: bool },
  /// A meeting the user organizes: the bot becomes one of its invitees, which puts it into the
  /// meeting chat's group (e2ee.md §4.6, "Meeting chats").
  Meeting(MeetingId),
  /// A conversation the user can't add the bot to: the call moves into a new meeting room with
  /// everybody in it and the bot (docs/meetings.md, "A call becomes a meeting"), where the core
  /// asks it in. The conversation keeps its members and history; the bot never sees them.
  MoveToRoom,
}

/// What the user can do in the conversation of the call, which decides the [`AddRoute`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConversationRights {
  /// A meeting the user organizes.
  OrganizesMeeting(MeetingId),
  /// A channel of a space where the user manages members.
  ManagesChannel { joins_space: bool },
  /// A DM or group DM (which takes nobody more), a channel whose members the user doesn't manage,
  /// or a meeting somebody else organizes: the user can't add anybody there.
  CannotAdd,
}

/// "Add notetaker" of one call: asks the notetaker in, adding it to the conversation first where
/// it is no member yet.
#[derive(Clone, Copy)]
pub struct NotetakerInvite {
  state: AppState,
  conversation: ConversationId,
  maybe_route: Memo<Option<AddRoute>>,
  maybe_hub: Option<MeetingsHub>,
  /// Who is in the call besides the user, for the room a DM call moves into.
  participants: Memo<Vec<UserId>>,
  i18n: I18n,
  /// One key per call screen: a retry after a lost answer gets the same room, not a second one.
  room_key: IdempotencyKey,
  pending: PendingAction,
  /// Asked in, until it is there or [`ASKED_NOTICE`] passed: the banner says it is coming.
  pub is_asked: RwSignal<bool>,
}

/// The notetaker of the verified policy in this call, with `participants` (the others), and the
/// "Add notetaker" that goes with it.
pub fn use_notetaker_in_call(
  conversation: ConversationId,
  participants: Memo<Vec<UserId>>,
) -> (Memo<Option<NotetakerInCall>>, NotetakerInvite) {
  let state = use_app_state();
  let maybe_hub = use_context::<MeetingsHub>();
  let maybe_route = use_add_route(state, conversation, maybe_hub);
  let notetaker = Memo::new(move |_| {
    let others = participants.get();
    let can_bring_in = maybe_route.get().is_some();
    state.read(|backend| {
      let policy = backend.notetaker_policy()?;
      let me = backend.me();
      let member_ids = backend
        .conversation(conversation)
        .map(|conversation| conversation.member_ids)
        .unwrap_or_default();
      notetaker_in_call(
        &policy,
        !me.is_guest && !me.is_bot,
        backend.can_manage_integration_sources(),
        &member_ids,
        &others,
        can_bring_in,
      )
    })
  });
  let is_asked = RwSignal::new(false);
  // Asked in from the DM call this one moved from: "joins in a moment" goes on here.
  let asked_in = state.call.notetaker_asked_in;
  Effect::new(move |_| {
    if asked_in.get() == Some(conversation) {
      asked_in.set(None);
      show_asked(is_asked);
    }
  });
  let invite = NotetakerInvite {
    state,
    conversation,
    maybe_route,
    maybe_hub,
    participants,
    i18n: use_i18n(),
    room_key: IdempotencyKey::random(),
    pending: PendingAction::new(),
    is_asked,
  };
  (notetaker, invite)
}

impl NotetakerInvite {
  /// Adding or asking runs, or the notetaker was asked in and is on its way (reactive).
  pub fn is_busy(&self) -> bool {
    self.is_asked.get() || self.pending.is_pending()
  }

  /// Asks `notetaker` in; where it is no member, adds it first and asks once that went through.
  /// Asking before its membership commit would send the request to a group it can't read.
  pub fn press(&self, notetaker: &NotetakerInCall) {
    let Self {
      state,
      conversation,
      is_asked,
      ..
    } = *self;
    if notetaker.is_member {
      ask_in(state, conversation, is_asked);
      return;
    }
    let bot = notetaker.bot;
    match self.maybe_route.get_untracked() {
      Some(AddRoute::Channel { joins_space }) => self.pending.run_and_then(
        || {
          state.write(|backend| {
            // A bot never reads what was written before it was added.
            backend.add_people_to_channel(
              conversation,
              vec![(bot, true)],
              HistoryPolicy::FromJoin,
              joins_space,
            )
          })
        },
        move || ask_in(state, conversation, is_asked),
      ),
      Some(AddRoute::Meeting(meeting)) => {
        let Some(hub) = self.maybe_hub else {
          return;
        };
        is_asked.set(true);
        spawn_local(async move {
          if invite_to_meeting(state, hub, meeting, bot).await {
            ask_in(state, conversation, is_asked);
          } else {
            let _ = is_asked.try_set(false);
          }
        });
      },
      Some(AddRoute::MoveToRoom) => self.move_to_room(bot),
      None => {},
    }
  }

  /// The call moves into a new meeting room with everybody in it and `bot`, as "Add people" does
  /// in a DM call; the core asks the bot in there once the move went through, and this device
  /// follows at once (the others with a tap). The notice goes on in the room's call screen.
  fn move_to_room(&self, bot: UserId) {
    let Self {
      state,
      conversation,
      maybe_hub,
      participants,
      i18n,
      room_key,
      ..
    } = *self;
    let me = state.me().id;
    let in_call = people_in_call(&participants.get_untracked(), me);
    let title = i18n.t("call-notetaker-room-title");
    self.pending.run_and_then(
      || {
        state.write(|backend| {
          backend.add_people_to_call(conversation, in_call, vec![bot], Some(title), room_key)
        })
      },
      move || {
        // The call screen shows the host controls of the new meeting.
        if let Some(hub) = maybe_hub {
          spawn_local(async move { hub.refresh(me, now_ms()).await });
        }
      },
    );
  }
}

/// Everybody in the call, the user included, sorted: who the room the call moves into is for (the
/// core keeps only members of the conversation's group).
fn people_in_call(others: &[UserId], me: UserId) -> Vec<UserId> {
  let mut people: Vec<UserId> = others.iter().copied().chain([me]).collect();
  people.sort_unstable();
  people.dedup();
  people
}

/// Where the user can bring the notetaker in, reactive (see [`add_route`]).
fn use_add_route(
  state: AppState,
  conversation: ConversationId,
  maybe_hub: Option<MeetingsHub>,
) -> Memo<Option<AddRoute>> {
  let access = ChannelAccess::new(state, conversation);
  Memo::new(move |_| {
    let me = state.me().id;
    let rights = if let Some(meeting) = maybe_hub.and_then(|hub| hub.hosted_meeting_for(conversation, me)) {
      ConversationRights::OrganizesMeeting(meeting)
    } else {
      match state.read(|backend| backend.conversation(conversation)) {
        Some(found) if found.has_channel_settings() && access.can_add_members() => {
          let joins_space = state
            .read(|backend| backend.spaces())
            .into_iter()
            .find(|space| Some(space.id) == found.maybe_space)
            .is_some_and(|space| space.can_add_members || space.can_invite_guests);
          ConversationRights::ManagesChannel { joins_space }
        },
        _ => ConversationRights::CannotAdd,
      }
    };
    // A call move needs the server's real calls; the demo has none.
    let can_move = maybe_hub.is_some() && state.read(|backend| !backend.has_simulated_calls());
    add_route(rights, can_move)
  })
}

/// Where the user can bring the notetaker in: a meeting the user organizes (as an invitee), a
/// channel of a space where the user manages members (as from its integrations). Anywhere else
/// (a DM or group DM, a channel whose members somebody else manages, a meeting somebody else
/// organizes) its call moves into a meeting room with the notetaker, `can_move`: the app can move
/// calls; the call stays where it is for those who don't follow (e2ee.md §8.2, "In every call").
fn add_route(rights: ConversationRights, can_move: bool) -> Option<AddRoute> {
  match rights {
    ConversationRights::OrganizesMeeting(meeting) => Some(AddRoute::Meeting(meeting)),
    ConversationRights::ManagesChannel { joins_space } => Some(AddRoute::Channel { joins_space }),
    ConversationRights::CannotAdd => can_move.then_some(AddRoute::MoveToRoom),
  }
}

/// Adds `bot` to the invitees of `meeting` (which the user organizes) and the meeting chat's
/// group. Whether that went through.
async fn invite_to_meeting(state: AppState, hub: MeetingsHub, meeting: MeetingId, bot: UserId) -> bool {
  let Some(me) = state.try_read(|backend| backend.me().id) else {
    return false;
  };
  let Some(before) = hub
    .meetings
    .try_with_untracked(|all| all.iter().find(|found| found.id == meeting).cloned())
    .flatten()
  else {
    return false;
  };
  let request = crate::meetings::adding_invitee(&before, me, bot);
  crate::components::meetings_view::save_meeting_edit(state, hub, me, before, request).await
}

/// Asks the notetaker into the call; the banner says it is coming until it is there. Also after
/// the call screen is gone (an addition that finished late): the request stands.
fn ask_in(state: AppState, conversation: ConversationId, is_asked: RwSignal<bool>) {
  state.try_write(|backend| {
    backend.request_notetaker(
      conversation,
      NotetakerRequest::Join {
        scope: NotetakerScope::ThisCall,
      },
    )
  });
  show_asked(is_asked);
}

/// "Joins in a moment" until the notetaker is there or [`ASKED_NOTICE`] passed.
fn show_asked(is_asked: RwSignal<bool>) {
  let _ = is_asked.try_set(true);
  set_timeout(
    move || {
      // The screen may be gone; then there is nothing to reset.
      let _ = is_asked.try_set(false);
    },
    ASKED_NOTICE,
  );
}

/// The i18n key of why "Add notetaker" is disabled (issue #1290).
pub fn notetaker_refusal_key(refusal: NotetakerRefusal) -> &'static str {
  match refusal {
    NotetakerRefusal::Guest => "call-notetaker-refused-guest",
    NotetakerRefusal::AdminsOnly => "call-notetaker-refused-admins",
  }
}

/// The notice while the notetaker is in the call (for everybody, also who joins later), or while
/// it was asked in and is on its way.
#[component]
pub fn NotetakerBanner(
  conversation: ConversationId,
  notetaker: Memo<Option<NotetakerInCall>>,
  is_asked: RwSignal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  Effect::new(move |_| {
    if notetaker.get().is_some_and(|notetaker| notetaker.is_present) && is_asked.get_untracked() {
      is_asked.set(false);
    }
  });
  let send_away =
    move |_| state.write(|backend| backend.request_notetaker(conversation, NotetakerRequest::Stop));
  move || {
    let notetaker = notetaker.get()?;
    if notetaker.is_present {
      let text = i18n.t_with(
        "call-notetaker-banner",
        &[("model", &notetaker.transcription_model)],
      );
      return Some(
        view! {
          <div class="call-notetaker-banner" role="status">
            <Icon name=IconName::Compose />
            <p>{text}</p>
            {notetaker.can_stop.then(|| view! {
              <button type="button" class="btn btn-small" on:click=send_away>{i18n.t("call-notetaker-remove")}</button>
            })}
          </div>
        }
        .into_any(),
      );
    }
    is_asked.get().then(|| {
      view! {
        <div class="call-notetaker-banner" role="status">
          <Icon name=IconName::Compose />
          <p>{i18n.t("call-notetaker-joining")}</p>
        </div>
      }
      .into_any()
    })
  }
}

/// The pre-join screen's notice when the running call has the notetaker in it.
#[component]
pub fn NotetakerPreJoinNotice(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_present = Memo::new(move |_| {
    state.read(|backend| {
      let Some(bot) = backend.notetaker_policy().and_then(|policy| policy.maybe_bot) else {
        return false;
      };
      backend
        .call_participants(conversation)
        .iter()
        .any(|participant| participant.user == bot)
    })
  });
  view! {
    <Show when=move || is_present.get() fallback=|| ()>
      <p class="call-notetaker-banner prejoin-notetaker" role="status">
        <Icon name=IconName::Compose />
        <span>{move || i18n.t("prejoin-notetaker")}</span>
      </p>
    </Show>
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{NotetakerInviters, NotetakerPolicy};

  use super::*;
  use crate::model::id_from_raw;
  use crate::notetaker::notetaker_in_call;

  /// #1271, #1274: a meeting the user organizes and a channel the user manages bring the
  /// notetaker in by adding it; every other call (a DM or group DM, a channel somebody else
  /// manages, a meeting somebody else organizes) by moving into a room with it.
  #[test]
  fn the_route_follows_what_the_user_can_do_in_the_conversation() {
    let meeting: MeetingId = id_from_raw(7);
    assert_eq!(
      add_route(ConversationRights::CannotAdd, true),
      Some(AddRoute::MoveToRoom)
    );
    assert_eq!(
      add_route(ConversationRights::CannotAdd, false),
      None,
      "the demo moves no calls"
    );
    assert_eq!(
      add_route(ConversationRights::OrganizesMeeting(meeting), true),
      Some(AddRoute::Meeting(meeting))
    );
    assert_eq!(
      add_route(ConversationRights::ManagesChannel { joins_space: true }, true),
      Some(AddRoute::Channel { joins_space: true })
    );
  }

  /// "Add notetaker" in a 1:1 or group DM call, also one that runs already: offered to whoever
  /// the policy lets ask it in, refused to anybody else.
  #[test]
  fn in_a_dm_call_the_policy_alone_decides() {
    let (me, bob, bot): (UserId, UserId, UserId) = (id_from_raw(1), id_from_raw(2), id_from_raw(9));
    let dm_members = [me, bob];
    let can_bring_in = add_route(ConversationRights::CannotAdd, true).is_some();
    let members = policy(bot, NotetakerInviters::Members);
    let offered = notetaker_in_call(&members, true, false, &dm_members, &[bob], can_bring_in).unwrap();
    assert!(offered.can_invite && !offered.is_member);

    let admins = policy(bot, NotetakerInviters::Admins);
    let refused = notetaker_in_call(&admins, true, false, &dm_members, &[bob], can_bring_in).unwrap();
    assert!(!refused.can_invite, "not an admin");
    let admin = notetaker_in_call(&admins, true, true, &dm_members, &[bob], can_bring_in).unwrap();
    assert!(admin.can_invite);
    let guest = notetaker_in_call(&members, false, false, &dm_members, &[bob], can_bring_in).unwrap();
    assert!(!guest.can_invite, "guests ask nobody in");
  }

  /// #1274: in the call of a channel whose members the user doesn't manage, or of a meeting the
  /// user doesn't organize, "Add notetaker" is offered to whoever the policy lets ask it in (the
  /// call moves into a room), also mid-call, and refused to anybody else.
  #[test]
  fn in_a_channel_or_meeting_the_user_cannot_add_to_the_policy_alone_decides() {
    let (me, bob, carol, bot): (UserId, UserId, UserId, UserId) =
      (id_from_raw(1), id_from_raw(2), id_from_raw(3), id_from_raw(9));
    // The channel's or meeting chat's members, without the notetaker; Bob and Carol in the call.
    let conversation_members = [me, bob, carol];
    let route = add_route(ConversationRights::CannotAdd, true);
    assert_eq!(route, Some(AddRoute::MoveToRoom));
    let can_bring_in = route.is_some();
    let in_call = [bob, carol];

    let members = policy(bot, NotetakerInviters::Members);
    let offered = notetaker_in_call(
      &members,
      true,
      false,
      &conversation_members,
      &in_call,
      can_bring_in,
    )
    .unwrap();
    assert!(offered.can_invite && !offered.is_member);
    let admins = policy(bot, NotetakerInviters::Admins);
    let refused = notetaker_in_call(
      &admins,
      true,
      false,
      &conversation_members,
      &in_call,
      can_bring_in,
    )
    .unwrap();
    assert!(!refused.can_invite, "not an admin");
    let guest = notetaker_in_call(
      &members,
      false,
      false,
      &conversation_members,
      &in_call,
      can_bring_in,
    )
    .unwrap();
    assert!(!guest.can_invite, "guests ask nobody in");
    let present = notetaker_in_call(
      &members,
      true,
      false,
      &conversation_members,
      &[bob, bot],
      can_bring_in,
    )
    .unwrap();
    assert!(
      !present.can_invite && present.is_present,
      "it is in the call already"
    );
  }

  #[test]
  fn the_room_is_for_everybody_in_the_call() {
    let (me, bob, carol): (UserId, UserId, UserId) = (id_from_raw(1), id_from_raw(2), id_from_raw(3));
    assert_eq!(people_in_call(&[carol, bob, me], me), vec![me, bob, carol]);
    assert_eq!(people_in_call(&[], me), vec![me]);
  }

  fn policy(bot: UserId, inviters: NotetakerInviters) -> NotetakerPolicy {
    NotetakerPolicy {
      is_enabled: true,
      maybe_bot: Some(bot),
      inviters,
      ..NotetakerPolicy::default()
    }
  }
}
