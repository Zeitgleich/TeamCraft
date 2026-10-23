//! Building blocks shared by the pre-join screen and the call screen: self preview, participant tiles
//! and the background effect picker. No real video here: tiles show a stylised camera image so the
//! layouts and effects can be reviewed without media.

use leptos::prelude::*;

use crate::calls::BackgroundEffect;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::User;

/// A participant tile (or the self preview). `effect` only applies to the user's own camera.
#[component]
pub fn VideoTile(
  user: User,
  #[prop(default = false)] is_self: bool,
  #[prop(into)] is_muted: Signal<bool>,
  #[prop(into)] is_camera_on: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] is_speaking: Signal<bool>,
  #[prop(into, default = Signal::stored(false))] has_hand_raised: Signal<bool>,
  /// The person's connection was lost and they are rejoining.
  #[prop(into, default = Signal::stored(false))]
  is_reconnecting: Signal<bool>,
  #[prop(into, default = Signal::stored(BackgroundEffect::None))] effect: Signal<BackgroundEffect>,
  #[prop(into, default = Signal::stored(false))] is_mirrored: Signal<bool>,
  #[prop(default = false)] is_large: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let name = if is_self {
    String::new()
  } else {
    user.display_name.clone()
  };
  let display = user.display_name.clone();
  let hue = user.avatar_hue;
  let initials = user.initials();

  view! {
    <div
      class="tile"
      class:tile-speaking=move || is_speaking.get()
      class:tile-reconnecting=move || is_reconnecting.get()
      class:tile-large=is_large
      style:--hue=hue.to_string()
      role="group"
      aria-label=move || {
        let mut parts = vec![if is_self { i18n.t("tile-you") } else { display.clone() }];
        if is_muted.get() {
          parts.push(i18n.t("tile-muted"));
        }
        if has_hand_raised.get() {
          parts.push(i18n.t("tile-hand"));
        }
        if is_speaking.get() {
          parts.push(i18n.t("tile-speaking"));
        }
        if is_reconnecting.get() {
          parts.push(i18n.t("tile-reconnecting"));
        }
        parts.join(", ")
      }
    >
      <div class=move || format!("video-surface effect-{}", if is_self { effect.get().slug() } else { "none" })>
        <Show
          when=move || is_camera_on.get()
          fallback={
            let initials = initials.clone();
            move || view! { <span class="tile-initials" aria-hidden="true">{initials.clone()}</span> }
          }
        >
          <div class="scene" aria-hidden="true"></div>
          <div class="silhouette" class:mirrored=move || is_mirrored.get() aria-hidden="true"></div>
        </Show>
      </div>
      <div class="tile-label">
        {(!is_self).then(|| view! { <span class="tile-name" translate="no">{name.clone()}</span> })}
        {is_self.then(|| view! { <span class="tile-name">{move || i18n.t("tile-you")}</span> })}
        <Show when=move || is_muted.get() fallback=|| ()>
          <Icon name=IconName::MicOff class="tile-badge" />
        </Show>
      </div>
      <Show when=move || is_reconnecting.get() fallback=|| ()>
        <span class="tile-status" aria-hidden="true">{move || i18n.t("tile-reconnecting")}</span>
      </Show>
      <Show when=move || has_hand_raised.get() fallback=|| ()>
        <span class="tile-hand" aria-hidden="true">
          <Icon name=IconName::Hand />
        </span>
      </Show>
    </div>
  }
}

pub use crate::components::background_picker::BackgroundPicker;
