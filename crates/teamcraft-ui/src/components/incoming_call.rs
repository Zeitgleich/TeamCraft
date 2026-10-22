use leptos::prelude::*;

use crate::calls::{CallKind, CallRequest};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::state::use_app_state;

/// Ringing card for an incoming call. It does not take focus from what the user is typing; screen
/// readers hear it through the assertive live region.
#[component]
pub fn IncomingCallDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;

  move || {
    let incoming = call.incoming.get()?;
    let caller = state.read(|backend| backend.user(incoming.caller))?;
    let conversation = state.read(|backend| backend.conversation(incoming.conversation));
    let kind = match conversation.as_ref().map(|conv| conv.kind) {
      Some(ConversationKind::Direct) | None => CallKind::Direct,
      Some(_) => CallKind::Group,
    };
    let request = CallRequest {
      conversation: incoming.conversation,
      kind,
    };
    let caller_name = caller.display_name.clone();
    let text = if incoming.with_video {
      i18n.t_with("incoming-video", &[("name", &caller_name)])
    } else {
      i18n.t_with("incoming-audio", &[("name", &caller_name)])
    };
    let accept = move |with_video: bool| {
      call.incoming.set(None);
      call.is_camera_on.set(with_video);
      state.open_conversation(request.conversation);
      call.join(request);
    };

    Some(view! {
      <div class="incoming" role="alertdialog" aria-labelledby="incoming-title" aria-describedby="incoming-text">
        <div class="incoming-ring" aria-hidden="true">
          <Avatar user=caller size=AvatarSize::Large />
        </div>
        <div class="incoming-body">
          <h2 id="incoming-title">{move || i18n.t("incoming-title")}</h2>
          <p id="incoming-text" aria-live="assertive" translate="no">
            {text}
          </p>
        </div>
        <div class="incoming-actions">
          <button class="round-btn decline" aria-label=move || i18n.t("incoming-decline") title=move || i18n.t("incoming-decline") on:click=move |_| call.incoming.set(None)>
            <Icon name=IconName::Phone class="hangup" />
          </button>
          <button class="round-btn accept" data-accept aria-label=move || i18n.t("incoming-accept-audio") title=move || i18n.t("incoming-accept-audio") on:click=move |_| accept(false)>
            <Icon name=IconName::Phone />
          </button>
          <button class="round-btn accept" aria-label=move || i18n.t("incoming-accept-video") title=move || i18n.t("incoming-accept-video") on:click=move |_| accept(true)>
            <Icon name=IconName::Video />
          </button>
        </div>
      </div>
    })
  }
}
