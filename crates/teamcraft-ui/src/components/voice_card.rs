//! A voice message in the conversation (#204): play and pause, the sender's waveform as the
//! scrubber with the played part filled, the time, and the speed (1×, 1.5×, 2×, remembered on the
//! device). It steers the app's one player ([`crate::audio_player`]) like the audio card, so one
//! thing plays at a time and a row drawn anew keeps playing.
//!
//! The waveform and the duration come from the sender, inside the encrypted message; the file
//! itself is downloaded and decrypted on the first press, as any audio attachment.

use leptos::prelude::*;
use teamcraft_client_api::actions::{FileInfo, FileTransfer};
use teamcraft_client_api::voice::Waveform;

use crate::audio::{AudioFailure, AudioFormat, AudioPhase};
use crate::audio_player::AudioRequest;
use crate::components::attachment_menu;
use crate::components::file_views::{FileActions, FileMenu, TransferRing, transfer_status};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::settings;
use crate::state::use_app_state;
use crate::video_controls::{PlayerCommand, format_seconds};
use crate::voice_recorder::is_in_call;

/// Where the chosen speed is remembered.
pub const SPEED_KEY: &str = "voice-playback-speed";
/// Bars the card draws.
const BARS: usize = 40;
/// Seconds an arrow key moves.
const KEY_STEP_SECONDS: f64 = 5.0;

/// Whether an audio file is a voice message: it carries a waveform.
pub fn is_voice(file: &FileInfo) -> bool {
  file
    .maybe_waveform
    .as_deref()
    .and_then(Waveform::checked)
    .is_some()
}

/// One voice message; see the module. `MessageFiles` only puts audio on the server here.
#[component]
pub fn VoiceCard(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let player = state.audio;
  let speed = state.voice_speed;
  let (Some(key), Some(format), Some(waveform)) = (
    MediaKey::for_file(&file),
    AudioFormat::of(&file.media_type, &file.file_name),
    file.maybe_waveform.as_deref().and_then(Waveform::checked),
  ) else {
    return ().into_any();
  };
  player.show(key);
  let heights = waveform.heights(BARS);
  let actions = FileActions::new(state, &file);
  let phase = Memo::new(move |_| player.phase_of(key));
  let is_loaded = move || phase.get().is_some_and(AudioPhase::is_loaded);
  let sent_seconds = file
    .maybe_duration_ms
    .map(|duration_ms| duration_ms as f64 / 1000.0);
  let duration = move || {
    let maybe_known = phase
      .get()
      .map(|_| player.duration())
      .filter(|known| known.is_finite() && *known > 0.0);
    maybe_known.or(sent_seconds).unwrap_or(0.0)
  };
  let position = move || match phase.get() {
    Some(_) => player.position(),
    None => 0.0,
  };
  let played = Memo::new(move |_| match duration() {
    total if total > 0.0 => (position() / total).clamp(0.0, 1.0),
    _ => 0.0,
  });
  let (done_bytes, total_bytes) = match file.transfer {
    FileTransfer::Downloading {
      done_bytes,
      total_bytes,
    } => (done_bytes, total_bytes),
    _ => (0, 0),
  };
  let status = transfer_status(&file.transfer, actions);
  let conversation = file.conversation;
  let max_seconds = state.read(|backend| backend.voice_message_policy()).max_seconds();
  let press = move || {
    // No voice message plays over a call (#204).
    if is_in_call(state) {
      return;
    }
    state.active_inline_video.set(None);
    player.press(AudioRequest {
      conversation,
      key,
      format,
      rate: speed.get_untracked().rate(),
      maybe_voice_max_seconds: Some(max_seconds),
    });
  };
  let cycle_speed = move |_| {
    let next = speed.get_untracked().next();
    speed.set(next);
    settings::store_json(SPEED_KEY, &next);
    player.set_rate(key, next.rate());
  };
  let seek_to_fraction = move |fraction: f64| {
    if is_loaded() {
      player.seek_to(fraction.clamp(0.0, 1.0) * duration());
    } else {
      press();
    }
  };
  let on_wave_click = move |ev: web_sys::MouseEvent| {
    let Some(target) = ev.current_target() else {
      return;
    };
    let Ok(element) = wasm_bindgen::JsCast::dyn_into::<web_sys::Element>(target) else {
      return;
    };
    let rect = element.get_bounding_client_rect();
    if rect.width() > 0.0 {
      seek_to_fraction((f64::from(ev.client_x()) - rect.left()) / rect.width());
    }
  };
  let on_wave_key = move |ev: web_sys::KeyboardEvent| {
    let command = match ev.key().as_str() {
      "ArrowLeft" | "ArrowDown" => PlayerCommand::SeekBy(-KEY_STEP_SECONDS),
      "ArrowRight" | "ArrowUp" => PlayerCommand::SeekBy(KEY_STEP_SECONDS),
      "Home" => PlayerCommand::SeekToStart,
      "End" => PlayerCommand::SeekToEnd,
      " " | "Enter" | "k" => PlayerCommand::TogglePlay,
      _ => return,
    };
    ev.prevent_default();
    ev.stop_propagation();
    match is_loaded() {
      true => player.run(command),
      false if command == PlayerCommand::TogglePlay => press(),
      false => {},
    }
  };
  let play_label = move || {
    let length = format_seconds(duration());
    if is_in_call(state) && phase.get() != Some(AudioPhase::Playing) {
      return i18n.t("voice-in-call-playback");
    }
    let key = match phase.get() {
      Some(AudioPhase::Playing) => "voice-pause",
      Some(AudioPhase::Loading) => "voice-loading",
      _ => "voice-play",
    };
    i18n.t_with(key, &[("length", &length)])
  };
  let time_text = move || match phase.get() {
    Some(_) => format!("{} / {}", format_seconds(position()), format_seconds(duration())),
    None => format_seconds(duration()),
  };
  let position_text = move || {
    i18n.t_with(
      "video-position",
      &[
        ("current", &format_seconds(position())),
        ("total", &format_seconds(duration())),
      ],
    )
  };
  let speed_label = move || i18n.t_with("voice-speed", &[("speed", speed.get().label())]);
  let failure = move || {
    match phase.get() {
    Some(AudioPhase::Failed(AudioFailure::Unplayable)) => Some(
      view! {
        <span class="file-error" role="alert">{move || i18n.t("audio-unplayable")}</span>
        <button class="link-btn" on:click=move |_| actions.download()>{move || i18n.t("file-download")}</button>
      }
      .into_any(),
    ),
    Some(AudioPhase::Failed(AudioFailure::Unavailable)) => Some(
      view! {
        <span class="file-error" role="alert">{move || i18n.t("audio-unavailable")}</span>
        <button class="link-btn" on:click=move |_| press()>{move || i18n.t("file-retry")}</button>
      }
      .into_any(),
    ),
    _ => None,
  }
  };
  let maybe_menu = attachment_menu::use_attachment_menu();
  let menu_file = StoredValue::new(file.clone());
  let on_context_menu = move |ev: web_sys::MouseEvent| {
    let is_touch = crate::viewport::is_touch_primary();
    menu_file.with_value(|file| attachment_menu::open_on_right_click(maybe_menu, &ev, file, is_touch, false));
  };
  let bars = heights
    .into_iter()
    .enumerate()
    .map(|(index, height)| {
      let threshold = (index as f64 + 0.5) / BARS as f64;
      let is_played = Signal::derive(move || played.get() >= threshold);
      view! {
        <span
          class="voice-bar"
          class:is-played=is_played
          style:--media-bar-height=format!("{:.0}%", 12.0 + f64::from(height) * 88.0)
        ></span>
      }
    })
    .collect_view();
  view! {
    <div
      class="file-card voice-card"
      class:is-active=move || phase.get().is_some()
      role="group"
      aria-label=move || i18n.t_with("voice-message", &[("length", &format_seconds(duration()))])
      on:contextmenu=on_context_menu
    >
      <button
        class="audio-play"
        class:is-playing=move || phase.get() == Some(AudioPhase::Playing)
        aria-label=play_label
        aria-busy=move || (phase.get() == Some(AudioPhase::Loading)).to_string()
        disabled=move || {
          phase.get() == Some(AudioPhase::Failed(AudioFailure::Unplayable))
            || (is_in_call(state) && phase.get() != Some(AudioPhase::Playing))
        }
        on:click=move |_| press()
      >
        {move || match phase.get() {
          Some(AudioPhase::Loading) => view! { <TransferRing done_bytes=done_bytes total_bytes=total_bytes /> }.into_any(),
          Some(AudioPhase::Playing) => view! { <Icon name=IconName::Pause /> }.into_any(),
          _ => view! { <Icon name=IconName::Play /> }.into_any(),
        }}
      </button>
      <div class="voice-body">
        <div
          class="voice-wave"
          role="slider"
          tabindex="0"
          aria-label=move || i18n.t("video-seek")
          aria-valuemin="0"
          aria-valuemax=move || format!("{:.0}", duration())
          aria-valuenow=move || format!("{:.0}", position())
          aria-valuetext=position_text
          on:click=on_wave_click
          on:keydown=on_wave_key
        >
          {bars}
        </div>
        <div class="voice-meta">
          <span class="voice-time" aria-hidden="true">{time_text}</span>
          {status}
          {failure}
        </div>
      </div>
      <button class="voice-speed" aria-label=speed_label data-tip=speed_label on:click=cycle_speed>
        {move || speed.get().label()}
      </button>
      <FileMenu actions=actions />
    </div>
  }
  .into_any()
}
