//! The quality menu of another device's video in a call (#967), like a video player's: a small
//! button on the video opens it, and it offers Auto (the quality arriving now in brackets) and
//! each quality the sender offers. A pick holds for the rest of the call; the media layer still
//! caps it by what is sent and what the connection carries. The choices and labels come from
//! [`crate::video_quality`].

use leptos::prelude::*;
use teamcraft_proto::MediaStreamId;

use crate::calls::VideoQuality;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::reactive::use_interval_while_visible;
use crate::state::use_app_state;
use crate::video_quality::{arriving_lines, quality_choices, quality_label};

/// How often the open menu asks which quality arrives.
const ARRIVING_INTERVAL_MS: u64 = 1_000;

/// The button on another device's video that opens its quality menu (and closes it again).
#[component]
pub fn VideoQualityButton(stream_id: MediaStreamId) -> impl IntoView {
  let call = use_app_state().call;
  let i18n = use_i18n();
  let is_open = move || call.maybe_quality_menu.get() == Some(stream_id);
  view! {
    <button
      type="button"
      class="tile-quality"
      class:active=is_open
      aria-label=move || i18n.t("video-quality")
      title=move || i18n.t("video-quality")
      aria-haspopup="dialog"
      aria-expanded=move || is_open().to_string()
      on:click=move |_| {
        call
          .maybe_quality_menu
          .update(|open| *open = if *open == Some(stream_id) { None } else { Some(stream_id) });
      }
    >
      <Icon name=IconName::Sliders />
    </button>
  }
}

/// The open quality menu of `stream_id`, over the call's controls like the call's other menus. It
/// closes once the video is gone.
#[component]
pub fn VideoQualityMenu(stream_id: MediaStreamId) -> impl IntoView {
  let state = use_app_state();
  let call = state.call;
  let i18n = use_i18n();
  let track = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .call_video_tracks()
        .into_iter()
        .find(|track| track.stream_id == stream_id)
    })
  });
  Effect::new(move |_| {
    if track.with(Option::is_none) {
      call.maybe_quality_menu.set(None);
    }
  });
  let name = Memo::new(move |_| {
    let user = track.get()?.user;
    state
      .read(|backend| backend.user(user))
      .map(|user| user.display_name)
  });
  let choices = Memo::new(move |_| {
    track.with(|maybe_track| {
      maybe_track.as_ref().map_or_else(
        || vec![VideoQuality::Auto],
        |track| quality_choices(&track.layers),
      )
    })
  });
  // Which quality arrives now, for "Auto (720p)": asked of the media layer while the menu is open.
  let arriving = RwSignal::new(None::<u16>);
  let ask = move || {
    state.read(|backend| {
      backend.fetch_call_video_stats(Box::new(move |maybe_json| {
        let maybe_lines = maybe_json
          .and_then(|json| json.read())
          .and_then(|report| arriving_lines(&report, stream_id));
        // The menu may have closed meanwhile.
        let _ = arriving.try_set(maybe_lines);
      }));
    });
  };
  ask();
  use_interval_while_visible(ARRIVING_INTERVAL_MS, ask);
  let title = move || match name.get() {
    Some(name) => i18n.t_with("video-quality-for", &[("name", &name)]),
    None => i18n.t("video-quality"),
  };
  let pick = move |quality: VideoQuality| {
    call.set_video_quality(stream_id, quality);
    call.maybe_quality_menu.set(None);
  };
  view! {
    <div class="effects-popover camera-popover quality-popover" role="dialog" aria-labelledby="quality-menu-title">
      <h2 id="quality-menu-title">{title}</h2>
      <div class="camera-options" role="radiogroup" aria-labelledby="quality-menu-title">
        {move || {
          choices
            .get()
            .into_iter()
            .map(|choice| {
              let is_chosen = Signal::derive(move || call.video_quality(stream_id) == choice);
              let label = move || {
                let words = |key: &str, args: &[(&str, &str)]| i18n.t_with(key, args);
                quality_label(choice, arriving.get(), &words)
              };
              view! {
                <button
                  type="button"
                  role="radio"
                  class="camera-option"
                  class:chosen=move || is_chosen.get()
                  aria-checked=move || is_chosen.get().to_string()
                  on:click=move |_| pick(choice)
                >
                  <span class="camera-option-name">{label}</span>
                  <Show when=move || is_chosen.get() fallback=|| ()>
                    <Icon name=IconName::Check />
                  </Show>
                </button>
              }
            })
            .collect_view()
        }}
      </div>
      <p class="quality-hint muted-text">{move || i18n.t("video-quality-hint")}</p>
    </div>
  }
}
