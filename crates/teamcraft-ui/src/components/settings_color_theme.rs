//! Settings > Appearance > Colours (issue #202): the server's default, a preset, or a custom theme
//! of four colours. The choice applies at once (brand.rs sets the tokens) and follows the account
//! (`display.color_theme`, settings_sync.rs). Only the chrome changes; messages stay neutral.

use leptos::prelude::*;
use teamcraft_client_api::brand::BrandView;
use teamcraft_client_api::color_theme::{ColorTheme, PRESETS, PresetGroup, Rgb, ThemeColors, ThemeTokens};

use crate::brand::{Brand, system_prefers_dark};
use crate::components::message_actions::copy_and_confirm;
use crate::components::settings_rows::{SegmentedRow, SettingsCard, row_id, slug_options};
use crate::form_errors::{FieldError, FieldErrors, FormErrors, error_id};
use crate::i18n::use_i18n;
use crate::settings::Theme;
use crate::state::use_app_state;

/// The product's own colours, where a custom theme starts without a preset to start from.
const PRODUCT_COLORS: &str = "#1a2434,#0064fc,#2eb67d,#c8441d";
/// The custom theme's colours, in share code order: (field slug, i18n key).
const SLOTS: [(&str, &str); 4] = [
  ("navigation", "color-theme-navigation"),
  ("selection", "color-theme-selection"),
  ("presence", "color-theme-presence"),
  ("notification", "color-theme-notification"),
];
const IMPORT_FIELD: &str = "color-theme-import";

#[component]
pub fn ColorThemeCard() -> impl IntoView {
  let state = use_app_state();
  let prefs = state.settings.prefs;
  let chosen = Memo::new(move |_| prefs.with(|all| all.display.color_theme.clone()));
  let is_custom = RwSignal::new(matches!(chosen.get_untracked(), ColorTheme::Custom(_)));
  let choose = Callback::new(move |theme: ColorTheme| prefs.update(|all| all.display.color_theme = theme));
  view! {
    <SettingsCard heading="settings-color-theme" hint="settings-color-theme-hint">
      <SegmentedRow
        label="settings-color-theme-mode"
        options=slug_options(&["presets", "custom"], "settings-color-theme-mode-")
        value=Signal::derive(move || if is_custom.get() { "custom" } else { "presets" }.to_owned())
        on_change=Callback::new(move |slug: String| {
          let wants_custom = slug == "custom";
          is_custom.set(wants_custom);
          // Custom starts from what is on screen, so the switch itself changes nothing.
          if wants_custom && !matches!(chosen.get_untracked(), ColorTheme::Custom(_)) {
            choose.run(ColorTheme::Custom(starting_colors(&chosen.get_untracked())));
          }
        })
      />
      <Show when=move || is_custom.get() fallback=move || view! { <PresetGallery chosen choose /> }>
        <CustomEditor chosen choose />
      </Show>
    </SettingsCard>
  }
}

/// The server's default, then the presets by group, each a radio with a small picture.
#[component]
fn PresetGallery(chosen: Memo<ColorTheme>, choose: Callback<ColorTheme>) -> impl IntoView {
  let i18n = use_i18n();
  let is_dark = use_is_dark();
  let brand = Brand::use_current().0;
  let brand_colors = Signal::derive(move || brand.with(|view| brand_theme_colors(view, is_dark.get())));
  view! {
    <div class="set-swatches" role="radiogroup" id=row_id("settings-color-theme") aria-labelledby=format!("{}-heading", row_id("settings-color-theme"))>
      <div class="set-swatch-row">
        <Swatch name="color-theme-brand" theme=ColorTheme::Brand colors=brand_colors chosen choose is_dark />
      </div>
      {PresetGroup::ALL
        .into_iter()
        .map(|group| {
          let heading = format!("color-theme-group-{}", group.slug());
          view! {
            <h4 class="set-swatch-group">{move || i18n.t(&heading)}</h4>
            <div class="set-swatch-row">
              {PRESETS
                .iter()
                .filter(|preset| preset.group == group)
                .map(|preset| {
                  let name = format!("color-theme-{}", preset.id.as_str());
                  view! {
                    <Swatch name theme=ColorTheme::Preset(preset.id) colors=Signal::stored(preset.colors) chosen choose is_dark />
                  }
                })
                .collect_view()}
            </div>
          }
        })
        .collect_view()}
    </div>
  }
}

/// One theme as a radio: a tile in its sidebar colour with the selected row, the presence dot and
/// a badge, and its name.
#[component]
fn Swatch(
  #[prop(into)] name: String,
  theme: ColorTheme,
  colors: Signal<ThemeColors>,
  chosen: Memo<ColorTheme>,
  choose: Callback<ColorTheme>,
  is_dark: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let color = move |pick: fn(&ThemeTokens) -> Rgb| pick(&colors.get().tokens(is_dark.get())).to_string();
  let is_chosen = {
    let theme = theme.clone();
    move || chosen.with(|current| *current == theme)
  };
  let is_checked = is_chosen.clone();
  view! {
    <label class="set-swatch" class:is-chosen=is_chosen>
      <input
        type="radio"
        name="color-theme"
        class="visually-hidden"
        prop:checked=is_checked
        on:change=move |_| choose.run(theme.clone())
      />
      <span
        class="set-swatch-tile"
        aria-hidden="true"
        style:--sw-nav=move || color(|tokens| tokens.sidebar)
        style:--sw-sel=move || color(|tokens| tokens.selected)
        style:--sw-dot=move || color(|tokens| tokens.presence)
        style:--sw-badge=move || color(|tokens| tokens.badge)
        style:--sw-text=move || color(|tokens| tokens.sidebar_muted)
      >
        <span class="set-swatch-line"></span>
        <span class="set-swatch-sel"></span>
        <span class="set-swatch-line is-short"></span>
        <span class="set-swatch-dot"></span>
        <span class="set-swatch-badge"></span>
      </span>
      <span class="set-swatch-name">{move || i18n.t(&name)}</span>
    </label>
  }
}

/// Four colours with a hex field and a picker each, the theme code to copy or paste, and
/// "Surprise me". A valid change applies at once; an invalid hex shows its error at the field once
/// the field was left, and the theme stays as it was.
#[component]
fn CustomEditor(chosen: Memo<ColorTheme>, choose: Callback<ColorTheme>) -> impl IntoView {
  let i18n = use_i18n();
  let current = move || starting_colors(&chosen.get());
  let drafts: [RwSignal<String>; 4] = {
    let colors = current();
    [
      colors.navigation,
      colors.selection,
      colors.presence,
      colors.notification,
    ]
    .map(|color| RwSignal::new(color.to_string()))
  };
  let errors = FormErrors::new();
  // Another device (or "Surprise me", the code) changed the theme: show its colours.
  Effect::new(move |_| {
    let colors = current();
    let values = [
      colors.navigation,
      colors.selection,
      colors.presence,
      colors.notification,
    ];
    for (draft, color) in drafts.iter().zip(values) {
      let is_same = draft.with_untracked(|text| text.parse::<Rgb>().ok() == Some(color));
      if !is_same {
        draft.set(color.to_string());
      }
    }
  });
  let commit = move || {
    let code = drafts.map(|draft| draft.get_untracked()).join(",");
    if let Ok(colors) = ThemeColors::from_share_code(&code) {
      choose.run(ColorTheme::Custom(colors));
    }
  };
  // A field is judged once the user left it (forms.md), then as they type.
  let left: [RwSignal<bool>; 4] = std::array::from_fn(|_| RwSignal::new(false));
  let import_text = RwSignal::new(String::new());
  let import = move || match ThemeColors::from_share_code(&import_text.get_untracked()) {
    Ok(colors) => {
      errors.clear_field(IMPORT_FIELD);
      import_text.set(String::new());
      choose.run(ColorTheme::Custom(colors));
    },
    Err(_) => {
      let mut found = FieldErrors::new();
      found.push(IMPORT_FIELD, i18n.t("color-theme-import-invalid"));
      errors.refuse(row_id("settings-color-theme"), found);
    },
  };
  view! {
    <div class="set-custom-theme" id=row_id("settings-color-theme")>
      <p class="set-row-hint">{move || i18n.t("color-theme-custom-hint")}</p>
      <div class="set-custom-colors">
        {SLOTS
          .into_iter()
          .enumerate()
          .map(|(index, (slug, label))| {
            let id = field_id(slug);
            let draft = drafts[index];
            let was_left = left[index];
            let message_id = error_id(&id);
            let is_invalid = move || was_left.get() && draft.with(|text| text.parse::<Rgb>().is_err());
            let picker_value = move || {
              draft.with(|text| text.parse::<Rgb>().map(|color| color.to_string()).unwrap_or_else(|_| "#000000".to_owned()))
            };
            view! {
              <div class="field set-custom-color">
                <label for=id.clone()>{move || i18n.t(label)}</label>
                <div class="field-row">
                  <input
                    type="color"
                    class="set-color-picker"
                    aria-label=move || format!("{}: {}", i18n.t(label), i18n.t("color-theme-picker"))
                    prop:value=picker_value
                    on:input=move |ev| {
                      draft.set(event_target_value(&ev));
                      commit();
                    }
                  />
                  <input
                    id=id.clone()
                    class="input set-color-hex"
                    translate="no"
                    spellcheck="false"
                    autocomplete="off"
                    maxlength="7"
                    aria-invalid=move || is_invalid().then_some("true")
                    aria-describedby={
                      let message_id = message_id.clone();
                      move || is_invalid().then(|| message_id.clone())
                    }
                    prop:value=move || draft.get()
                    on:input=move |ev| {
                      draft.set(event_target_value(&ev));
                      commit();
                    }
                    on:change=move |_| was_left.set(true)
                  />
                </div>
                <p class="field-error" id=message_id>
                  {move || is_invalid().then(|| i18n.t("color-theme-hex-invalid"))}
                </p>
              </div>
            }
          })
          .collect_view()}
      </div>
      <div class="set-custom-actions">
        <button
          type="button"
          class="btn btn-small"
          on:click=move |_| {
            if let ColorTheme::Custom(colors) = chosen.get_untracked() {
              copy_and_confirm(colors.share_code());
            }
          }
        >
          {move || i18n.t("color-theme-copy")}
        </button>
        <button
          type="button"
          class="btn btn-small"
          on:click=move |_| {
            errors.clear();
            for was_left in left {
              was_left.set(false);
            }
            choose.run(ColorTheme::Custom(ThemeColors::from_hue(js_sys::Math::random() * 360.0)));
          }
        >
          {move || i18n.t("color-theme-surprise")}
        </button>
      </div>
      <form
        class="field set-custom-import"
        on:submit=move |ev| {
          ev.prevent_default();
          import();
        }
      >
        <label for=IMPORT_FIELD>{move || i18n.t("color-theme-import")}</label>
        <div class="field-row">
          <input
            id=IMPORT_FIELD
            class="input"
            translate="no"
            spellcheck="false"
            autocomplete="off"
            maxlength="64"
            placeholder=move || i18n.t("color-theme-import-placeholder")
            aria-invalid=errors.invalid(IMPORT_FIELD)
            aria-describedby=errors.described(IMPORT_FIELD)
            prop:value=move || import_text.get()
            on:input=move |ev| {
              import_text.set(event_target_value(&ev));
              errors.clear_field(IMPORT_FIELD);
            }
          />
          <button type="submit" class="btn btn-small" disabled=move || import_text.with(|text| text.trim().is_empty())>
            {move || i18n.t("color-theme-import-apply")}
          </button>
        </div>
        <FieldError errors field=IMPORT_FIELD />
      </form>
    </div>
  }
}

/// Whether the app shows dark mode now: the device's choice, else the system.
fn use_is_dark() -> Signal<bool> {
  let theme = use_app_state().settings.theme;
  Signal::derive(move || match theme.get() {
    Theme::Dark => true,
    Theme::Light => false,
    Theme::System => system_prefers_dark(),
  })
}

/// The server's default as theme colours, for its swatch: the brand's sidebar and selection where
/// it sets them, else the product's.
fn brand_theme_colors(view: &BrandView, is_dark: bool) -> ThemeColors {
  let product = starting_colors(&ColorTheme::Brand);
  let token = |name: &str| {
    view
      .properties(is_dark)
      .iter()
      .find(|(property, _)| property == name)
      .and_then(|(_, value)| value.parse::<Rgb>().ok())
  };
  let default_nav = if is_dark {
    Rgb([0x14, 0x14, 0x14])
  } else {
    product.navigation
  };
  ThemeColors {
    navigation: token("--sb-bg").unwrap_or(default_nav),
    selection: token("--sb-current").unwrap_or(product.selection),
    ..product
  }
}

/// The colours a custom theme starts from: the current custom or preset colours, else the
/// product's.
fn starting_colors(theme: &ColorTheme) -> ThemeColors {
  theme
    .colors()
    .or_else(|| ThemeColors::from_share_code(PRODUCT_COLORS).ok())
    .unwrap_or(ThemeColors {
      navigation: Rgb([0x1a, 0x24, 0x34]),
      selection: Rgb([0x00, 0x64, 0xfc]),
      presence: Rgb([0x2e, 0xb6, 0x7d]),
      notification: Rgb([0xc8, 0x44, 0x1d]),
    })
}

fn field_id(slug: &str) -> String {
  format!("color-theme-{slug}")
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::color_theme::Preset;

  use super::*;

  #[test]
  fn custom_starts_from_what_is_on_screen() {
    let preset = Preset::find("pine").unwrap();
    assert_eq!(starting_colors(&ColorTheme::Preset(preset.id)), preset.colors);
    assert_eq!(starting_colors(&ColorTheme::Brand).share_code(), PRODUCT_COLORS);
  }

  #[test]
  fn field_ids_are_distinct_from_the_import_field() {
    for (slug, _) in SLOTS {
      assert_ne!(field_id(slug), IMPORT_FIELD);
      assert_ne!(error_id(&field_id(slug)), error_id(IMPORT_FIELD));
    }
  }
}
