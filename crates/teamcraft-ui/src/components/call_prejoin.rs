use std::cell::Cell;

use leptos::prelude::*;

use crate::calls::{CallKind, CallRequest, MediaDevice};
use crate::components::call_video::{BackgroundPicker, VideoTile};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::use_interval;
use crate::state::use_app_state;

const METER_BARS: usize = 14;
const METER_TICK_MS: u64 = 120;
const SPEAKER_TEST_MS: i32 = 1600;

/// Lobby before a call: check camera, microphone and speaker, pick a background, then join.
#[component]
pub fn PreJoinScreen(request: CallRequest) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let me = state.me();
  let conversation = Memo::new(move |_| state.read(|backend| backend.conversation(request.conversation)));
  let devices = state.read(|backend| backend.media_devices());
  let participants = Memo::new(move |_| {
    state
      .read(|backend| backend.call_participants(request.conversation))
      .len()
  });

  // Remember the first device of each kind when nothing was chosen before.
  for (signal, list) in [
    (call.camera_id, &devices.cameras),
    (call.microphone_id, &devices.microphones),
    (call.speaker_id, &devices.speakers),
  ] {
    if signal.get_untracked().is_empty()
      && let Some(first) = list.first()
    {
      signal.set(first.id.clone());
    }
  }

  let title = move || {
    let name = conversation.get().map(|conv| conv.name).unwrap_or_default();
    match request.kind {
      CallKind::Direct => i18n.t_with("call-title-direct", &[("name", &name)]),
      CallKind::Group => i18n.t_with("call-title-group", &[("name", &name)]),
      CallKind::Huddle => i18n.t_with("call-title-huddle", &[("name", &name)]),
    }
  };

  // Simulated microphone level while the microphone is on.
  let level = RwSignal::new(0_usize);
  let tick = Cell::new(7_u32);
  use_interval(METER_TICK_MS, move || {
    if call.is_mic_on.get_untracked() {
      let next = tick.get().wrapping_mul(1_103_515_245).wrapping_add(12_345);
      tick.set(next);
      level.set((next >> 16) as usize % (METER_BARS - 2) + 2);
    } else {
      level.set(0);
    }
  });

  let is_testing_speaker = RwSignal::new(false);
  let test_speaker = move |_| {
    is_testing_speaker.set(true);
    set_timeout(
      move || {
        // The pre-join screen may be gone by then; `try_set` ignores a disposed signal.
        let _ = is_testing_speaker.try_set(false);
      },
      std::time::Duration::from_millis(SPEAKER_TEST_MS as u64),
    );
  };

  view! {
    <section class="prejoin" aria-labelledby="prejoin-title">
      <div class="prejoin-preview">
        <VideoTile
          user=me
          is_self=true
          is_large=true
          is_muted=Signal::derive(move || !call.is_mic_on.get())
          is_camera_on=call.is_camera_on
          effect=call.effect
          is_mirrored=call.mirror_self_view
        />
        <div class="preview-controls" role="group" aria-label=move || i18n.t("prejoin-quick-controls")>
          <button
            class="round-btn"
            class:off=move || !call.is_mic_on.get()
            aria-pressed=move || (!call.is_mic_on.get()).to_string()
            aria-label=move || if call.is_mic_on.get() { i18n.t("control-mic-mute") } else { i18n.t("control-mic-unmute") }
            on:click=move |_| call.is_mic_on.update(|on| *on = !*on)
          >
            <Icon name=Signal::derive(move || if call.is_mic_on.get() { IconName::Mic } else { IconName::MicOff }) />
          </button>
          <button
            class="round-btn"
            class:off=move || !call.is_camera_on.get()
            aria-pressed=move || (!call.is_camera_on.get()).to_string()
            aria-label=move || if call.is_camera_on.get() { i18n.t("control-camera-off") } else { i18n.t("control-camera-on") }
            on:click=move |_| call.is_camera_on.update(|on| *on = !*on)
          >
            <Icon name=Signal::derive(move || if call.is_camera_on.get() { IconName::Video } else { IconName::VideoOff }) />
          </button>
        </div>
      </div>
      <div class="prejoin-panel">
        <h1 id="prejoin-title">{title}</h1>
        <p class="muted-text">
          {move || {
            let count = participants.get();
            if request.kind == CallKind::Direct {
              i18n.t("prejoin-direct-hint")
            } else if count == 0 {
              i18n.t("prejoin-nobody")
            } else {
              i18n.t_count("prejoin-people-in-call", count)
            }
          }}
        </p>
        <div class="field">
          <label for="prejoin-camera">{move || i18n.t("prejoin-camera")}</label>
          <DeviceSelect id="prejoin-camera" list=devices.cameras.clone() selected=call.camera_id />
        </div>
        <div class="field">
          <label for="prejoin-mic">{move || i18n.t("prejoin-microphone")}</label>
          <DeviceSelect id="prejoin-mic" list=devices.microphones.clone() selected=call.microphone_id />
          <div
            class="meter"
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
        </div>
        <div class="field">
          <label for="prejoin-speaker">{move || i18n.t("prejoin-speaker")}</label>
          <div class="field-row">
            <DeviceSelect id="prejoin-speaker" list=devices.speakers.clone() selected=call.speaker_id />
            <button type="button" class="btn" on:click=test_speaker disabled=move || is_testing_speaker.get()>
              <Icon name=IconName::Volume />
              <span>
                {move || if is_testing_speaker.get() { i18n.t("prejoin-testing-speaker") } else { i18n.t("prejoin-test-speaker") }}
              </span>
            </button>
          </div>
        </div>
        <div class="field">
          <span class="field-title" id="prejoin-effects-title">{move || i18n.t("prejoin-effects")}</span>
          <BackgroundPicker effect=call.effect />
          <p class="muted-text small">{move || i18n.t("prejoin-effects-hint")}</p>
        </div>
        <label class="checkbox">
          <input
            type="checkbox"
            prop:checked=move || call.mirror_self_view.get()
            on:change=move |ev| call.mirror_self_view.set(event_target_checked(&ev))
          />
          <span>{move || i18n.t("prejoin-mirror")}</span>
        </label>
        <p class="e2ee-note">
          <Icon name=IconName::Shield />
          <span>{move || i18n.t("call-encrypted")}</span>
        </p>
        <div class="prejoin-actions">
          <button class="btn" on:click=move |_| call.leave()>
            {move || i18n.t("cancel")}
          </button>
          <button class="btn btn-primary" data-autofocus on:click=move |_| call.enter()>
            {move || i18n.t("prejoin-join")}
          </button>
        </div>
      </div>
    </section>
  }
}

#[component]
fn DeviceSelect(id: &'static str, list: Vec<MediaDevice>, selected: RwSignal<String>) -> impl IntoView {
  view! {
    <select
      id=id
      class="input grow"
      on:change=move |ev| selected.set(event_target_value(&ev))
    >
      {list
        .into_iter()
        .map(|device| {
          let value = device.id.clone();
          view! {
            <option value=device.id selected=move || selected.get() == value>
              {device.label}
            </option>
          }
        })
        .collect_view()}
    </select>
  }
}
