//! What the own screen share sends (#1175): a small live preview, collapsible, in place of a
//! picture of the share at full size. Small so sharing the whole screen or this window doesn't
//! fill the stage with mirrors of itself. Collapsed, nothing runs for it.

use leptos::prelude::*;

use crate::components::call_video::{LiveVideo, VideoSource, can_show_video};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// The preview of the own share, or, where this page shows no call video, a plain note that the
/// share runs. `is_over_picture`: it sits in a corner of somebody else's shared screen.
#[component]
pub fn OwnSharePreview(is_over_picture: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_hidden = Memo::new(move |_| state.settings.prefs.with(|prefs| prefs.calls.hide_share_preview));
  // The call's word, not the button's: that one is on while the picker is still open, when there
  // is no capture to show yet.
  let is_capturing = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .is_some_and(|active| active.is_sharing_screen)
    })
  });
  let toggle = move |_| {
    state
      .settings
      .prefs
      .update(|prefs| prefs.calls.hide_share_preview = !prefs.calls.hide_share_preview);
  };
  let caption = move || {
    let key = if !can_show_video() {
      "share-preview-unavailable"
    } else if is_hidden.get() {
      "share-preview-hidden"
    } else if is_capturing.get() {
      "share-preview-caption"
    } else {
      "share-preview-starting"
    };
    i18n.t(key)
  };
  let is_shown = move || can_show_video() && !is_hidden.get() && is_capturing.get();
  view! {
    <div
      class="share-preview"
      class:share-preview-over-picture=is_over_picture
      role="group"
      aria-label=move || i18n.t("share-preview-label")
    >
      <Show when=is_shown fallback=|| ()>
        <div class="share-preview-picture">
          <LiveVideo source=VideoSource::OwnScreen is_mirrored=false />
        </div>
      </Show>
      <div class="share-preview-bar">
        <Icon name=IconName::Monitor />
        <span>{caption}</span>
        <Show when=can_show_video fallback=|| ()>
          <button
            class="btn btn-small"
            aria-expanded=move || (!is_hidden.get()).to_string()
            on:click=toggle
          >
            {move || i18n.t(if is_hidden.get() { "share-preview-show" } else { "share-preview-hide" })}
          </button>
        </Show>
      </div>
    </div>
  }
}
