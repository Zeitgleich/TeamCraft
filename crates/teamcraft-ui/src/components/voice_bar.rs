//! Recording a voice message in the composer (#204): the record button and the strip above the
//! input that shows the recording and its take.
//!
//! - A click (or Enter or Space) starts a hands-free recording with Discard, Stop and Send.
//! - Pressing and holding records while held: letting go sends, letting go after sliding to the
//!   left discards, after sliding up keeps recording hands-free.
//! - The first press on a device explains what the microphone is for before the browser or the
//!   system asks; a refusal says how to allow it.
//! - A take stopped by a call, the background, leaving or the length limit waits here with what
//!   stopped it; only Send sends it.

use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::voice::{RecordingRefusal, RecordingStop, Waveform};

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;
use crate::video_controls::format_seconds;
use crate::voice_recorder::{ComposerKey, RecorderPhase};

/// A press held this long records while held.
const HOLD_DELAY: Duration = Duration::from_millis(300);
/// Sliding this far to the left while holding discards on release (CSS pixels).
const DISCARD_SLIDE_PX: i32 = 80;
/// Sliding this far up while holding keeps recording hands-free.
const LOCK_SLIDE_PX: i32 = 60;
/// Bars of a take's waveform in the strip.
const TAKE_BARS: usize = 32;

/// What a held press does when it is let go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
  Send,
  Discard,
  Lock,
}

/// Where a held press started, while it is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hold {
  start_x: i32,
  start_y: i32,
  is_recording: bool,
}

/// The record button. `on_send` sends the take (the composer builds the message around it).
#[component]
pub fn VoiceButton(key: ComposerKey, max_seconds: u32, #[prop(into)] on_send: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let recorder = use_app_state().voice;
  let maybe_hold = RwSignal::new(None::<Hold>);
  let is_busy_elsewhere = move || recorder.is_busy_elsewhere(key);
  let is_active = move || recorder.phase_for(key).is_some();
  let on_pointer_down = move |ev: web_sys::PointerEvent| {
    if ev.button() != 0 || is_active() || is_busy_elsewhere() {
      return;
    }
    if let Some(target) = ev.target()
      && let Ok(element) = wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(target)
    {
      drop(element.set_pointer_capture(ev.pointer_id()));
    }
    maybe_hold.set(Some(Hold {
      start_x: ev.client_x(),
      start_y: ev.client_y(),
      is_recording: false,
    }));
    set_timeout(
      move || {
        if maybe_hold.try_get_untracked().flatten().is_some() {
          maybe_hold.try_update(|hold| {
            if let Some(hold) = hold {
              hold.is_recording = true;
            }
          });
          recorder.press(key, max_seconds);
        }
      },
      HOLD_DELAY,
    );
  };
  let on_pointer_up = move |ev: web_sys::PointerEvent| {
    let Some(hold) = maybe_hold.get_untracked() else {
      return;
    };
    maybe_hold.set(None);
    if !hold.is_recording {
      // A click: hands-free.
      recorder.press(key, max_seconds);
      return;
    }
    match recorder.phase_for(key) {
      Some(RecorderPhase::Recording) => {},
      // Let go before the microphone opened: nothing to send.
      Some(RecorderPhase::Starting) => {
        recorder.discard();
        return;
      },
      _ => return,
    }
    match release_of(ev.client_x() - hold.start_x, ev.client_y() - hold.start_y) {
      Release::Send => on_send.run(()),
      Release::Discard => recorder.discard(),
      Release::Lock => {},
    }
  };
  let on_pointer_cancel = move |_| {
    if maybe_hold.get_untracked().is_some_and(|hold| hold.is_recording) {
      recorder.stop(None);
    }
    maybe_hold.set(None);
  };
  // Enter and Space: a click without a pointer (`detail` 0) starts hands-free.
  let on_click = move |ev: web_sys::MouseEvent| {
    if ev.detail() == 0 && !is_active() && !is_busy_elsewhere() {
      recorder.press(key, max_seconds);
    }
  };
  let label = move || match is_busy_elsewhere() {
    true => i18n.t("voice-busy-elsewhere"),
    false => i18n.t("voice-record"),
  };
  view! {
    <button
      type="button"
      class="icon-btn voice-record-btn"
      class:is-holding=move || maybe_hold.get().is_some_and(|hold| hold.is_recording)
      aria-label=label
      data-tip=label
      aria-disabled=move || is_busy_elsewhere().to_string()
      on:pointerdown=on_pointer_down
      on:pointerup=on_pointer_up
      on:pointercancel=on_pointer_cancel
      on:click=on_click
    >
      <Icon name=IconName::Mic />
    </button>
  }
}

/// The strip above the input while `key` has a recording or a take.
#[component]
pub fn VoiceBar(key: ComposerKey, max_seconds: u32, #[prop(into)] on_send: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let recorder = use_app_state().voice;
  let phase = Memo::new(move |_| recorder.phase_for(key));
  move || {
    let current = phase.get()?;
    let content = match current {
      RecorderPhase::Explaining => view! {
        <p class="voice-text">{move || i18n.t("voice-explain")}</p>
        <button class="btn btn-small" on:click=move |_| recorder.discard()>{move || i18n.t("cancel")}</button>
        <button class="btn btn-small btn-primary" on:click=move |_| recorder.start(max_seconds)>
          {move || i18n.t("voice-explain-continue")}
        </button>
      }
      .into_any(),
      RecorderPhase::Starting => view! {
        <p class="voice-text">{move || i18n.t("voice-starting")}</p>
        <button class="btn btn-small" on:click=move |_| recorder.discard()>{move || i18n.t("cancel")}</button>
      }
      .into_any(),
      RecorderPhase::Recording | RecorderPhase::Finishing => view! {
        <button class="icon-btn" aria-label=move || i18n.t("voice-discard") data-tip=move || i18n.t("voice-discard") on:click=move |_| recorder.discard()>
          <Icon name=IconName::Trash />
        </button>
        <span class="voice-dot" aria-hidden="true"></span>
        <span class="voice-time">
          {move || format!("{} / {}", format_seconds(recorder.elapsed_ms() as f64 / 1000.0), format_seconds(f64::from(max_seconds)))}
        </span>
        <span class="voice-live" aria-hidden="true">
          {move || recorder.live_levels().into_iter().map(|level| view! {
            <span class="voice-bar is-played" style:--media-bar-height=format!("{:.0}%", 12.0 + level * 88.0)></span>
          }).collect_view()}
        </span>
        <span class="voice-hint">{move || i18n.t("voice-hold-hint")}</span>
        <button class="icon-btn" aria-label=move || i18n.t("voice-stop") data-tip=move || i18n.t("voice-stop") on:click=move |_| recorder.stop(None)>
          <Icon name=IconName::Stop />
        </button>
        <button class="send-btn" aria-label=move || i18n.t("voice-send") data-tip=move || i18n.t("voice-send") on:click=move |_| on_send.run(())>
          <Icon name=IconName::Send />
        </button>
      }
      .into_any(),
      RecorderPhase::Review(info) => {
        let heights = Waveform::checked(&info.waveform)
          .map(|waveform| waveform.heights(TAKE_BARS))
          .unwrap_or_default();
        let stop_note = info.maybe_stop.map(|stop| i18n.t(stop_key(stop)));
        view! {
          <button class="icon-btn" aria-label=move || i18n.t("voice-discard") data-tip=move || i18n.t("voice-discard") on:click=move |_| recorder.discard()>
            <Icon name=IconName::Trash />
          </button>
          <span class="voice-live" aria-hidden="true">
            {heights.into_iter().map(|height| view! {
              <span class="voice-bar" style:--media-bar-height=format!("{:.0}%", 12.0 + height * 88.0)></span>
            }).collect_view()}
          </span>
          <span class="voice-time">{format_seconds(info.duration_ms as f64 / 1000.0)}</span>
          {stop_note.map(|note| view! { <span class="voice-hint" role="status">{note}</span> })}
          <button class="send-btn" aria-label=move || i18n.t("voice-send") data-tip=move || i18n.t("voice-send") on:click=move |_| on_send.run(())>
            <Icon name=IconName::Send />
          </button>
        }
        .into_any()
      },
      RecorderPhase::Refused(refusal) => view! {
        <p class="voice-text file-error" role="alert">{move || i18n.t(refusal_key(refusal))}</p>
        <button class="btn btn-small" on:click=move |_| recorder.discard()>{move || i18n.t("close")}</button>
        <button class="btn btn-small btn-primary" on:click=move |_| recorder.start(max_seconds)>{move || i18n.t("voice-try-again")}</button>
      }
      .into_any(),
    };
    let status = match phase.get_untracked() {
      Some(RecorderPhase::Recording) => i18n.t("voice-recording"),
      Some(RecorderPhase::Review(_)) => i18n.t("voice-review"),
      _ => String::new(),
    };
    Some(view! {
      <div class="voice-strip" role="group" aria-label=move || i18n.t("voice-message-label")>
        <span class="visually-hidden" aria-live="polite">{status}</span>
        {content}
      </div>
    })
  }
}

/// What a held press does when let go after moving `dx`, `dy` CSS pixels.
pub fn release_of(dx: i32, dy: i32) -> Release {
  if dx <= -DISCARD_SLIDE_PX {
    Release::Discard
  } else if dy <= -LOCK_SLIDE_PX {
    Release::Lock
  } else {
    Release::Send
  }
}

fn stop_key(stop: RecordingStop) -> &'static str {
  match stop {
    RecordingStop::LimitReached => "voice-stopped-limit",
    RecordingStop::Call => "voice-stopped-call",
    RecordingStop::Background => "voice-stopped-background",
    RecordingStop::MicrophoneLost => "voice-stopped-microphone",
    RecordingStop::Left => "voice-stopped-left",
  }
}

fn refusal_key(refusal: RecordingRefusal) -> &'static str {
  match (refusal, crate::auth::tauri::is_available()) {
    (RecordingRefusal::Microphone, true) => "voice-refused-desktop",
    (RecordingRefusal::Microphone, false) => "voice-refused-browser",
    (RecordingRefusal::Busy, _) => "voice-busy",
    (RecordingRefusal::Failed, _) => "voice-failed",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_held_press_sends_discards_or_locks_by_where_it_ends() {
    assert_eq!(release_of(0, 0), Release::Send);
    assert_eq!(release_of(-20, 10), Release::Send);
    assert_eq!(release_of(-DISCARD_SLIDE_PX, 0), Release::Discard);
    assert_eq!(release_of(0, -LOCK_SLIDE_PX), Release::Lock);
    // Sliding left wins over sliding up: discarding is what the user aimed for.
    assert_eq!(release_of(-200, -200), Release::Discard);
  }
}
