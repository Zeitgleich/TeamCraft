use leptos::prelude::*;

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::clock::format_time;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::media_layout::format_duration;
use crate::model::{CallTile, CallTileState, ConversationKind, Message};
use crate::state::use_app_state;

/// A call in the conversation (docs/notifications.md, "Calls in the timeline"): a tile at its start with
/// who started it and how it stands; a call that goes on offers to join while this device isn't in
/// it. A channel's call is a huddle, as its bar names it.
#[component]
pub fn CallTileRow(message: Message, tile: CallTile) -> impl IntoView {
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let state = use_app_state();
  let call = state.call;
  let conversation = message.conversation;
  let message_id = message.id;
  let maybe_conversation = state.read(|backend| backend.conversation(conversation));
  let kind = match maybe_conversation
    .as_ref()
    .map(|info| (info.kind, crate::meetings::is_meeting_chat(info)))
  {
    Some((ConversationKind::Direct, _)) => CallKind::Direct,
    Some((ConversationKind::Group, _) | (_, true)) => CallKind::Group,
    _ => CallKind::Huddle,
  };
  let starter = state
    .read(|backend| backend.user(message.author))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let title_key = match (tile.is_missed, kind) {
    (true, _) => "call-tile-missed",
    (false, CallKind::Huddle) => "call-tile-huddle",
    (false, _) => "call-tile-call",
  };
  let is_ongoing = tile.state == CallTileState::Ongoing;
  let state_text = match tile.state {
    CallTileState::Ongoing => i18n.t("call-tile-ongoing"),
    CallTileState::Ended {
      maybe_duration_ms: Some(duration_ms),
    } => format_duration(duration_ms),
    CallTileState::Ended {
      maybe_duration_ms: None,
    } => i18n.t("call-tile-ended"),
  };
  let detail = if starter.is_empty() {
    state_text
  } else {
    format!("{starter} · {state_text}")
  };
  let time = format_time(message.sent_at, locale);
  let title = i18n.t(title_key);
  let aria_label = format!("{title}, {detail}, {time}");
  let has_calls = state.read(|backend| backend.has_calls());
  // Join only while the call still runs here and this device isn't in it.
  let can_join = Memo::new(move |_| {
    is_ongoing
      && !call.is_in_call_for(conversation)
      && state.read(|backend| backend.huddle(conversation)).is_some()
  });
  let join = move |_| {
    call.is_camera_on.set(false);
    call.join(CallRequest { conversation, kind });
  };
  view! {
    <div
      class="msg msg-call"
      class:msg-highlight=move || state.highlighted.get() == Some(message_id)
      tabindex="-1"
      data-nav="row"
      data-msg-id=message_id.to_string()
      aria-label=aria_label
    >
      <div class="call-tile" class:is-ongoing=is_ongoing class:is-missed=tile.is_missed>
        <span class="call-tile-icon" aria-hidden="true">
          <Icon name=if kind == CallKind::Direct { IconName::Phone } else { IconName::Video } />
        </span>
        <span class="call-tile-text">
          <strong class="call-tile-title">{title}</strong>
          <span class="call-tile-detail" translate="no">{detail}</span>
        </span>
        <Show when=move || can_join.get() fallback=|| ()>
          <button
            class="btn btn-primary btn-small call-tile-join"
            disabled=move || !has_calls || call.phase.get() != CallPhase::Idle
            on:click=join
          >
            {move || i18n.t("call-tile-join")}
          </button>
        </Show>
        <time class="call-tile-time" datetime=message.sent_at.to_string()>{time}</time>
      </div>
    </div>
  }
}
