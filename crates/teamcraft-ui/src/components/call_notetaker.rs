//! The notetaker in the call screens (e2ee.md §8.2, "Everybody sees it"): a notice for as long as
//! it is in the call, which says where the audio goes and lets those who may send it away, and the
//! state behind the call screen's button and badge. "Add notetaker" also works where the notetaker
//! is no member of the conversation yet: it adds it first, as adding people does, and asks it in
//! once its membership commit is through, so the request is sent to a group it is in.

use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{ConversationId, HistoryPolicy, MeetingId, NotetakerRequest, NotetakerScope};

use crate::channel_access::ChannelAccess;
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::meetings::MeetingsHub;
use crate::model::UserId;
use crate::notetaker::{NotetakerInCall, notetaker_in_call};
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
}

/// "Add notetaker" of one call: asks the notetaker in, adding it to the conversation first where
/// it is no member yet.
#[derive(Clone, Copy)]
pub struct NotetakerInvite {
  state: AppState,
  conversation: ConversationId,
  maybe_route: Memo<Option<AddRoute>>,
  maybe_hub: Option<MeetingsHub>,
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
    let can_add_members = maybe_route.get().is_some();
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
        can_add_members,
      )
    })
  });
  let invite = NotetakerInvite {
    state,
    conversation,
    maybe_route,
    maybe_hub,
    pending: PendingAction::new(),
    is_asked: RwSignal::new(false),
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
      None => {},
    }
  }
}

/// Where the user can add the notetaker, reactive: a meeting the user organizes, or a channel of a
/// space where the user manages members. Nowhere else: a DM takes nobody more (its call moves to a
/// meeting room for that), and only the organizer changes a meeting's invitees.
fn use_add_route(
  state: AppState,
  conversation: ConversationId,
  maybe_hub: Option<MeetingsHub>,
) -> Memo<Option<AddRoute>> {
  let access = ChannelAccess::new(state, conversation);
  Memo::new(move |_| {
    let me = state.me().id;
    if let Some(meeting) = maybe_hub.and_then(|hub| hub.hosted_meeting_for(conversation, me)) {
      return Some(AddRoute::Meeting(meeting));
    }
    let space = state
      .read(|backend| backend.conversation(conversation))
      .filter(|conversation| conversation.has_channel_settings())?
      .maybe_space?;
    access.can_add_members().then(|| {
      let joins_space = state
        .read(|backend| backend.spaces())
        .into_iter()
        .find(|found| found.id == space)
        .is_some_and(|found| found.can_add_members || found.can_invite_guests);
      AddRoute::Channel { joins_space }
    })
  })
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
  let _ = is_asked.try_set(true);
  set_timeout(
    move || {
      // The screen may be gone; then there is nothing to reset.
      let _ = is_asked.try_set(false);
    },
    ASKED_NOTICE,
  );
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
