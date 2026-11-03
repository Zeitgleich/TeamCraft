//! The call's microphone and speaker menu (#986), next to the microphone button: the
//! microphones and speakers to switch to during the call, the microphone's level, and a speaker
//! test. A pick switches the running call at once, keeps the mute and is remembered like the
//! setting.

use std::cell::Cell;
use std::rc::Rc;

use leptos::prelude::*;

use crate::calls::{AudioKind, AudioMenuEntry, CallEvent, audio_menu_entries};
use crate::components::call_prejoin::{METER_BARS, METER_TICK_MS, SPEAKER_TEST_MS, meter_bars};
use crate::components::call_video::can_choose_speakers;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::{use_interval_while, use_page_visible};
use crate::state::use_app_state;

/// The open menu. It lists the devices again when it opens (a headset may have come since), and
/// the call's microphone level reaches its meter while it is open.
#[component]
pub fn AudioMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let is_real = !untrack(|| state.read(|backend| backend.has_simulated_calls()));
  let devices = Memo::new(move |_| state.read(|backend| backend.media_devices()));
  call.emit(CallEvent::RefreshDevices);
  if is_real {
    untrack(|| state.read(|backend| backend.watch_call_level(true)));
    on_cleanup(move || {
      // Gone only while the app itself is torn down: nothing left to stop.
      let _ = state.try_read(|backend| backend.watch_call_level(false));
    });
  }

  // The level while the microphone is on and the page visible: measured in the call, or made
  // up in the demo. A muted microphone shows an empty meter, as before joining.
  let level = RwSignal::new(0_usize);
  let tick = Rc::new(Cell::new(11_u32));
  let is_page_visible = use_page_visible();
  use_interval_while(
    METER_TICK_MS,
    move || is_page_visible.get() && call.is_mic_on.get(),
    move || {
      let bars = if is_real {
        meter_bars(state.read(|backend| backend.call_preview_level()))
      } else {
        let next = tick.get().wrapping_mul(1_103_515_245).wrapping_add(12_345);
        tick.set(next);
        (next >> 16) as usize % (METER_BARS - 2) + 2
      };
      level.set(bars);
    },
  );
  Effect::new(move |_| {
    if !call.is_mic_on.get() {
      level.set(0);
    }
  });

  let is_testing_speaker = RwSignal::new(false);
  let test_speaker = move |_| {
    is_testing_speaker.set(true);
    // Browsers start sound only inside the click: the context is made here, before any `await`.
    #[cfg(target_arch = "wasm32")]
    if crate::web_calls::is_in_page() {
      crate::speaker_test::prime();
    }
    call.emit(CallEvent::TestSpeakers(call.speaker_id.get_untracked()));
    set_timeout(
      move || {
        // The menu may be closed by then; `try_set` ignores a disposed signal.
        let _ = is_testing_speaker.try_set(false);
      },
      std::time::Duration::from_millis(SPEAKER_TEST_MS),
    );
  };
  let pick = move |kind: AudioKind, id: String| state.choose_audio_device(kind, id);
  let entries = move |kind: AudioKind| {
    let chosen = call.audio_id(kind).get();
    devices.with(|devices| audio_menu_entries(kind.of(devices), &chosen))
  };

  view! {
    <div class="effects-popover camera-popover audio-popover" role="dialog" aria-label=move || i18n.t("control-audio-choose")>
      <h2 id="audio-menu-microphone">{move || i18n.t("prejoin-microphone")}</h2>
      <div class="camera-options" role="radiogroup" aria-labelledby="audio-menu-microphone">
        {move || {
          entries(AudioKind::Microphone)
            .into_iter()
            .map(|entry| audio_option(entry, move |id| pick(AudioKind::Microphone, id)))
            .collect_view()
        }}
      </div>
      <div
        class="meter audio-meter"
        role="meter"
        aria-label=move || i18n.t("prejoin-mic-level")
        aria-valuemin="0"
        aria-valuemax=METER_BARS.to_string()
        aria-valuenow=move || level.get().to_string()
      >
        {(0..METER_BARS)
          .map(|bar| view! { <span class="meter-bar" class:lit=move || bar < level.get()></span> })
          .collect_view()}
      </div>
      // A browser that can't route sound (Safari) has no choice to offer; the test still plays on
      // the system's output.
      <h2 id="audio-menu-speakers">{move || i18n.t("prejoin-speaker")}</h2>
      {can_choose_speakers().then(|| view! {
        <div class="camera-options" role="radiogroup" aria-labelledby="audio-menu-speakers">
          {move || {
            entries(AudioKind::Speakers)
              .into_iter()
              .map(|entry| audio_option(entry, move |id| pick(AudioKind::Speakers, id)))
              .collect_view()
          }}
        </div>
      })}
      <button type="button" class="btn audio-test" on:click=test_speaker disabled=move || is_testing_speaker.get()>
        <Icon name=IconName::Volume />
        <span>
          {move || if is_testing_speaker.get() { i18n.t("prejoin-testing-speaker") } else { i18n.t("prejoin-test-speaker") }}
        </span>
      </button>
    </div>
  }
}

/// One microphone or speakers of the menu, checked while the call uses it.
fn audio_option(entry: AudioMenuEntry, pick: impl Fn(String) + 'static) -> impl IntoView {
  let i18n = use_i18n();
  let AudioMenuEntry {
    id,
    label,
    is_system_default,
    is_checked,
  } = entry;
  // Ours names the device it is now; a browser's own default entry is named by the browser.
  let name = if is_system_default && id.is_empty() {
    if label.is_empty() {
      i18n.t("calls-system-default")
    } else {
      i18n.t_with("call-audio-system-default", &[("device", label.as_str())])
    }
  } else {
    label
  };
  view! {
    <button
      type="button"
      role="radio"
      class="camera-option"
      class:chosen=is_checked
      aria-checked=is_checked.to_string()
      on:click=move |_| {
        if !is_checked {
          pick(id.clone());
        }
      }
    >
      <span class="camera-option-name">{name}</span>
      {is_checked.then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}
