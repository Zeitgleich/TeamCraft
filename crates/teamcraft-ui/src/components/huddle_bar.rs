use leptos::prelude::*;

use crate::areas::{self, Place, lazy_area};
use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::call_prejoin::call_heading;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind};
use crate::state::{MainView, use_app_state};

/// How the bar words and joins the call of a conversation: a channel has huddles; a DM or group DM
/// has calls (#1211), joined with the kind the header's call buttons use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BarKind {
  kind: CallKind,
  /// Wording keys: running, join, leave, and the bar's label.
  in_progress: &'static str,
  join: &'static str,
  leave: &'static str,
  label: &'static str,
}

fn bar_kind(conversation: Option<ConversationKind>) -> BarKind {
  let kind = match conversation {
    Some(ConversationKind::Direct) => CallKind::Direct,
    Some(ConversationKind::Group) => CallKind::Group,
    _ => CallKind::Huddle,
  };
  if kind == CallKind::Huddle {
    BarKind {
      kind,
      in_progress: "huddle-in-progress",
      join: "huddle-join",
      leave: "huddle-leave",
      label: "huddle-bar-label",
    }
  } else {
    BarKind {
      kind,
      in_progress: "call-bar-in-progress",
      join: "call-bar-join",
      leave: "call-bar-leave",
      label: "call-bar-label",
    }
  }
}

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
  let bar = Memo::new(move |_| {
    bar_kind(
      state
        .read(|backend| backend.conversation(conversation))
        .map(|info| info.kind),
    )
  });
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
      kind: bar.get_untracked().kind,
    });
  };
  let take_over = move |_| {
    call.is_camera_on.set(false);
    call.join_and_take_over(CallRequest {
      conversation,
      kind: bar.get_untracked().kind,
    });
  };
  let join = move |_| {
    call.is_camera_on.set(false);
    call.join(CallRequest {
      conversation,
      kind: bar.get_untracked().kind,
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
        aria-label=move || i18n.t(bar.get().label)
      >
        <Icon name=IconName::Headphones class="huddle-icon" />
        <div class="huddle-info">
          <strong>
            {move || if is_on_other_device.get() { i18n.t("huddle-on-other-device") } else { i18n.t(bar.get().in_progress) }}
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
                      {move || i18n.t(bar.get().join)}
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
            <MicButton class="icon-btn" />
            <CameraButton class="icon-btn" />
            <button
              class="icon-btn"
              aria-label=move || i18n.t("call-open")
              title=move || i18n.t("call-open")
              on:click=move |_| call.is_minimized.set(false)
            >
              <Icon name=IconName::Maximize />
            </button>
            <button class="btn btn-danger btn-small" on:click=move |_| call.leave()>
              {move || i18n.t(bar.get().leave)}
            </button>
          </div>
        </Show>
      </div>
    </Show>
  }
}

/// The bar of a minimized call (#1141), above the app in every view and layout: the rest of the
/// app stays usable while the call runs. It loads with the call screens (`areas::call_bar`). Not
/// over the call's own conversation, whose huddle bar has the same controls.
#[component]
pub fn MinimizedCallBar() -> impl IntoView {
  let state = use_app_state();
  let call = state.call;
  let is_shown = Memo::new(move |_| {
    call.phase.get() == CallPhase::Active
      && call.is_minimized.get()
      && call
        .maybe_request
        .get()
        .is_some_and(|request| state.view.get() != MainView::Conversation(request.conversation))
  });
  move || {
    is_shown
      .get()
      .then(|| lazy_area(Place::Inline, || areas::call_bar(())))
  }
}

/// The minimized call's bar: what call it is, the microphone and the camera, back to the call and
/// leaving it.
#[component]
pub fn CallBar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let maybe_hub = use_context::<crate::meetings::MeetingsHub>();
  let title = Memo::new(move |_| {
    let Some(request) = call.maybe_request.get() else {
      return String::new();
    };
    let name = state
      .read(|backend| backend.conversation(request.conversation))
      .map(|conversation| conversation.name)
      .unwrap_or_default();
    let maybe_meeting_title =
      crate::meetings::MeetingsHub::heading_title(maybe_hub, request.conversation, &name);
    call_heading(i18n, request.kind, maybe_meeting_title.as_deref(), &name)
  });
  let people = Memo::new(move |_| {
    call.maybe_request.get().map_or(1, |request| {
      state.read(|backend| backend.call_participants(request.conversation).len()) + 1
    })
  });
  let status = move || {
    if call.has_ended.get() {
      i18n.t("call-ended")
    } else {
      format!(
        "{} · {}",
        i18n.t("call-in-progress"),
        i18n.t_count("call-participants", people.get())
      )
    }
  };

  view! {
    <div class="call-bar" role="region" aria-label=move || i18n.t("call-pill-label")>
      <span class="call-bar-dot" aria-hidden="true"></span>
      <button class="call-bar-open" title=move || i18n.t("call-open") on:click=move |_| call.is_minimized.set(false)>
        <strong translate="no">{move || title.get()}</strong>
        <span class="small" role="status">{status}</span>
      </button>
      <div class="call-bar-controls" role="group" aria-label=move || i18n.t("call-controls-label")>
        <MicButton class="icon-btn on-dark" />
        <CameraButton class="icon-btn on-dark" />
        <button
          class="icon-btn on-dark"
          aria-label=move || i18n.t("call-open")
          title=move || i18n.t("call-open")
          on:click=move |_| call.is_minimized.set(false)
        >
          <Icon name=IconName::Maximize />
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
    </div>
  }
}

/// Mutes and unmutes the call's microphone (in the call too, not only the icon).
#[component]
fn MicButton(class: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let label = move || {
    if call.is_mic_on.get() {
      i18n.t("control-mic-mute")
    } else {
      i18n.t("control-mic-unmute")
    }
  };
  view! {
    <button
      class=class
      aria-pressed=move || (!call.is_mic_on.get()).to_string()
      aria-label=label
      title=label
      on:click=move |_| call.set_muted(call.is_mic_on.get_untracked())
    >
      <Icon name=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff }) />
    </button>
  }
}

/// Turns the call's camera on and off.
#[component]
fn CameraButton(class: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let label = move || {
    if call.is_camera_on.get() {
      i18n.t("control-camera-off")
    } else {
      i18n.t("control-camera-on")
    }
  };
  view! {
    <button
      class=class
      aria-pressed=move || (!call.is_camera_on.get()).to_string()
      aria-label=label
      title=label
      on:click=move |_| call.set_camera(!call.is_camera_on.get_untracked())
    >
      <Icon name=Signal::derive(move || if call.is_camera_on.get() { IconName::Video } else { IconName::VideoOff }) />
    </button>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn channels_keep_the_huddle() {
    for kind in [ConversationKind::Public, ConversationKind::Private] {
      let bar = bar_kind(Some(kind));
      assert_eq!(bar.kind, CallKind::Huddle);
      assert_eq!(bar.join, "huddle-join");
    }
  }

  #[test]
  fn direct_messages_have_calls_not_huddles() {
    let direct = bar_kind(Some(ConversationKind::Direct));
    assert_eq!(direct.kind, CallKind::Direct);
    let group = bar_kind(Some(ConversationKind::Group));
    assert_eq!(group.kind, CallKind::Group);
    for bar in [direct, group] {
      assert_eq!(bar.in_progress, "call-bar-in-progress");
      assert_eq!(bar.join, "call-bar-join");
      assert_eq!(bar.leave, "call-bar-leave");
      assert_eq!(bar.label, "call-bar-label");
    }
  }
}
