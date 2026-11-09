//! Colour themes (issue #202): the user's choice of colours for the app's chrome – the rail, the
//! sidebar, the top bar, the selected row, the accent, the presence dot and the unread badge.
//!
//! In light mode the sidebar is a light tint of the navigation colour with dark text, and only the
//! rail and the top bar carry the colour itself; in dark mode the sidebar is the (darkened)
//! navigation colour.
//!
//! A theme never touches the content surfaces (messages, composer, threads, panels, dialogs): those
//! stay neutral, white in light mode and dark grey in dark mode, so text is always readable.
//!
//! The choice is one of three: the server's brand colours (the default, docs/branding.md §4), one of
//! the built-in [`PRESETS`], or a custom theme from four colours. Every theme is made of the same
//! four colours ([`ThemeColors`]); the other tokens, for light and dark mode, are derived from them
//! with WCAG contrast enforced ([`ThemeColors::tokens`]), so no choice can make text unreadable.
//!
//! The choice follows the account (settings sync, e2ee.md §6.4) as a short string
//! ([`ColorTheme`]'s wire form); light/dark stays per device. A custom theme can be shared as a
//! short code of four hex colours ([`ThemeColors::share_code`]).
//!
//! A brand can set the default theme and limit what people may pick ([`ThemePolicy`], issue #1495).
//! A theme the policy doesn't allow is shown as the default here, and the choice stays as it is,
//! since another brand may allow it.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use teamcraft_proto::BrandThemePolicy;

/// WCAG AA for normal text.
const TEXT_CONTRAST: f64 = 4.5;
/// WCAG 1.4.11 for marks that carry meaning (presence dots, the selected row against the sidebar).
const MARK_CONTRAST: f64 = 3.0;
/// The content surfaces themes are checked against: the darkest light surface accent text sits
/// on (`--surface-2`), and the lightest dark one (`--surface-3`). 000-tokens.css.
const LIGHT_SURFACE: Rgb = Rgb([0xf0, 0xf0, 0xf0]);
const LIGHT_PAGE: Rgb = Rgb([0xff, 0xff, 0xff]);
const DARK_SURFACE: Rgb = Rgb([0x30, 0x30, 0x30]);
const DARK_PAGE: Rgb = Rgb([0x1e, 0x1e, 0x1e]);
const WHITE: Rgb = Rgb([0xff, 0xff, 0xff]);
const BLACK: Rgb = Rgb([0x00, 0x00, 0x00]);
/// Text on a light sidebar.
const INK: Rgb = Rgb([0x14, 0x14, 0x14]);
/// What a light navigation colour is darkened toward in dark mode.
const DARK_NAV_BASE: Rgb = Rgb([0x12, 0x12, 0x12]);
/// The most luminous a navigation colour may be in dark mode, so the chrome stays dark there.
const MAX_DARK_NAV_LUMINANCE: f64 = 0.03;
/// How much of a dark navigation colour tints the light-mode sidebar: `--sb-tint` in
/// 000-tokens.css, which brands get.
const SIDEBAR_TINT: f64 = 0.08;
/// How far a light navigation colour (the bright presets) is lightened for the light-mode sidebar,
/// so the sidebar stays a lighter step of the chrome.
const LIGHT_NAV_SIDEBAR_LIFT: f64 = 0.5;
/// Steps of a contrast adjustment: each mixes a little more toward black or white.
const ADJUST_STEP: f64 = 0.04;
/// The length of a share code: four `#rrggbb` colours and three commas.
pub const MAX_SHARE_CODE_CHARS: usize = 64;

/// What the user chose. Serialized as one string, so it syncs as one setting: `brand`,
/// `preset:<id>` or `custom:<share code>`. A value this build doesn't know (a newer client's
/// preset or kind) is kept as it is and shows the brand colours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ColorTheme {
  /// The server's brand colours, or the product's own without a brand.
  #[default]
  Brand,
  Preset(PresetId),
  Custom(ThemeColors),
  /// A value from a newer client.
  Unknown(String),
}

/// The four colours every theme is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeColors {
  /// The sidebar, and (a little darker) the rail and the top bar.
  pub navigation: Rgb,
  /// The selected row in the sidebar; the accent (links, buttons, focus) is derived from it.
  pub selection: Rgb,
  /// The "online" dot.
  pub presence: Rgb,
  /// The unread and mention badges.
  pub notification: Rgb,
}

/// A colour in sRGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub [u8; 3]);

/// The colours a theme sets, for one of light or dark mode, each checked for contrast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeTokens {
  /// The navigation colour, moved away from its text until that reads: the dark-mode sidebar, and
  /// the mobile apps' header.
  pub navigation: Rgb,
  /// The rail and the top bar: the navigation colour a little darker.
  pub chrome: Rgb,
  /// Icons and text on the chrome and on [`Self::navigation`]: 4.5:1 on both.
  pub chrome_text: Rgb,
  /// The strongest text on the chrome (the brand name, active icons).
  pub chrome_strong: Rgb,
  /// Light in light mode (a tint of the navigation colour), dark in dark mode.
  pub sidebar: Rgb,
  /// Names and icons in the sidebar: 4.5:1.
  pub sidebar_text: Rgb,
  /// The strongest text in the sidebar (unread names, titles).
  pub sidebar_strong: Rgb,
  /// Secondary text in the sidebar (section titles, read channels): 4.5:1.
  pub sidebar_muted: Rgb,
  /// Whether the sidebar is light: hover and fills on it are a translucent black, else white.
  pub is_light_sidebar: bool,
  /// The selected row; white text on it is 4.5:1.
  pub selected: Rgb,
  /// Links, buttons and the focus ring: 4.5:1 on the content surfaces.
  pub accent: Rgb,
  pub accent_strong: Rgb,
  /// Text on the accent: 4.5:1.
  pub on_accent: Rgb,
  /// A tint of the accent behind content (selected list rows, chips): text stays 4.5:1 on it.
  pub accent_soft: Rgb,
  /// 3:1 on the content surfaces.
  pub presence: Rgb,
  /// White numbers on it are 4.5:1.
  pub badge: Rgb,
}

/// The default theme of a brand and what its people may pick (docs/branding.md §5.1). [`Default`]
/// restricts nothing, which is what a server without a policy means.
///
/// A default theme is colours like any other: its tokens come from [`ThemeColors::tokens`], so an
/// admin's default can't make text unreadable either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemePolicy {
  /// What [`ColorTheme::Brand`] and a theme that isn't allowed show.
  pub default_theme: ColorTheme,
  pub allowed_presets: AllowedPresets,
  pub is_custom_allowed: bool,
}

/// Which presets a policy allows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowedPresets {
  All,
  /// Preset ids this build knows.
  Only(BTreeSet<String>),
}

/// A built-in theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preset {
  pub id: PresetId,
  pub group: PresetGroup,
  pub colors: ThemeColors,
}

/// The id of a preset: its wire name, and the end of its i18n key (`color-theme-<id>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresetId(&'static str);

/// How the settings group the presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresetGroup {
  /// Dark sidebars in quiet colours.
  Calm,
  /// Light sidebars.
  Bright,
  /// Saturated colours.
  Vivid,
  /// Colours told apart with colour vision deficiencies, and high contrast.
  Accessible,
}

/// Why a custom theme or share code was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ColorThemeError {
  #[error("a share code has four colours, separated by commas")]
  WrongCount,
  #[error("not a hex colour (#rgb or #rrggbb)")]
  NotHex,
  #[error("the share code is too long")]
  TooLong,
}

/// Why an admin's theme policy was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ThemePolicyError {
  #[error("default_theme must be `brand`, `preset:<id>` of a known preset or `custom:<four colours>`")]
  UnknownDefaultTheme,
  #[error("allowed_presets names a preset that doesn't exist")]
  UnknownPreset,
}

/// The built-in themes, in the order of the settings. Original names and colours.
pub const PRESETS: &[Preset] = &[
  preset(
    "harbor",
    PresetGroup::Calm,
    0x1b2233,
    0x2f66e0,
    0x2eb67d,
    0xd1453b,
  ),
  preset(
    "graphite",
    PresetGroup::Calm,
    0x26262a,
    0x5b5b66,
    0x3fb37f,
    0xd9483b,
  ),
  preset("pine", PresetGroup::Calm, 0x1d3328, 0x2b7a50, 0x5ccf86, 0xd4553a),
  preset(
    "deep-sea",
    PresetGroup::Calm,
    0x0f3550,
    0x1d6fb0,
    0x34c3a0,
    0xe0533d,
  ),
  preset(
    "heather",
    PresetGroup::Calm,
    0x34203a,
    0x7d3f96,
    0x2eb67d,
    0xe2574c,
  ),
  preset(
    "paper",
    PresetGroup::Bright,
    0xf1f1f1,
    0x2f6fe0,
    0x2eb67d,
    0xd1453b,
  ),
  preset(
    "dune",
    PresetGroup::Bright,
    0xefe6d8,
    0xa55a26,
    0x2e9e6a,
    0xc9402f,
  ),
  preset(
    "meadow",
    PresetGroup::Bright,
    0xe3f1e8,
    0x1f7f53,
    0x2e9e6a,
    0xc9402f,
  ),
  preset(
    "lavender",
    PresetGroup::Bright,
    0xebe6f4,
    0x6446b8,
    0x2e9e6a,
    0xc9402f,
  ),
  preset(
    "ember",
    PresetGroup::Vivid,
    0x4a1d12,
    0xc4481f,
    0x5ccf86,
    0xe8a317,
  ),
  preset(
    "orchid",
    PresetGroup::Vivid,
    0x4a1030,
    0xc22a63,
    0x5ccf86,
    0xe07b2a,
  ),
  preset(
    "lagoon",
    PresetGroup::Vivid,
    0x053b3b,
    0x0d8780,
    0x7bd389,
    0xe0533d,
  ),
  preset(
    "indigo",
    PresetGroup::Vivid,
    0x1e1b4b,
    0x5145cd,
    0x34c3a0,
    0xe0533d,
  ),
  // Okabe–Ito colours: blue and orange stay apart without red–green vision.
  preset(
    "red-green-safe",
    PresetGroup::Accessible,
    0x1d2430,
    0x0072b2,
    0x56b4e9,
    0xd55e00,
  ),
  // Red and teal stay apart without blue–yellow vision.
  preset(
    "blue-yellow-safe",
    PresetGroup::Accessible,
    0x262626,
    0xb8285b,
    0x009e88,
    0xd0343a,
  ),
  preset(
    "high-contrast",
    PresetGroup::Accessible,
    0x000000,
    0x1a4fd6,
    0x00a86b,
    0xc8102e,
  ),
];

impl ColorTheme {
  /// The four colours to derive the tokens from; `None` for the brand colours (and an unknown
  /// value, which shows them too).
  pub fn colors(&self) -> Option<ThemeColors> {
    match self {
      Self::Brand | Self::Unknown(_) => None,
      Self::Preset(id) => Preset::find(id.as_str()).map(|preset| preset.colors),
      Self::Custom(colors) => Some(*colors),
    }
  }

  /// The CSS custom properties to set over the brand's, for light or dark mode; empty for the
  /// brand colours.
  pub fn css_properties(&self, is_dark: bool) -> Vec<(&'static str, String)> {
    self
      .colors()
      .map(|colors| colors.tokens(is_dark).css_properties())
      .unwrap_or_default()
  }
}

impl ThemePolicy {
  /// The policy of a brand as the client takes it: what this build doesn't know (a newer server's
  /// preset) is left out, and a default it doesn't know shows the brand's colours.
  pub fn from_wire(policy: &BrandThemePolicy) -> Self {
    let Ok(default_theme) = policy.default_theme.parse();
    Self {
      default_theme: match default_theme {
        ColorTheme::Unknown(_) => ColorTheme::Brand,
        known => known,
      },
      allowed_presets: match &policy.maybe_allowed_presets {
        None => AllowedPresets::All,
        Some(ids) => AllowedPresets::Only(
          ids
            .iter()
            .filter(|id| Preset::find(id).is_some())
            .cloned()
            .collect(),
        ),
      },
      is_custom_allowed: policy.is_custom_allowed,
    }
  }

  /// The policy as the server would send it.
  pub fn to_wire(&self) -> BrandThemePolicy {
    BrandThemePolicy {
      default_theme: self.default_theme.to_string(),
      maybe_allowed_presets: match &self.allowed_presets {
        AllowedPresets::All => None,
        AllowedPresets::Only(ids) => Some(ids.iter().cloned().collect()),
      },
      is_custom_allowed: self.is_custom_allowed,
    }
  }

  /// Checks an admin's policy (console and configuration file): the default must be a real theme
  /// and every allowed preset a preset of this build.
  pub fn check(policy: &BrandThemePolicy) -> Result<(), ThemePolicyError> {
    if matches!(policy.default_theme.parse(), Ok(ColorTheme::Unknown(_))) {
      return Err(ThemePolicyError::UnknownDefaultTheme);
    }
    let are_known = policy
      .maybe_allowed_presets
      .iter()
      .flatten()
      .all(|id| Preset::find(id).is_some());
    if !are_known {
      return Err(ThemePolicyError::UnknownPreset);
    }
    Ok(())
  }

  pub fn allows_preset(&self, id: &str) -> bool {
    match &self.allowed_presets {
      AllowedPresets::All => true,
      AllowedPresets::Only(ids) => ids.contains(id),
    }
  }

  /// Whether people can pick anything besides the default.
  pub fn has_choices(&self) -> bool {
    self.is_custom_allowed
      || match &self.allowed_presets {
        AllowedPresets::All => !PRESETS.is_empty(),
        AllowedPresets::Only(ids) => !ids.is_empty(),
      }
  }

  /// What a person's choice comes to here: the choice if this policy allows it, else the server's
  /// default ([`ColorTheme::Brand`]). The stored choice is the caller's and stays as it is.
  pub fn in_force(&self, chosen: &ColorTheme) -> ColorTheme {
    // Without a policy everything is as before it existed, a newer client's value included.
    if *self == Self::default() {
      return chosen.clone();
    }
    let is_allowed = match chosen {
      ColorTheme::Brand => true,
      ColorTheme::Preset(id) => self.allows_preset(id.as_str()),
      ColorTheme::Custom(_) => self.is_custom_allowed,
      ColorTheme::Unknown(_) => false,
    };
    if is_allowed {
      chosen.clone()
    } else {
      ColorTheme::Brand
    }
  }

  /// The theme to show for a person's choice: [`Self::in_force`], with the server's default
  /// replaced by the policy's default theme.
  pub fn effective(&self, chosen: &ColorTheme) -> ColorTheme {
    match self.in_force(chosen) {
      ColorTheme::Brand => self.default_theme.clone(),
      in_force => in_force,
    }
  }
}

impl Default for ThemePolicy {
  fn default() -> Self {
    Self {
      default_theme: ColorTheme::Brand,
      allowed_presets: AllowedPresets::All,
      is_custom_allowed: true,
    }
  }
}

impl fmt::Display for ColorTheme {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Brand => f.write_str("brand"),
      Self::Preset(id) => write!(f, "preset:{}", id.as_str()),
      Self::Custom(colors) => write!(f, "custom:{}", colors.share_code()),
      Self::Unknown(value) => f.write_str(value),
    }
  }
}

impl FromStr for ColorTheme {
  type Err = std::convert::Infallible;

  /// Never fails: what this build doesn't know is kept as [`ColorTheme::Unknown`].
  fn from_str(value: &str) -> Result<Self, Self::Err> {
    let parsed = match value.split_once(':') {
      None if value == "brand" => Some(Self::Brand),
      Some(("preset", id)) => Preset::find(id).map(|preset| Self::Preset(preset.id)),
      Some(("custom", code)) => ThemeColors::from_share_code(code)
        .ok()
        // Only a canonical code: anything else would not write back the same value.
        .filter(|colors| colors.share_code() == code)
        .map(Self::Custom),
      _ => None,
    };
    Ok(parsed.unwrap_or_else(|| Self::Unknown(value.to_owned())))
  }
}

impl Serialize for ColorTheme {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(self)
  }
}

impl<'de> Deserialize<'de> for ColorTheme {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let value = String::deserialize(deserializer)?;
    let Ok(theme) = value.parse();
    Ok(theme)
  }
}

impl ThemeColors {
  /// Reads a share code: four hex colours separated by commas (or spaces), `#` optional, as people
  /// paste them.
  pub fn from_share_code(code: &str) -> Result<Self, ColorThemeError> {
    if code.chars().count() > MAX_SHARE_CODE_CHARS {
      return Err(ColorThemeError::TooLong);
    }
    let parts: Vec<&str> = code
      .split(|c: char| c == ',' || c.is_whitespace())
      .filter(|part| !part.is_empty())
      .collect();
    let [navigation, selection, presence, notification] = parts.as_slice() else {
      return Err(ColorThemeError::WrongCount);
    };
    Ok(Self {
      navigation: navigation.parse()?,
      selection: selection.parse()?,
      presence: presence.parse()?,
      notification: notification.parse()?,
    })
  }

  /// A harmonious theme around one hue (0 to 360 degrees), for "Surprise me": a deep sidebar and a
  /// lively selection of that hue, green presence and a red badge. The caller picks the hue.
  pub fn from_hue(hue: f64) -> Self {
    let hue = hue.rem_euclid(360.0);
    Self {
      navigation: Rgb::from_hsl(hue, 0.38, 0.17),
      selection: Rgb::from_hsl(hue + 20.0, 0.62, 0.46),
      presence: Rgb::from_hsl(150.0, 0.6, 0.42),
      notification: Rgb::from_hsl(6.0, 0.68, 0.5),
    }
  }

  /// The share code: `#rrggbb,#rrggbb,#rrggbb,#rrggbb` (navigation, selection, presence,
  /// notifications).
  pub fn share_code(&self) -> String {
    format!(
      "{},{},{},{}",
      self.navigation, self.selection, self.presence, self.notification
    )
  }

  /// The derived colours for light or dark mode, with contrast enforced: every colour that fails
  /// is moved toward black or white until it passes.
  pub fn tokens(&self, is_dark: bool) -> ThemeTokens {
    let navigation = if is_dark {
      darken_nav(self.navigation)
    } else {
      self.navigation
    };
    // The navigation colour as before #202's light sidebar: light or dark, moved away from its
    // text until that reads on it and on the darker chrome. In dark mode it is the sidebar.
    let is_light_nav = INK.contrast(navigation) > WHITE.contrast(navigation);
    let (ink, away) = if is_light_nav {
      (INK, WHITE)
    } else {
      (WHITE, BLACK)
    };
    let chrome_of = |sidebar: Rgb| {
      if is_light_nav {
        sidebar.mix(BLACK, 0.06)
      } else {
        sidebar.mix(BLACK, 0.25)
      }
    };
    let nav = navigation.toward(away, |nav| {
      ink.contrast(nav) >= TEXT_CONTRAST && ink.contrast(chrome_of(nav)) >= TEXT_CONTRAST
    });
    let chrome = chrome_of(nav);
    let (chrome_text, chrome_strong) = text_on(ink, &[nav, chrome]);
    let sidebar = match (is_dark, is_light_nav) {
      (true, _) => nav,
      (false, true) => nav.mix(WHITE, LIGHT_NAV_SIDEBAR_LIFT),
      (false, false) => WHITE.mix(nav, SIDEBAR_TINT),
    };
    let is_light_sidebar = INK.contrast(sidebar) > WHITE.contrast(sidebar);
    let sidebar_ink = if is_light_sidebar { INK } else { WHITE };
    let (sidebar_text, sidebar_strong) = text_on(sidebar_ink, &[sidebar]);
    let sidebar_muted = sidebar_ink
      .mix(sidebar, 0.36)
      .toward(sidebar_ink, |color| color.contrast(sidebar) >= TEXT_CONTRAST);
    let selected = self
      .selection
      .toward(BLACK, |color| WHITE.contrast(color) >= TEXT_CONTRAST);
    let (accent, accent_strong, on_accent, accent_soft) = if is_dark {
      let accent = self.selection.toward(WHITE, |color| {
        color.contrast(DARK_SURFACE) >= TEXT_CONTRAST && color.contrast(DARK_PAGE) >= TEXT_CONTRAST
      });
      let on_accent = if BLACK.contrast(accent) >= WHITE.contrast(accent) {
        accent.mix(BLACK, 0.9)
      } else {
        WHITE
      };
      let soft = accent.mix(DARK_PAGE, 0.78);
      (accent, accent.mix(WHITE, 0.3), on_accent, soft)
    } else {
      let accent = self.selection.toward(BLACK, |color| {
        color.contrast(LIGHT_SURFACE) >= TEXT_CONTRAST && color.contrast(LIGHT_PAGE) >= TEXT_CONTRAST
      });
      let soft = accent.mix(LIGHT_PAGE, 0.88);
      (accent, accent.mix(BLACK, 0.15), WHITE, soft)
    };
    let (page, surface, away) = if is_dark {
      (DARK_PAGE, DARK_SURFACE, WHITE)
    } else {
      (LIGHT_PAGE, LIGHT_SURFACE, BLACK)
    };
    let presence = self.presence.toward(away, |color| {
      color.contrast(page) >= MARK_CONTRAST && color.contrast(surface) >= MARK_CONTRAST
    });
    let badge = self
      .notification
      .toward(BLACK, |color| WHITE.contrast(color) >= TEXT_CONTRAST);
    ThemeTokens {
      navigation: nav,
      chrome,
      chrome_text,
      chrome_strong,
      sidebar,
      sidebar_text,
      sidebar_strong,
      sidebar_muted,
      is_light_sidebar,
      selected,
      accent,
      accent_strong,
      on_accent,
      accent_soft,
      presence,
      badge,
    }
  }
}

impl ThemeTokens {
  /// The UI's CSS custom properties (000-tokens.css) these colours set.
  pub fn css_properties(&self) -> Vec<(&'static str, String)> {
    // The same translucent layers as 000-tokens.css for a light or a dark sidebar.
    let [hover, fill, fill_strong, edge, border] = if self.is_light_sidebar {
      [
        "rgba(0, 0, 0, 0.06)",
        "rgba(0, 0, 0, 0.05)",
        "rgba(0, 0, 0, 0.09)",
        "rgba(0, 0, 0, 0.12)",
        "rgba(0, 0, 0, 0.08)",
      ]
    } else {
      [
        "rgba(255, 255, 255, 0.07)",
        "rgba(255, 255, 255, 0.08)",
        "rgba(255, 255, 255, 0.14)",
        "rgba(255, 255, 255, 0.14)",
        "rgba(0, 0, 0, 0)",
      ]
    };
    vec![
      ("--chrome", self.chrome.to_string()),
      ("--chrome-text", self.chrome_text.to_string()),
      ("--chrome-strong", self.chrome_strong.to_string()),
      ("--sb-bg", self.sidebar.to_string()),
      ("--sb-border", border.to_owned()),
      ("--sb-text", self.sidebar_text.to_string()),
      ("--sb-strong", self.sidebar_strong.to_string()),
      ("--sb-hover", hover.to_owned()),
      ("--sb-fill", fill.to_owned()),
      ("--sb-fill-strong", fill_strong.to_owned()),
      ("--sb-edge", edge.to_owned()),
      ("--sb-current", self.selected.to_string()),
      ("--sb-muted", self.sidebar_muted.to_string()),
      ("--sb-dim", self.sidebar_muted.to_string()),
      ("--accent", self.accent.to_string()),
      ("--focus", self.accent.to_string()),
      ("--accent-strong", self.accent_strong.to_string()),
      ("--on-accent", self.on_accent.to_string()),
      ("--accent-soft", self.accent_soft.to_string()),
      ("--online", self.presence.to_string()),
      ("--badge-bg", self.badge.to_string()),
    ]
  }
}

impl Preset {
  /// The preset with this id.
  pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id.as_str() == id)
  }
}

impl PresetId {
  pub const fn as_str(self) -> &'static str {
    self.0
  }
}

impl PresetGroup {
  pub const ALL: [Self; 4] = [Self::Calm, Self::Bright, Self::Vivid, Self::Accessible];

  /// The end of its i18n key (`color-theme-group-<slug>`).
  pub const fn slug(self) -> &'static str {
    match self {
      Self::Calm => "calm",
      Self::Bright => "bright",
      Self::Vivid => "vivid",
      Self::Accessible => "accessible",
    }
  }
}

impl Rgb {
  /// Relative luminance (WCAG 2.x).
  pub fn luminance(self) -> f64 {
    let channel = |value: u8| {
      let c = f64::from(value) / 255.0;
      if c <= 0.039_28 {
        c / 12.92
      } else {
        ((c + 0.055) / 1.055).powf(2.4)
      }
    };
    let [r, g, b] = self.0;
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
  }

  /// Contrast ratio (WCAG 2.x), 1 to 21.
  pub fn contrast(self, other: Rgb) -> f64 {
    let (a, b) = (self.luminance(), other.luminance());
    let (light, dark) = if a > b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
  }

  /// `amount` (0 to 1) of `other` mixed into this colour.
  pub fn mix(self, other: Rgb, amount: f64) -> Rgb {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
      let value = f64::from(a) + (f64::from(b) - f64::from(a)) * amount;
      // Invariant: between two u8 values, rounded.
      value.round().clamp(0.0, 255.0) as u8
    };
    let ([r1, g1, b1], [r2, g2, b2]) = (self.0, other.0);
    Rgb([channel(r1, r2), channel(g1, g2), channel(b1, b2)])
  }

  /// From hue (degrees), saturation and lightness (0 to 1).
  pub fn from_hsl(hue: f64, saturation: f64, lightness: f64) -> Self {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let second = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (r, g, b) = match hue {
      h if h < 1.0 => (chroma, second, 0.0),
      h if h < 2.0 => (second, chroma, 0.0),
      h if h < 3.0 => (0.0, chroma, second),
      h if h < 4.0 => (0.0, second, chroma),
      h if h < 5.0 => (second, 0.0, chroma),
      _ => (chroma, 0.0, second),
    };
    let offset = lightness - chroma / 2.0;
    // Invariant: each channel is between 0 and 1 before scaling, clamped anyway.
    let channel = |value: f64| ((value + offset) * 255.0).round().clamp(0.0, 255.0) as u8;
    Self([channel(r), channel(g), channel(b)])
  }

  /// This colour if it passes, else the first step toward `target` that does (`target` itself at
  /// the end, which the callers' checks always accept).
  fn toward(self, target: Rgb, passes: impl Fn(Rgb) -> bool) -> Rgb {
    let mut amount = 0.0;
    while amount < 1.0 {
      let candidate = self.mix(target, amount);
      if passes(candidate) {
        return candidate;
      }
      amount += ADJUST_STEP;
    }
    target
  }
}

/// Text and strong text in `ink`'s direction that read at 4.5:1 on every background: the text a
/// little softer than the ink, the strong text the ink itself.
fn text_on(ink: Rgb, backgrounds: &[Rgb]) -> (Rgb, Rgb) {
  let readable = |color: Rgb| {
    backgrounds
      .iter()
      .all(|background| color.contrast(*background) >= TEXT_CONTRAST)
  };
  let background = backgrounds.first().copied().unwrap_or(ink);
  (ink.mix(background, 0.18).toward(ink, readable), ink)
}

/// A navigation colour for dark mode: dark ones stay, light ones are darkened toward a neutral
/// near-black, keeping a trace of their hue.
fn darken_nav(navigation: Rgb) -> Rgb {
  navigation.toward(DARK_NAV_BASE, |color| color.luminance() <= MAX_DARK_NAV_LUMINANCE)
}

const fn preset(
  id: &'static str,
  group: PresetGroup,
  navigation: u32,
  selection: u32,
  presence: u32,
  notification: u32,
) -> Preset {
  Preset {
    id: PresetId(id),
    group,
    colors: ThemeColors {
      navigation: Rgb::from_u32(navigation),
      selection: Rgb::from_u32(selection),
      presence: Rgb::from_u32(presence),
      notification: Rgb::from_u32(notification),
    },
  }
}

impl Rgb {
  const fn from_u32(value: u32) -> Self {
    Self([(value >> 16) as u8, (value >> 8) as u8, value as u8])
  }
}

impl fmt::Display for Rgb {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let [r, g, b] = self.0;
    write!(f, "#{r:02x}{g:02x}{b:02x}")
  }
}

impl FromStr for Rgb {
  type Err = ColorThemeError;

  /// `#rgb` or `#rrggbb`, the `#` optional, any case.
  fn from_str(value: &str) -> Result<Self, Self::Err> {
    let digits = value.trim().trim_start_matches('#');
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
      return Err(ColorThemeError::NotHex);
    }
    let full = match digits.len() {
      3 => digits.chars().flat_map(|c| [c, c]).collect::<String>(),
      6 => digits.to_owned(),
      _ => return Err(ColorThemeError::NotHex),
    };
    let value = u32::from_str_radix(&full, 16).map_err(|_| ColorThemeError::NotHex)?;
    Ok(Self::from_u32(value))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CODE: &str = "#1b2233,#2f66e0,#2eb67d,#d1453b";

  mod wire {
    use super::*;

    #[test]
    fn values_round_trip() {
      for value in [
        "brand",
        "preset:harbor",
        "preset:high-contrast",
        &format!("custom:{CODE}"),
      ] {
        let theme: ColorTheme = value.parse().unwrap();
        assert!(!matches!(theme, ColorTheme::Unknown(_)), "{value}");
        assert_eq!(theme.to_string(), value);
        let json = serde_json::to_value(&theme).unwrap();
        assert_eq!(json, serde_json::json!(value));
        assert_eq!(serde_json::from_value::<ColorTheme>(json).unwrap(), theme);
      }
    }

    #[test]
    fn unknown_values_are_kept_and_show_the_brand() {
      for value in [
        "preset:from-the-future",
        "gradient:#000",
        "",
        "custom:#fff,#000,#fff,#000",
      ] {
        let theme: ColorTheme = value.parse().unwrap();
        assert_eq!(theme, ColorTheme::Unknown(value.to_owned()));
        assert_eq!(theme.to_string(), value);
        assert!(theme.css_properties(false).is_empty());
      }
    }

    #[test]
    fn the_default_is_the_brand() {
      assert_eq!(ColorTheme::default(), ColorTheme::Brand);
      assert!(ColorTheme::Brand.css_properties(true).is_empty());
    }
  }

  mod share_code {
    use super::*;

    #[test]
    fn reads_what_people_paste() {
      let canonical = ThemeColors::from_share_code(CODE).unwrap();
      for code in [
        "1B2233, 2F66E0, 2EB67D, D1453B",
        "#1b2233 #2f66e0 #2eb67d #d1453b",
        " #1b2233,#2f66e0,#2eb67d,#d1453b ",
      ] {
        assert_eq!(ThemeColors::from_share_code(code).unwrap(), canonical, "{code}");
      }
      assert_eq!(canonical.share_code(), CODE);
      let short = ThemeColors::from_share_code("#000,#fff,#0f0,#f00").unwrap();
      assert_eq!(short.share_code(), "#000000,#ffffff,#00ff00,#ff0000");
    }

    #[test]
    fn refuses_anything_else() {
      assert_eq!(ThemeColors::from_share_code(""), Err(ColorThemeError::WrongCount));
      assert_eq!(
        ThemeColors::from_share_code("#000,#fff,#0f0"),
        Err(ColorThemeError::WrongCount)
      );
      assert_eq!(
        ThemeColors::from_share_code("#000,#fff,#0f0,#f00,#00f"),
        Err(ColorThemeError::WrongCount)
      );
      for bad in ["red", "#12345", "#gggggg", "url(x)", "#00000;"] {
        assert_eq!(
          ThemeColors::from_share_code(&format!("{bad},#fff,#0f0,#f00")),
          Err(ColorThemeError::NotHex),
          "{bad}"
        );
      }
      assert_eq!(
        ThemeColors::from_share_code(&"#000,".repeat(20)),
        Err(ColorThemeError::TooLong)
      );
    }
  }

  mod contrast {
    use super::*;

    #[test]
    fn every_preset_and_extreme_passes_in_both_modes() {
      let extremes = [
        WHITE,
        BLACK,
        Rgb([0xff, 0xff, 0x00]),
        Rgb([0x80, 0x80, 0x80]),
        Rgb([0x00, 0x00, 0xff]),
      ];
      let mut themes: Vec<ThemeColors> = PRESETS.iter().map(|preset| preset.colors).collect();
      for navigation in extremes {
        for selection in extremes {
          themes.push(ThemeColors {
            navigation,
            selection,
            presence: selection,
            notification: navigation,
          });
        }
      }
      for colors in themes {
        for is_dark in [false, true] {
          assert_readable(&colors.tokens(is_dark), is_dark, &colors);
        }
      }
    }

    #[test]
    fn dark_mode_keeps_the_chrome_dark() {
      let paper = Preset::find("paper").unwrap().colors;
      let tokens = paper.tokens(true);
      assert!(tokens.sidebar.luminance() <= MAX_DARK_NAV_LUMINANCE);
      assert_eq!(tokens.sidebar, tokens.navigation);
      assert!(!tokens.is_light_sidebar);
      let light = paper.tokens(false);
      assert_eq!(light.navigation, paper.navigation);
      assert!(light.is_light_sidebar);
    }

    #[test]
    fn light_mode_has_a_light_sidebar_and_the_colour_on_the_chrome() {
      for preset in PRESETS {
        let tokens = preset.colors.tokens(false);
        assert!(tokens.is_light_sidebar, "{}", preset.id.as_str());
        assert!(
          tokens.sidebar.luminance() > tokens.chrome.luminance(),
          "{}",
          preset.id.as_str()
        );
        // The selected row stands out from the sidebar (WCAG 1.4.11).
        assert!(
          tokens.selected.contrast(tokens.sidebar) >= MARK_CONTRAST,
          "{}",
          preset.id.as_str()
        );
      }
      let harbor = Preset::find("harbor").unwrap().colors;
      let tokens = harbor.tokens(false);
      assert_eq!(tokens.navigation, harbor.navigation);
      assert_eq!(tokens.sidebar, WHITE.mix(harbor.navigation, SIDEBAR_TINT));
    }

    #[test]
    fn readable_colours_stay_as_chosen() {
      let colors = ThemeColors::from_share_code(CODE).unwrap();
      let tokens = colors.tokens(false);
      assert_eq!(tokens.navigation, colors.navigation);
      assert_eq!(tokens.selected, colors.selection);
      assert_eq!(tokens.badge, colors.notification);
    }

    pub(super) fn assert_readable(tokens: &ThemeTokens, is_dark: bool, colors: &ThemeColors) {
      let context = format!("{} dark={is_dark}", colors.share_code());
      for (text, background) in [
        (tokens.chrome_text, tokens.chrome),
        (tokens.chrome_text, tokens.navigation),
        (tokens.chrome_strong, tokens.chrome),
        (tokens.sidebar_text, tokens.sidebar),
        (tokens.sidebar_muted, tokens.sidebar),
        (tokens.sidebar_strong, tokens.sidebar),
        (WHITE, tokens.selected),
        (WHITE, tokens.badge),
        (tokens.on_accent, tokens.accent),
      ] {
        assert!(
          text.contrast(background) >= TEXT_CONTRAST,
          "{context}: {text} on {background}"
        );
      }
      let (page, surface, text) = if is_dark {
        (DARK_PAGE, DARK_SURFACE, Rgb([0xf5, 0xf5, 0xf5]))
      } else {
        (LIGHT_PAGE, LIGHT_SURFACE, Rgb([0x1d, 0x1d, 0x1d]))
      };
      for background in [page, surface] {
        assert!(
          tokens.accent.contrast(background) >= TEXT_CONTRAST,
          "{context}: accent"
        );
        assert!(
          tokens.presence.contrast(background) >= MARK_CONTRAST,
          "{context}: presence"
        );
      }
      assert!(
        text.contrast(tokens.accent_soft) >= TEXT_CONTRAST,
        "{context}: soft"
      );
    }
  }

  #[test]
  fn css_properties_are_known_tokens_with_safe_values() {
    let properties = ColorTheme::Preset(PRESETS[0].id).css_properties(false);
    assert_eq!(properties.len(), 21);
    for (name, value) in properties {
      assert!(name.starts_with("--"));
      assert!(
        teamcraft_proto::is_brand_hex_color(&value) || value.starts_with("rgba("),
        "{name}: {value}"
      );
    }
  }

  #[test]
  fn hsl_and_hue_themes() {
    assert_eq!(Rgb::from_hsl(0.0, 1.0, 0.5), Rgb([0xff, 0, 0]));
    assert_eq!(Rgb::from_hsl(120.0, 1.0, 0.5), Rgb([0, 0xff, 0]));
    assert_eq!(Rgb::from_hsl(240.0, 1.0, 0.5), Rgb([0, 0, 0xff]));
    assert_eq!(Rgb::from_hsl(0.0, 0.0, 1.0), WHITE);
    let theme = ThemeColors::from_hue(400.0);
    assert_eq!(theme, ThemeColors::from_hue(40.0));
    assert_eq!(theme.tokens(false).navigation, theme.navigation);
  }

  fn policy(default_theme: &str, presets: Option<&[&str]>, is_custom_allowed: bool) -> ThemePolicy {
    ThemePolicy::from_wire(&BrandThemePolicy {
      default_theme: default_theme.to_owned(),
      maybe_allowed_presets: presets.map(|ids| ids.iter().map(|id| (*id).to_owned()).collect()),
      is_custom_allowed,
    })
  }

  fn theme(value: &str) -> ColorTheme {
    value.parse().unwrap()
  }

  #[test]
  fn without_a_policy_every_choice_stays_as_it_was() {
    let none = ThemePolicy::default();
    for value in [
      "brand",
      "preset:pine",
      "custom:#112233,#445566,#778899,#aabbcc",
      "preset:from-a-newer-client",
    ] {
      assert_eq!(none.in_force(&theme(value)), theme(value));
      assert_eq!(none.effective(&theme(value)), theme(value));
    }
    assert_eq!(ThemePolicy::default().to_wire(), BrandThemePolicy::default());
  }

  #[test]
  fn a_choice_the_policy_no_longer_allows_shows_the_default_and_nothing_else_changes() {
    let fixed = policy("preset:harbor", Some(&["pine"]), false);
    let stored = theme("preset:paper");
    assert_eq!(fixed.effective(&stored), theme("preset:harbor"));
    assert_eq!(fixed.in_force(&stored), ColorTheme::Brand);
    // The stored value is untouched; under a brand that allows it, it shows again.
    assert_eq!(stored, theme("preset:paper"));
    assert_eq!(ThemePolicy::default().effective(&stored), stored);
    // Allowed ones stay, the default stays the default, custom and unknown fall back.
    assert_eq!(fixed.effective(&theme("preset:pine")), theme("preset:pine"));
    assert_eq!(fixed.effective(&ColorTheme::Brand), theme("preset:harbor"));
    assert_eq!(
      fixed.effective(&theme("custom:#112233,#445566,#778899,#aabbcc")),
      theme("preset:harbor")
    );
    assert_eq!(
      fixed.effective(&theme("preset:from-a-newer-client")),
      theme("preset:harbor")
    );
  }

  #[test]
  fn a_policy_without_choices_is_a_fixed_look() {
    assert!(!policy("brand", Some(&[]), false).has_choices());
    assert!(policy("brand", Some(&[]), true).has_choices());
    assert!(policy("brand", Some(&["pine"]), false).has_choices());
    assert!(policy("brand", None, false).has_choices());
  }

  #[test]
  fn what_this_build_does_not_know_stays_out_of_the_policy() {
    let newer = policy("preset:from-the-future", Some(&["pine", "from-the-future"]), true);
    assert_eq!(newer.default_theme, ColorTheme::Brand);
    assert!(newer.allows_preset("pine"));
    assert!(!newer.allows_preset("from-the-future"));
  }

  #[test]
  fn an_admin_policy_is_checked() {
    let good = BrandThemePolicy {
      default_theme: "custom:#000000,#000000,#000000,#000000".into(),
      maybe_allowed_presets: Some(vec!["pine".into()]),
      is_custom_allowed: false,
    };
    assert_eq!(ThemePolicy::check(&good), Ok(()));
    assert_eq!(
      ThemePolicy::check(&BrandThemePolicy {
        default_theme: "preset:nope".into(),
        ..good.clone()
      }),
      Err(ThemePolicyError::UnknownDefaultTheme)
    );
    assert_eq!(
      ThemePolicy::check(&BrandThemePolicy {
        default_theme: "custom:#000,#000,#000,#000".into(),
        ..good.clone()
      }),
      Err(ThemePolicyError::UnknownDefaultTheme)
    );
    assert_eq!(
      ThemePolicy::check(&BrandThemePolicy {
        maybe_allowed_presets: Some(vec!["nope".into()]),
        ..good
      }),
      Err(ThemePolicyError::UnknownPreset)
    );
  }

  #[test]
  fn even_a_hostile_default_theme_is_held_to_the_contrast_rules() {
    for code in [
      "custom:#ffffff,#ffffff,#ffffff,#ffffff",
      "custom:#000000,#000000,#000000,#000000",
      "custom:#ffff00,#ffff00,#ffff00,#ffff00",
    ] {
      let chosen = policy(code, Some(&[]), false).effective(&ColorTheme::Brand);
      let colors = chosen.colors().unwrap();
      for is_dark in [false, true] {
        contrast::assert_readable(&colors.tokens(is_dark), is_dark, &colors);
      }
    }
  }

  #[test]
  fn preset_ids_are_unique() {
    let mut ids: Vec<&str> = PRESETS.iter().map(|preset| preset.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), PRESETS.len());
  }
}
