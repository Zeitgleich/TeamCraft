//! Colour themes for the mobile apps (issue #202): the presets and the colours a theme gives the
//! app's chrome and accent, from `teamcraft_client_api::color_theme`, so every client derives the
//! same colours with the same contrast rules. The chosen theme itself is a synced setting
//! (`SyncedAppSettings::color_theme`), a string the app keeps as it is.

use teamcraft_client_api::color_theme::{ColorTheme, PRESETS, Rgb, ThemeColors};

/// The colours a theme sets for light or dark mode, as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct AppThemeTokens {
  /// Bars that frame the content (the top bar).
  pub chrome: u32,
  /// Text and icons on [`Self::chrome`]: 4.5:1.
  pub on_chrome: u32,
  /// Links, buttons, the focus ring: 4.5:1 on the content surfaces.
  pub accent: u32,
  pub accent_strong: u32,
  pub on_accent: u32,
  /// A tint of the accent behind content (selected rows, chips).
  pub accent_soft: u32,
  /// A selected item's fill; white text on it is 4.5:1.
  pub selected: u32,
  /// The "online" dot: 3:1 on the content surfaces.
  pub presence: u32,
  /// Unread and mention badges; white numbers on it are 4.5:1.
  pub badge: u32,
}

/// A built-in theme, for the picker.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppThemePreset {
  /// The value to store as the setting.
  pub value: String,
  /// The end of the name's string resource (`color_theme_<id>` with `-` as `_`).
  pub id: String,
  /// `calm`, `bright`, `vivid` or `accessible`.
  pub group: String,
  /// The theme's own colours, for a swatch: navigation, selection, presence, notification.
  pub colors: Vec<u32>,
}

/// The built-in themes in the order of the picker.
#[uniffi::export]
pub fn color_theme_presets() -> Vec<AppThemePreset> {
  PRESETS
    .iter()
    .map(|preset| AppThemePreset {
      value: ColorTheme::Preset(preset.id).to_string(),
      id: preset.id.as_str().to_owned(),
      group: preset.group.slug().to_owned(),
      colors: colors_of(&preset.colors),
    })
    .collect()
}

/// The colours of the stored theme `value` for light or dark mode; `None` for the server's
/// default (the brand, or the app's own colours) and for a value this build doesn't know.
#[uniffi::export]
pub fn color_theme_tokens(value: String, is_dark: bool) -> Option<AppThemeTokens> {
  let Ok(theme) = value.parse::<ColorTheme>();
  let tokens = theme.colors()?.tokens(is_dark);
  Some(AppThemeTokens {
    chrome: rgb(tokens.chrome),
    on_chrome: rgb(tokens.sidebar_text),
    accent: rgb(tokens.accent),
    accent_strong: rgb(tokens.accent_strong),
    on_accent: rgb(tokens.on_accent),
    accent_soft: rgb(tokens.accent_soft),
    selected: rgb(tokens.selected),
    presence: rgb(tokens.presence),
    badge: rgb(tokens.badge),
  })
}

/// The colours of the stored theme `value` (navigation, selection, presence, notification), to
/// start a custom theme from; `None` for the server's default.
#[uniffi::export]
pub fn color_theme_colors(value: String) -> Option<Vec<u32>> {
  let Ok(theme) = value.parse::<ColorTheme>();
  theme.colors().map(|colors| colors_of(&colors))
}

/// The setting's value for a custom theme from a share code someone pasted (four hex colours);
/// `None` if it isn't one.
#[uniffi::export]
pub fn color_theme_from_code(code: String) -> Option<String> {
  ThemeColors::from_share_code(&code)
    .ok()
    .map(|colors| ColorTheme::Custom(colors).to_string())
}

/// The setting's value for a custom theme of four `0xRRGGBB` colours (navigation, selection,
/// presence, notification); `None` without four.
#[uniffi::export]
pub fn color_theme_custom(colors: Vec<u32>) -> Option<String> {
  let [navigation, selection, presence, notification] = colors.as_slice() else {
    return None;
  };
  let colors = ThemeColors {
    navigation: from_rgb(*navigation),
    selection: from_rgb(*selection),
    presence: from_rgb(*presence),
    notification: from_rgb(*notification),
  };
  Some(ColorTheme::Custom(colors).to_string())
}

/// The share code of a custom theme `value`, to copy; `None` for anything else.
#[uniffi::export]
pub fn color_theme_share_code(value: String) -> Option<String> {
  match value.parse::<ColorTheme>() {
    Ok(ColorTheme::Custom(colors)) => Some(colors.share_code()),
    _ => None,
  }
}

/// "Surprise me": a custom theme around `hue` (0 to 360; the app picks it at random).
#[uniffi::export]
pub fn color_theme_surprise(hue: f64) -> String {
  ColorTheme::Custom(ThemeColors::from_hue(hue)).to_string()
}

fn colors_of(colors: &ThemeColors) -> Vec<u32> {
  [
    colors.navigation,
    colors.selection,
    colors.presence,
    colors.notification,
  ]
  .map(rgb)
  .to_vec()
}

fn rgb(color: Rgb) -> u32 {
  let [r, g, b] = color.0;
  u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

fn from_rgb(value: u32) -> Rgb {
  let [_, r, g, b] = value.to_be_bytes();
  Rgb([r, g, b])
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn presets_resolve_to_tokens_and_the_default_to_none() {
    for preset in color_theme_presets() {
      assert!(
        color_theme_tokens(preset.value.clone(), true).is_some(),
        "{}",
        preset.id
      );
      assert_eq!(preset.colors.len(), 4);
      assert_eq!(color_theme_colors(preset.value), Some(preset.colors));
    }
    assert_eq!(color_theme_tokens("brand".into(), false), None);
    assert_eq!(color_theme_tokens("preset:from-the-future".into(), false), None);
  }

  #[test]
  fn custom_themes_round_trip_through_codes_and_colours() {
    let value = color_theme_from_code("1b2233, 2f66e0, 2eb67d, d1453b".into()).unwrap();
    assert_eq!(value, "custom:#1b2233,#2f66e0,#2eb67d,#d1453b");
    let colors = color_theme_colors(value.clone()).unwrap();
    assert_eq!(colors[0], 0x1b2233);
    assert_eq!(color_theme_custom(colors), Some(value.clone()));
    assert_eq!(
      color_theme_share_code(value),
      Some("#1b2233,#2f66e0,#2eb67d,#d1453b".to_owned())
    );
    assert_eq!(color_theme_from_code("nope".into()), None);
    assert_eq!(color_theme_custom(vec![0, 1]), None);
    assert_eq!(color_theme_share_code("brand".into()), None);
    assert!(color_theme_surprise(200.0).starts_with("custom:#"));
  }
}
