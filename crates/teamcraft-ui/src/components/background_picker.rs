//! Radio group of call background effects: blur, shipped presets and the user's own pictures.
//! Own pictures are uploaded through the backend (the host normalizes and encrypts them, the core
//! syncs them to the user's devices) and can be deleted again here.

use leptos::prelude::*;
use wasm_bindgen_futures::JsFuture;

use teamcraft_proto::BackgroundId;

use crate::admin::format_bytes;
use crate::calls::{BackgroundEffect, BackgroundImageInfo, EffectGroup};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Largest picture the picker offers to upload; matches the server default `max_background_bytes`
/// (teamcraft.example.toml), which stays the authority.
const MAX_PICTURE_BYTES: u64 = 8 * 1024 * 1024;
const CUSTOM_SLUG_PREFIX: &str = "custom-";

/// Radio group of background effects plus an upload tile.
#[component]
pub fn BackgroundPicker(effect: RwSignal<BackgroundEffect>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let own = Memo::new(move |_| state.read(|backend| backend.background_images()));
  let error = RwSignal::new(None::<String>);
  let file_input = NodeRef::<leptos::html::Input>::new();
  let options = Memo::new(move |_| effect_options(&own.get()));

  // A picture deleted elsewhere (another device) must not stay selected.
  Effect::new(move |_| {
    if let BackgroundEffect::Custom(id) = effect.get()
      && !own.get().iter().any(|image| image.id == id)
    {
      effect.set(BackgroundEffect::None);
    }
  });

  let move_selection = move |ev: web_sys::KeyboardEvent| {
    let all = options.get_untracked();
    let Some(next) = next_effect(&all, effect.get_untracked(), &ev.key()) else {
      return;
    };
    ev.prevent_default();
    effect.set(next);
    focus_effect_tile(next);
  };

  let on_file = move |_| {
    let Some(file) = file_input
      .get_untracked()
      .and_then(|element| element.files())
      .and_then(|files| files.item(0))
    else {
      return;
    };
    if let Some(element) = file_input.get_untracked() {
      element.set_value("");
    }
    if file.size() as u64 > MAX_PICTURE_BYTES {
      error.set(Some(i18n.t_with(
        "effect-upload-too-large",
        &[("limit", &format_bytes(MAX_PICTURE_BYTES))],
      )));
      return;
    }
    error.set(None);
    leptos::task::spawn_local(async move {
      match JsFuture::from(file.array_buffer()).await {
        Ok(buffer) => {
          let picture = js_sys::Uint8Array::new(&buffer).to_vec();
          state.write(|backend| backend.add_background_image(&file.name(), picture));
        },
        Err(_) => error.set(Some(i18n.t("effect-upload-unreadable"))),
      }
    });
  };

  view! {
    <div
      class="effect-grid"
      role="radiogroup"
      aria-label=move || i18n.t("prejoin-effects")
      on:keydown=move_selection
    >
      {move || {
        let images = own.get();
        let mut previous = EffectGroup::Basic;
        options
          .get()
          .into_iter()
          .map(|option| {
            let maybe_image = match option {
              BackgroundEffect::Custom(id) => images.iter().find(|image| image.id == id).cloned(),
              _ => None,
            };
            let maybe_heading = (option.group() != previous)
              .then(|| group_heading_key(option.group()))
              .flatten()
              .map(|key| view! { <p class="effect-group" role="presentation">{move || i18n.t(key)}</p> });
            previous = option.group();
            view! {
              {maybe_heading}
              <EffectTile effect=effect option=option maybe_image=maybe_image error=error />
            }
          })
          .collect_view()
      }}
      <button type="button" class="effect-tile" on:click=move |_| {
        if let Some(element) = file_input.get_untracked() {
          element.click();
        }
      }>
        <span class="effect-swatch effect-upload" aria-hidden="true">
          <Icon name=IconName::Plus />
        </span>
        <span class="effect-name">{move || i18n.t("effect-upload")}</span>
      </button>
      <input
        type="file"
        accept="image/jpeg,image/png"
        hidden
        tabindex="-1"
        node_ref=file_input
        on:change=on_file
      />
    </div>
    <Show when=move || error.get().is_some() fallback=|| ()>
      <p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p>
    </Show>
    <SystemEffectsHint />
  }
}

/// macOS applies its own video effects (Portrait, background replacement, Studio Light) to every
/// camera image before the app sees it. The app can't tell whether they are on, so it says how the
/// two combine and opens the system's panel, where they are turned off.
#[component]
fn SystemEffectsHint() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_mac_app = crate::auth::tauri::is_available()
    && web_sys::window()
      .and_then(|window| window.navigator().platform().ok())
      .is_some_and(|platform| platform.to_ascii_lowercase().contains("mac"));
  is_mac_app.then(|| {
    view! {
      <div class="system-effects-hint" role="note">
        <p class="muted-text small">{move || i18n.t("effects-system-hint")}</p>
        <button class="btn btn-small" on:click=move |_| state.read(|backend| backend.open_system_video_effects())>
          {move || i18n.t("effects-system-open")}
        </button>
      </div>
    }
  })
}

/// One selectable tile; own pictures carry a delete button next to the radio button (a button must
/// not nest inside another one).
#[component]
fn EffectTile(
  effect: RwSignal<BackgroundEffect>,
  option: BackgroundEffect,
  maybe_image: Option<BackgroundImageInfo>,
  error: RwSignal<Option<String>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let label = match &maybe_image {
    Some(image) => image.name.clone(),
    None => String::new(),
  };
  let name_view = {
    let label = label.clone();
    move || {
      if label.is_empty() {
        i18n.t(&format!("effect-{}", option.slug()))
      } else {
        label.clone()
      }
    }
  };
  let delete = maybe_image.map(|image| {
    let name = image.name.clone();
    let id: BackgroundId = image.id;
    view! {
      <button
        type="button"
        class="effect-delete icon-button"
        aria-label=move || i18n.t_with("effect-delete", &[("name", &name)])
        on:click=move |_| {
          error.set(None);
          state.write(|backend| backend.delete_background_image(id));
        }
      >
        <Icon name=IconName::Trash />
      </button>
    }
  });

  view! {
    <div class="effect-cell">
      <button
        type="button"
        class="effect-tile"
        role="radio"
        id=format!("effect-{}", effect_slug(option))
        aria-checked=move || (effect.get() == option).to_string()
        tabindex=move || if effect.get() == option { "0" } else { "-1" }
        on:click=move |_| effect.set(option)
      >
        <span class=format!("effect-swatch effect-{}", option.slug()) aria-hidden="true">
          {match option {
            BackgroundEffect::Preset(preset) if preset.is_photo() => view! {
              <img
                class="effect-photo"
                src=photo_thumbnail_url(preset.slug())
                loading="lazy"
                decoding="async"
                width="320"
                height="180"
                alt=""
              />
            }
              .into_any(),
            _ => view! { <span class="scene"></span> }.into_any(),
          }}
        </span>
        <span class="effect-name" translate=if label.is_empty() { "yes" } else { "no" }>{name_view}</span>
      </button>
      {delete}
    </div>
  }
}

/// The picture shown for a photo preset: small files the web build serves next to the app
/// (`assets/backgrounds/thumbs`, copied by Trunk), loaded when the tile scrolls into view.
fn photo_thumbnail_url(slug: &str) -> String {
  format!("/backgrounds/thumbs/{slug}.jpg")
}

/// The i18n key of the heading above a group of tiles; the first group needs none.
fn group_heading_key(group: EffectGroup) -> Option<&'static str> {
  match group {
    EffectGroup::Basic | EffectGroup::Own => None,
    EffectGroup::Photos => Some("effect-group-photos"),
    EffectGroup::Illustrations => Some("effect-group-illustrations"),
  }
}

/// All options in display order: the shipped effects, then the user's own pictures.
pub fn effect_options(own: &[BackgroundImageInfo]) -> Vec<BackgroundEffect> {
  BackgroundEffect::ALL
    .into_iter()
    .chain(own.iter().map(|image| BackgroundEffect::Custom(image.id)))
    .collect()
}

/// Stable string for preferences and element ids; own pictures keep their id (`custom-<id>`).
pub fn effect_slug(effect: BackgroundEffect) -> String {
  match effect {
    BackgroundEffect::Custom(id) => format!("{CUSTOM_SLUG_PREFIX}{id}"),
    other => other.slug().to_owned(),
  }
}

/// The option an arrow key moves to, wrapping around. `None` for other keys and for an empty list.
fn next_effect(all: &[BackgroundEffect], current: BackgroundEffect, key: &str) -> Option<BackgroundEffect> {
  if all.is_empty() {
    return None;
  }
  let position = all
    .iter()
    .position(|candidate| *candidate == current)
    .unwrap_or(0);
  let next = match key {
    "ArrowRight" | "ArrowDown" => (position + 1) % all.len(),
    "ArrowLeft" | "ArrowUp" => (position + all.len() - 1) % all.len(),
    _ => return None,
  };
  all.get(next).copied()
}

/// Inverse of [`effect_slug`]; unknown values fall back to no effect.
pub fn effect_from_slug(slug: &str) -> BackgroundEffect {
  if let Some(raw) = slug.strip_prefix(CUSTOM_SLUG_PREFIX) {
    return raw.parse().map(BackgroundEffect::Custom).unwrap_or_default();
  }
  BackgroundEffect::ALL
    .into_iter()
    .find(|candidate| candidate.slug() == slug)
    .unwrap_or_default()
}

fn focus_effect_tile(option: BackgroundEffect) {
  use wasm_bindgen::JsCast;

  let target = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.get_element_by_id(&format!("effect-{}", effect_slug(option))))
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(target) = target {
    drop(target.focus());
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::calls::BackgroundPreset;

  const EN_FTL: &str = include_str!("../../i18n/en.ftl");
  const DE_FTL: &str = include_str!("../../i18n/de.ftl");

  #[test]
  fn slugs_round_trip_for_every_option() {
    let own = [image(7), image(9)];
    for option in effect_options(&own) {
      assert_eq!(effect_from_slug(&effect_slug(option)), option);
    }
  }

  #[test]
  fn unknown_slugs_mean_no_effect() {
    assert_eq!(effect_from_slug("custom-abc"), BackgroundEffect::None);
    assert_eq!(effect_from_slug("hologram"), BackgroundEffect::None);
    assert_eq!(effect_from_slug(""), BackgroundEffect::None);
  }

  #[test]
  fn own_pictures_come_after_the_shipped_effects() {
    let options = effect_options(&[image(3)]);
    assert_eq!(options.len(), BackgroundEffect::ALL.len() + 1);
    assert_eq!(options[0], BackgroundEffect::None);
    assert!(options.contains(&BackgroundEffect::Preset(BackgroundPreset::Forest)));
    assert!(options.contains(&BackgroundEffect::Preset(BackgroundPreset::LivingRoom)));
    assert_eq!(
      options.last(),
      Some(&BackgroundEffect::Custom("3".parse().unwrap()))
    );
  }

  #[test]
  fn photos_get_thumbnails_and_groups_get_headings() {
    assert_eq!(
      photo_thumbnail_url("living-room"),
      "/backgrounds/thumbs/living-room.jpg"
    );
    assert_eq!(
      group_heading_key(EffectGroup::Photos),
      Some("effect-group-photos")
    );
    assert_eq!(
      group_heading_key(EffectGroup::Illustrations),
      Some("effect-group-illustrations")
    );
    assert_eq!(group_heading_key(EffectGroup::Basic), None);
    assert_eq!(group_heading_key(EffectGroup::Own), None);
  }

  #[test]
  fn every_preset_name_and_heading_is_translated() {
    for lang in [EN_FTL, DE_FTL] {
      for option in BackgroundEffect::ALL {
        let key = format!("effect-{} =", option.slug());
        assert!(lang.lines().any(|line| line.starts_with(&key)), "missing {key}");
      }
      for group in [EffectGroup::Photos, EffectGroup::Illustrations] {
        let key = format!("{} =", group_heading_key(group).unwrap());
        assert!(lang.lines().any(|line| line.starts_with(&key)), "missing {key}");
      }
    }
  }

  #[test]
  fn arrow_keys_wrap_and_an_empty_list_is_safe() {
    let all = [BackgroundEffect::None, BackgroundEffect::BlurLight];
    assert_eq!(
      next_effect(&all, BackgroundEffect::BlurLight, "ArrowRight"),
      Some(BackgroundEffect::None)
    );
    assert_eq!(
      next_effect(&all, BackgroundEffect::None, "ArrowUp"),
      Some(BackgroundEffect::BlurLight)
    );
    assert_eq!(next_effect(&all, BackgroundEffect::None, "Enter"), None);
    assert_eq!(next_effect(&[], BackgroundEffect::None, "ArrowRight"), None);
    assert_eq!(next_effect(&[], BackgroundEffect::None, "ArrowLeft"), None);
  }

  fn image(raw: u64) -> BackgroundImageInfo {
    BackgroundImageInfo {
      id: raw.to_string().parse().unwrap(),
      name: format!("Picture {raw}"),
      size_bytes: 1024,
    }
  }
}
