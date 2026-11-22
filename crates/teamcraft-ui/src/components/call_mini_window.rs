//! The page of the desktop app's small call window (#2077): who talks, the call's time and the
//! controls that matter (mute, camera, sound, hang up, back to the call). It shows what the main
//! window hands over (`call_mini_info`, again on `CALL_MINI_CHANGED_EVENT`) and sends its buttons
//! back through the shell as tray actions; the call itself runs only with the main window.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::call_mini::{CALL_MINI_CHANGED_EVENT, CallMini};
use teamcraft_client_api::tray_menu::TrayAction;

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::components::incoming_call::apply_window_theme;
use crate::i18n::provide_i18n;

/// How often the window's clock ticks.
const TICK: Duration = Duration::from_secs(1);

/// Renders only the small window into the page (the shell opened it under its label).
pub fn mount() {
  if let Some(document) = web_sys::window().and_then(|window| window.document()) {
    // The window stays hidden until the controls are drawn; the loading screen has nothing to do.
    if let Some(loader) = document.get_element_by_id("tc-loader") {
      loader.remove();
    }
    if let Some(root) = document.document_element() {
      let _ = root.class_list().add_1("call-window");
    }
  }
  leptos::mount::mount_to_body(CallMiniWindow);
}

#[component]
fn CallMiniWindow() -> impl IntoView {
  // A page of its own: the user's language as the main window stored it.
  let i18n = provide_i18n(crate::settings::stored_locale());
  let shown = RwSignal::new(None::<CallMini>);
  let now_ms = RwSignal::new(js_sys::Date::now());
  read_call(shown, true);
  tauri::listen(CALL_MINI_CHANGED_EVENT, move || read_call(shown, false));
  // The window only exists while it is on screen, so its clock runs only then.
  if let Ok(handle) = set_interval_with_handle(move || now_ms.set(js_sys::Date::now()), TICK) {
    on_cleanup(move || handle.clear());
  }

  let act = move |action: TrayAction| {
    leptos::task::spawn_local(async move {
      let args = serde_json::json!({ "action": action });
      if let Err(error) = tauri::call::<()>("call_mini_action", &args).await {
        tracing::warn!(%error, "the small call window could not hand over a button");
      }
    });
  };
  let flag = move |read: fn(&CallMini) -> bool| shown.with(|maybe| maybe.as_ref().is_some_and(read));
  let is_muted = move || flag(|call| call.is_muted);
  let is_camera_on = move || flag(|call| call.is_camera_on);
  let is_sound_off = move || flag(|call| call.is_sound_off);
  let status = move || {
    let Some(call) = shown.get() else {
      return String::new();
    };
    if call.is_reconnecting {
      return i18n.t("call-mini-reconnecting");
    }
    let clock = call.maybe_joined_at_ms.map(|joined_at| {
      // A clock set back counts as no time.
      let elapsed = (now_ms.get() as i64).saturating_sub(joined_at).max(0);
      crate::calls::call_clock(u64::try_from(elapsed).unwrap_or(0))
    });
    let who = match &call.maybe_speaker {
      Some(speaker) => speaker.name.clone(),
      None => i18n.t_count("call-participants", call.people as usize),
    };
    match clock {
      Some(clock) => format!("{who} · {clock}"),
      None => who,
    }
  };

  view! {
    <div class="call-mini" data-tauri-drag-region role="region" aria-label=move || i18n.t("call-mini-label")>
      <div class="call-mini-head" data-tauri-drag-region>
        {move || match shown.get().and_then(|call| call.maybe_speaker) {
          Some(speaker) => view! {
            <span class="avatar avatar-md is-speaking" data-tauri-drag-region style:--hue=speaker.avatar_hue.to_string() aria-hidden="true">
              <span class="avatar-initials">{speaker.initials}</span>
            </span>
          }.into_any(),
          None => view! {
            <span class="call-mini-glyph" data-tauri-drag-region aria-hidden="true"><Icon name=IconName::Phone /></span>
          }.into_any(),
        }}
        <div class="call-mini-text" data-tauri-drag-region>
          <p class="call-mini-title" data-tauri-drag-region translate="no">{move || shown.get().map(|call| call.title)}</p>
          <p class="call-mini-status" data-tauri-drag-region aria-live="polite">{status}</p>
        </div>
        <button
          class="icon-btn call-mini-back"
          aria-label=move || i18n.t("call-mini-back")
          title=move || i18n.t("call-mini-back")
          on:click=move |_| act(TrayAction::OpenCall)
        >
          <Icon name=IconName::Maximize />
        </button>
      </div>
      <div class="call-mini-controls">
        <button
          class="round-btn"
          class:off=is_muted
          aria-label=move || if is_muted() { i18n.t("control-mic-unmute") } else { i18n.t("control-mic-mute") }
          title=move || if is_muted() { i18n.t("control-mic-unmute") } else { i18n.t("control-mic-mute") }
          on:click=move |_| act(if is_muted() { TrayAction::Unmute } else { TrayAction::Mute })
        >
          {move || view! { <Icon name=if is_muted() { IconName::MicOff } else { IconName::Mic } /> }}
        </button>
        <button
          class="round-btn"
          class:off=move || !is_camera_on()
          aria-label=move || if is_camera_on() { i18n.t("control-camera-off") } else { i18n.t("control-camera-on") }
          title=move || if is_camera_on() { i18n.t("control-camera-off") } else { i18n.t("control-camera-on") }
          on:click=move |_| act(if is_camera_on() { TrayAction::CameraOff } else { TrayAction::CameraOn })
        >
          {move || view! { <Icon name=if is_camera_on() { IconName::Video } else { IconName::VideoOff } /> }}
        </button>
        <button
          class="round-btn"
          class:off=is_sound_off
          aria-label=move || if is_sound_off() { i18n.t("control-undeafen") } else { i18n.t("control-deafen") }
          title=move || if is_sound_off() { i18n.t("control-undeafen") } else { i18n.t("control-deafen") }
          on:click=move |_| act(if is_sound_off() { TrayAction::SoundOn } else { TrayAction::SoundOff })
        >
          {move || view! { <Icon name=if is_sound_off() { IconName::VolumeOff } else { IconName::Volume } /> }}
        </button>
        <button
          class="round-btn call-mini-hangup"
          aria-label=move || i18n.t("call-bar-leave")
          title=move || i18n.t("call-bar-leave")
          on:click=move |_| {
            let asks = flag(|call| call.asks_handover);
            act(if asks { TrayAction::LeaveCallAsking } else { TrayAction::LeaveCall });
          }
        >
          <Icon name=IconName::Phone class="hangup" />
        </button>
      </div>
    </div>
  }
}

/// Reads what the window shows from the shell; the first time, the window appears once drawn.
fn read_call(shown: RwSignal<Option<CallMini>>, is_first: bool) {
  leptos::task::spawn_local(async move {
    let maybe_call = match tauri::call::<Option<CallMini>>("call_mini_info", &serde_json::json!({})).await {
      Ok(maybe_call) => maybe_call,
      Err(error) => {
        tracing::warn!(%error, "the small call window could not read its call");
        None
      },
    };
    if let Some(call) = &maybe_call {
      apply_window_theme(call.theme);
    }
    if shown.try_set(maybe_call).is_some() || !is_first {
      return;
    }
    // Shown once the controls are laid out, so the window never appears empty.
    request_animation_frame(|| {
      leptos::task::spawn_local(async {
        if let Err(error) = tauri::call::<()>("call_mini_ready", &serde_json::json!({})).await {
          tracing::warn!(%error, "the small call window could not appear");
        }
      });
    });
  });
}
