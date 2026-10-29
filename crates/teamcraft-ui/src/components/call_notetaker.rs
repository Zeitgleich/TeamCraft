//! The notetaker in the call screens (e2ee.md §8.2, "Everybody sees it"): a notice for as long as
//! it is in the call, which says where the audio goes and lets those who may send it away, and the
//! state behind the call screen's button and badge.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_proto::{ConversationId, NotetakerRequest, NotetakerScope};

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::UserId;
use crate::notetaker::{NotetakerInCall, notetaker_in_call};
use crate::state::use_app_state;

/// How long "joins in a moment" shows after asking, if the notetaker doesn't come.
const ASKED_NOTICE: Duration = Duration::from_secs(90);

/// The notetaker of the verified policy in this call, with `participants` (the others).
pub fn use_notetaker_in_call(
  conversation: ConversationId,
  participants: Memo<Vec<UserId>>,
) -> Memo<Option<NotetakerInCall>> {
  let state = use_app_state();
  Memo::new(move |_| {
    let others = participants.get();
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
      )
    })
  })
}

/// Asks the notetaker into the call; the banner says it is coming until it is there.
pub fn ask_notetaker_in(conversation: ConversationId, is_asked: RwSignal<bool>) {
  let state = use_app_state();
  state.write(|backend| {
    backend.request_notetaker(
      conversation,
      NotetakerRequest::Join {
        scope: NotetakerScope::ThisCall,
      },
    )
  });
  is_asked.set(true);
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
