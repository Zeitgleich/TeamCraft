use leptos::prelude::*;

use crate::areas::{self, Place, lazy_area};
use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::state::use_app_state;

/// Strip under the channel header: a running huddle to join, or the controls of the user's own call.
#[component]
pub fn HuddleBar(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let call = state.call;
  let is_shown = Memo::new(move |_| {
    call.is_in_call_for(conversation) || state.read(|backend| backend.huddle(conversation)).is_some()
  });
  // The bar loads with the first call it shows (`areas::huddle_bar`), next to the call screens.
  move || {
    is_shown
      .get()
      .then(|| lazy_area(Place::Inline, move || areas::huddle_bar(conversation)))
  }
}

/// The bar itself, while a call goes on in the conversation or this account is in it.
#[component]
pub fn HuddleBarContent(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let huddle = Memo::new(move |_| state.read(|backend| backend.huddle(conversation)));
  let is_in_call = Memo::new(move |_| call.is_in_call_for(conversation));
  let members = Memo::new(move |_| {
    huddle
      .get()
      .map(|info| {
        state.read(|backend| {
          info
            .participants
            .iter()
            .filter_map(|id| backend.user(*id))
            .collect::<Vec<_>>()
        })
      })
      .unwrap_or_default()
  });
  // Only for another device of the user (issue #976): this device's own entry, still listed for a
  // moment after it left, is none.
  let is_on_other_device = Memo::new(move |_| {
    let me = state.me().id;
    !is_in_call.get() && huddle.get().is_some_and(|info| info.is_on_other_device(me))
  });
  let join_alongside = move |_| {
    call.is_camera_on.set(false);
    call.join_alongside(CallRequest {
      conversation,
      kind: CallKind::Huddle,
    });
  };
  let take_over = move |_| {
    call.is_camera_on.set(false);
    call.join_and_take_over(CallRequest {
      conversation,
      kind: CallKind::Huddle,
    });
  };
  let join = move |_| {
    call.is_camera_on.set(false);
    call.join(CallRequest {
      conversation,
      kind: CallKind::Huddle,
    });
  };
  // Also clears an entry only the server still lists (a device that left without it hearing).
  let leaving_everywhere = PendingAction::new();
  let leave_everywhere = move |_| {
    leaving_everywhere.run(|| state.write(|backend| backend.leave_call_everywhere(conversation)));
  };

  view! {
    <Show when=move || is_in_call.get() || huddle.get().is_some() fallback=|| ()>
      <div
        class="huddle-bar"
        class:huddle-bar-choices=move || is_on_other_device.get()
        role="region"
        aria-label=move || i18n.t("huddle-bar-label")
      >
        <Icon name=IconName::Headphones class="huddle-icon" />
        <div class="huddle-info">
          <strong>
            {move || if is_on_other_device.get() { i18n.t("huddle-on-other-device") } else { i18n.t("huddle-in-progress") }}
          </strong>
          <span class="avatar-stack">
            {move || {
              members.get().into_iter().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> }).collect_view()
            }}
          </span>
          <span class="huddle-names" translate="no">
            {move || members.get().into_iter().map(|user| user.display_name).collect::<Vec<_>>().join(", ")}
          </span>
        </div>
        <Show
          when=move || is_in_call.get()
          fallback=move || {
            view! {
              <Show
                when=move || is_on_other_device.get()
                fallback=move || {
                  view! {
                    <button class="btn btn-primary btn-small" on:click=join>
                      {move || i18n.t("huddle-join")}
                    </button>
                  }
                }
              >
                <div class="huddle-controls" role="group" aria-label=move || i18n.t("huddle-on-other-device")>
                  <PendingButton pending=leaving_everywhere class="btn btn-small btn-quiet" on_click=leave_everywhere>
                    {move || i18n.t("huddle-leave-everywhere")}
                  </PendingButton>
                  <button class="btn btn-small" on:click=join_alongside>
                    {move || i18n.t("huddle-join-here")}
                  </button>
                  <button class="btn btn-primary btn-small" on:click=take_over>
                    {move || i18n.t("huddle-move-here")}
                  </button>
                </div>
              </Show>
            }
          }
        >
          <div class="huddle-controls" role="group" aria-label=move || i18n.t("call-controls-label")>
            <button
              class="icon-btn"
              aria-pressed=move || (!call.is_mic_on.get()).to_string()
              aria-label=move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") }
              on:click=move |_| call.is_mic_on.update(|on| *on = !*on)
            >
              <Icon name=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff }) />
            </button>
            <button
              class="icon-btn"
              aria-label=move || i18n.t("call-open")
              title=move || i18n.t("call-open")
              on:click=move |_| call.is_minimized.set(false)
            >
              <Icon name=IconName::Maximize />
            </button>
            <button class="btn btn-danger btn-small" on:click=move |_| call.leave()>
              {move || i18n.t("huddle-leave")}
            </button>
          </div>
        </Show>
      </div>
    </Show>
  }
}

/// Compact controls of the running call, shown in the sidebar while the call is minimized.
#[component]
pub fn CallPill() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let name = Memo::new(move |_| {
    call
      .maybe_request
      .get()
      .and_then(|request| state.read(|backend| backend.conversation(request.conversation)))
      .map(|conv| conv.name)
      .unwrap_or_default()
  });
  let is_visible = move || call.phase.get() == CallPhase::Active && call.is_minimized.get();

  view! {
    <Show when=is_visible fallback=|| ()>
      <div class="call-pill" role="region" aria-label=move || i18n.t("call-pill-label")>
        <span class="call-pill-dot" aria-hidden="true"></span>
        <button
          class="call-pill-name"
          on:click=move |_| {
            if let Some(request) = call.maybe_request.get_untracked() {
              state.open_conversation(request.conversation);
            }
            call.is_minimized.set(false);
          }
          title=move || i18n.t("call-open")
        >
          <span class="small">{move || i18n.t("call-in-progress")}</span>
          <span translate="no">{move || format!("# {}", name.get())}</span>
        </button>
        <button
          class="icon-btn on-dark"
          aria-pressed=move || (!call.is_mic_on.get()).to_string()
          aria-label=move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") }
          on:click=move |_| call.is_mic_on.update(|on| *on = !*on)
        >
          <Icon name=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff }) />
        </button>
        <button
          class="icon-btn on-dark leave"
          aria-label=move || i18n.t("call-leave")
          title=move || i18n.t("call-leave")
          on:click=move |_| call.leave()
        >
          <Icon name=IconName::Phone class="hangup" />
        </button>
      </div>
    </Show>
  }
}
