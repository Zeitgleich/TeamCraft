//! An audio attachment as a card that plays it in the conversation (#802): play and pause, the
//! name, the duration and size, and once loaded a scrubber with the time, mute and volume. The card
//! only shows and steers the app's one player ([`crate::audio_player`]), so a row drawn anew keeps
//! playing.
//!
//! A file above the "Audio in conversations" limit (`settings::AudioLimit`) is a card with the
//! download only. A download that failed offers to try again, a format the webview can't play
//! says so and offers the download. Keys work while the play button or the controls have focus
//! (`video_controls::PlayerCommand`, without full screen).

use leptos::ev;
use leptos::prelude::*;
use teamcraft_client_api::actions::{FileInfo, FileTransfer};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen::JsCast;

use crate::audio::{AudioFailure, AudioFormat, AudioPhase};
use crate::audio_player::{AudioPlayer, AudioRequest};
use crate::components::attachment_menu;
use crate::components::file_views::{FileActions, FileMenu, TransferRing, transfer_status};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::settings::AudioLimit;
use crate::state::use_app_state;
use crate::video_controls::{PlayerCommand, format_seconds, percent_of};

/// Whether a file of a message is an audio card: audio that is on the server (not still going up).
pub fn is_audio_card(file: &FileInfo) -> bool {
  MediaKey::for_file(file).is_some()
    && AudioFormat::of(&file.media_type, &file.file_name).is_some()
    && !matches!(
      file.transfer,
      FileTransfer::Uploading { .. } | FileTransfer::UploadStalled { .. } | FileTransfer::UploadFailed { .. }
    )
}

/// One audio attachment; see the module.
#[component]
pub fn AudioCard(file: FileInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let player = state.audio;
  // `MessageFiles` only puts audio on the server here.
  let (Some(key), Some(format)) = (
    MediaKey::for_file(&file),
    AudioFormat::of(&file.media_type, &file.file_name),
  ) else {
    return ().into_any();
  };
  player.show(key);
  let request = AudioRequest {
    conversation: file.conversation,
    key,
    format,
  };
  let fits = AudioLimit::load().allows(file.size_bytes);
  let actions = FileActions::new(state, &file);
  let phase = Memo::new(move |_| player.phase_of(key));
  let is_loaded = move || phase.get().is_some_and(AudioPhase::is_loaded);
  let name = StoredValue::new(file.file_name.clone());
  let size = format_bytes(file.size_bytes);
  // The sender's duration until the player knows it.
  let maybe_sent_seconds = file
    .maybe_duration_ms
    .map(|duration_ms| duration_ms as f64 / 1000.0);
  let duration = move || {
    // Only the card of the attachment the player holds follows the player's duration.
    let maybe_known = phase
      .get()
      .map(|_| player.duration())
      .filter(|known| known.is_finite());
    maybe_known.or(maybe_sent_seconds)
  };
  let details = move || {
    let size = size.clone();
    match (fits, duration()) {
      (false, _) => format!("{size} · {}", i18n.t("audio-too-large")),
      (true, Some(seconds)) => format!("{} · {size}", format_seconds(seconds)),
      (true, None) => size,
    }
  };
  let (done_bytes, total_bytes) = match file.transfer {
    FileTransfer::Downloading {
      done_bytes,
      total_bytes,
    } => (done_bytes, total_bytes),
    _ => (0, 0),
  };
  let status = transfer_status(&file.transfer, actions);
  let press = move || {
    // One thing plays at a time: a video playing in its row makes room.
    state.active_inline_video.set(None);
    player.press(request);
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| run_key(player, is_loaded(), &ev);
  let play_label = move || {
    let name = name.get_value();
    let key = match phase.get() {
      Some(AudioPhase::Playing) => "audio-pause",
      Some(AudioPhase::Loading) => "audio-loading",
      _ => "audio-play",
    };
    i18n.t_with(key, &[("name", &name)])
  };
  let play = match fits {
    true => view! {
      <button
        class="audio-play"
        class:is-playing=move || phase.get() == Some(AudioPhase::Playing)
        aria-label=play_label
        aria-busy=move || (phase.get() == Some(AudioPhase::Loading)).to_string()
        // Nothing to play: the card says why and offers the download.
        disabled=move || phase.get() == Some(AudioPhase::Failed(AudioFailure::Unplayable))
        on:click=move |_| press()
        on:keydown=on_keydown
      >
        {move || match phase.get() {
          Some(AudioPhase::Loading) => view! { <TransferRing done_bytes=done_bytes total_bytes=total_bytes /> }.into_any(),
          Some(AudioPhase::Playing) => view! { <Icon name=IconName::Pause /> }.into_any(),
          _ => view! { <Icon name=IconName::Play /> }.into_any(),
        }}
      </button>
    }
    .into_any(),
    false => view! { <span class="file-icon" aria-hidden="true"><Icon name=IconName::Headphones /></span> }.into_any(),
  };
  let failure = move || {
    match phase.get() {
    Some(AudioPhase::Failed(AudioFailure::Unplayable)) => Some(view! {
      <span class="file-error" role="alert">{move || i18n.t("audio-unplayable")}</span>
      <button class="link-btn" on:click=move |_| actions.download()>{move || i18n.t("file-download")}</button>
    }
    .into_any()),
    Some(AudioPhase::Failed(AudioFailure::Unavailable)) => Some(view! {
      <span class="file-error" role="alert">{move || i18n.t("audio-unavailable")}</span>
      <button class="link-btn" on:click=move |_| press()>{move || i18n.t("file-retry")}</button>
    }
    .into_any()),
    _ => None,
  }
  };
  // The same menu as on any file card: the app's in the desktop app, the actions on a long-press.
  let maybe_menu = attachment_menu::use_attachment_menu();
  let menu_file = StoredValue::new(file.clone());
  let on_context_menu = move |ev: web_sys::MouseEvent| {
    let is_touch = crate::viewport::is_touch_primary();
    menu_file.with_value(|file| attachment_menu::open_on_right_click(maybe_menu, &ev, file, is_touch));
  };
  let file_name = name.get_value();
  view! {
    <div
      class="file-card audio-card"
      class:is-active=move || phase.get().is_some()
      role="group"
      aria-label=file_name.clone()
      on:contextmenu=on_context_menu
    >
      {play}
      <div class="file-meta">
        <span class="file-name" translate="no" title=file_name.clone()>{file_name.clone()}</span>
        <span class="file-size">{details}</span>
        {status}
        {failure}
      </div>
      <button
        class="icon-btn file-action"
        aria-label=move || i18n.t("file-download")
        data-tip=move || i18n.t("file-download")
        on:click=move |_| actions.download()
      >
        <Icon name=IconName::Download />
      </button>
      <FileMenu actions=actions />
      {move || is_loaded().then(|| view! { <AudioControls player=player on_keydown=on_keydown /> })}
    </div>
  }
  .into_any()
}

/// The scrubber, the time, mute and volume of the attachment the player holds.
#[component]
fn AudioControls(
  player: AudioPlayer,
  on_keydown: impl Fn(web_sys::KeyboardEvent) + 'static,
) -> impl IntoView {
  let i18n = use_i18n();
  let max = move || {
    let duration = player.duration();
    if duration.is_finite() { duration } else { 0.0 }
  };
  let on_seek = move |ev: ev::Event| {
    if let Ok(seconds) = event_target_value(&ev).parse::<f64>() {
      player.seek_to(seconds);
    }
  };
  let on_volume = move |ev: ev::Event| {
    if let Ok(level) = event_target_value(&ev).parse::<f64>() {
      player.set_volume(level);
    }
  };
  let position_text = move || {
    i18n.t_with(
      "video-position",
      &[
        ("current", &format_seconds(player.position())),
        ("total", &format_seconds(player.duration())),
      ],
    )
  };
  let is_silent = move || player.is_muted() || player.volume() == 0.0;
  let mute_label = move || match player.is_muted() {
    true => i18n.t("video-unmute"),
    false => i18n.t("video-mute"),
  };
  view! {
    <div class="audio-controls" on:keydown=on_keydown>
      <input
        class="audio-scrubber"
        type="range"
        min="0"
        step="any"
        prop:max=max
        prop:value=move || player.position()
        style:--media-played=move || format!("{:.2}%", percent_of(player.position(), player.duration()))
        aria-label=move || i18n.t("video-seek")
        aria-valuetext=position_text
        on:input=on_seek
      />
      <span class="audio-time" aria-hidden="true">
        {move || format!("{} / {}", format_seconds(player.position()), format_seconds(player.duration()))}
      </span>
      <button
        class="icon-btn audio-mute"
        aria-label=mute_label
        data-tip=mute_label
        on:click=move |_| player.run(PlayerCommand::ToggleMute)
      >
        <Icon name=Signal::derive(move || if is_silent() { IconName::VolumeOff } else { IconName::Volume }) />
      </button>
      <input
        class="audio-volume"
        type="range"
        min="0"
        max="1"
        step="0.05"
        prop:value=move || if player.is_muted() { 0.0 } else { player.volume() }
        style:--media-played=move || format!("{:.0}%", if player.is_muted() { 0.0 } else { player.volume() * 100.0 })
        aria-label=move || i18n.t("video-volume")
        on:input=on_volume
      />
    </div>
  }
}

/// A key on the play button or the controls: the player's keys once the file is loaded. Space and
/// Enter on a button press it, nothing more.
fn run_key(player: AudioPlayer, is_loaded: bool, ev: &web_sys::KeyboardEvent) {
  let is_button = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::HtmlButtonElement>().ok())
    .is_some();
  if !is_loaded || (is_button && matches!(ev.key().as_str(), " " | "Enter")) {
    return;
  }
  let has_modifier = ev.ctrl_key() || ev.meta_key() || ev.alt_key();
  let Some(command) = PlayerCommand::for_key(&ev.key(), has_modifier) else {
    return;
  };
  if command == PlayerCommand::ToggleFullscreen {
    return;
  }
  // Arrows seek instead of scrolling the conversation; the scrubber steps the same way.
  ev.prevent_default();
  ev.stop_propagation();
  player.run(command);
}
