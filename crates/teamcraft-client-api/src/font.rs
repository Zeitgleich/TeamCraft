//! The interface font (issue #1494): the system's own, one of the fonts the apps ship, or one of a
//! few common fonts a device may have installed.
//!
//! The choice follows the account (settings sync, e2ee.md §6.4) as its wire name
//! ([`UiFont::id`]). Shipped fonts load only when someone picks one (web: `teamcraft-ui` `fonts.rs`;
//! Android bundles them); installed ones are named and fall back to the system font where missing.
//! Font names are proper names and stay as they are in every language; only the system font's name
//! and the group headings are translated.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The font of the whole interface. A value this build doesn't know (a newer client's font) is
/// kept as it is and shows the system font.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum UiFont {
  /// The operating system's interface font.
  #[default]
  System,
  Inter,
  GoogleSansFlex,
  IbmPlexSans,
  SourceSans3,
  AtkinsonHyperlegibleNext,
  Lexend,
  OpenDyslexic,
  NotoSans,
  JetBrainsMono,
  /// Not shipped: shown where the device has it installed.
  Arial,
  Georgia,
  Verdana,
  /// A value from a newer client.
  Unknown(String),
}

/// How the settings group the fonts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontGroup {
  /// The system font and Inter, the suggested one.
  Suggested,
  /// Further sans-serif fonts the apps ship.
  Sans,
  /// Fonts designed for legibility and reading with dyslexia.
  Readability,
  /// A monospaced font.
  Monospace,
  /// Fonts the device may have installed; nothing is downloaded.
  Installed,
}

/// Where a font comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSource {
  /// The system's own font: nothing to load.
  System,
  /// Shipped with the apps: loaded when picked.
  Shipped,
  /// Named only; a device without it shows the system font.
  Installed,
}

impl UiFont {
  /// Every known font, in the order of the settings.
  pub const ALL: [Self; 13] = [
    Self::System,
    Self::Inter,
    Self::GoogleSansFlex,
    Self::IbmPlexSans,
    Self::SourceSans3,
    Self::NotoSans,
    Self::AtkinsonHyperlegibleNext,
    Self::Lexend,
    Self::OpenDyslexic,
    Self::JetBrainsMono,
    Self::Arial,
    Self::Georgia,
    Self::Verdana,
  ];

  /// The wire name, also the font's file name stem (`fonts/<id>.woff2`) and the end of its
  /// Android resource name (`font_<id with _>`).
  pub fn id(&self) -> &str {
    match self {
      Self::System => "system",
      Self::Inter => "inter",
      Self::GoogleSansFlex => "google-sans-flex",
      Self::IbmPlexSans => "ibm-plex-sans",
      Self::SourceSans3 => "source-sans-3",
      Self::AtkinsonHyperlegibleNext => "atkinson-hyperlegible-next",
      Self::Lexend => "lexend",
      Self::OpenDyslexic => "opendyslexic",
      Self::NotoSans => "noto-sans",
      Self::JetBrainsMono => "jetbrains-mono",
      Self::Arial => "arial",
      Self::Georgia => "georgia",
      Self::Verdana => "verdana",
      Self::Unknown(value) => value,
    }
  }

  /// The font's own name, as its makers spell it; `None` for the system font (translated, it
  /// has no name of its own) and an unknown value.
  pub fn display_name(&self) -> Option<&'static str> {
    let name = match self {
      Self::System | Self::Unknown(_) => return None,
      Self::Inter => "Inter",
      Self::GoogleSansFlex => "Google Sans Flex",
      Self::IbmPlexSans => "IBM Plex Sans",
      Self::SourceSans3 => "Source Sans 3",
      Self::AtkinsonHyperlegibleNext => "Atkinson Hyperlegible Next",
      Self::Lexend => "Lexend",
      Self::OpenDyslexic => "OpenDyslexic",
      Self::NotoSans => "Noto Sans",
      Self::JetBrainsMono => "JetBrains Mono",
      Self::Arial => "Arial",
      Self::Georgia => "Georgia",
      Self::Verdana => "Verdana",
    };
    Some(name)
  }

  pub fn group(&self) -> FontGroup {
    match self {
      Self::System | Self::Inter | Self::Unknown(_) => FontGroup::Suggested,
      Self::GoogleSansFlex | Self::IbmPlexSans | Self::SourceSans3 | Self::NotoSans => FontGroup::Sans,
      Self::AtkinsonHyperlegibleNext | Self::Lexend | Self::OpenDyslexic => FontGroup::Readability,
      Self::JetBrainsMono => FontGroup::Monospace,
      Self::Arial | Self::Georgia | Self::Verdana => FontGroup::Installed,
    }
  }

  pub fn source(&self) -> FontSource {
    match self {
      Self::System | Self::Unknown(_) => FontSource::System,
      Self::Arial | Self::Georgia | Self::Verdana => FontSource::Installed,
      _ => FontSource::Shipped,
    }
  }

  /// The generic CSS family behind the font, for the fallback stack.
  pub fn is_monospace(&self) -> bool {
    matches!(self, Self::JetBrainsMono)
  }

  pub fn is_serif(&self) -> bool {
    matches!(self, Self::Georgia)
  }
}

impl FontGroup {
  pub const ALL: [Self; 5] = [
    Self::Suggested,
    Self::Sans,
    Self::Readability,
    Self::Monospace,
    Self::Installed,
  ];

  /// The end of its i18n key (`font-group-<slug>`).
  pub const fn slug(self) -> &'static str {
    match self {
      Self::Suggested => "suggested",
      Self::Sans => "sans",
      Self::Readability => "readability",
      Self::Monospace => "monospace",
      Self::Installed => "installed",
    }
  }
}

impl fmt::Display for UiFont {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.id())
  }
}

impl FromStr for UiFont {
  type Err = std::convert::Infallible;

  /// Never fails: what this build doesn't know is kept as [`UiFont::Unknown`].
  fn from_str(value: &str) -> Result<Self, Self::Err> {
    let known = Self::ALL.into_iter().find(|font| font.id() == value);
    Ok(known.unwrap_or_else(|| Self::Unknown(value.to_owned())))
  }
}

impl Serialize for UiFont {
  fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(self.id())
  }
}

impl<'de> Deserialize<'de> for UiFont {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    let value = String::deserialize(deserializer)?;
    let Ok(font) = value.parse();
    Ok(font)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wire_names_round_trip() {
    for font in UiFont::ALL {
      let json = serde_json::to_value(&font).unwrap();
      assert_eq!(json, serde_json::json!(font.id()));
      assert_eq!(serde_json::from_value::<UiFont>(json).unwrap(), font);
    }
  }

  /// The wire names are stable: changing one would reset everybody who picked that font.
  #[test]
  fn wire_names_are_fixed() {
    let ids: Vec<&str> = UiFont::ALL.iter().map(UiFont::id).collect();
    assert_eq!(
      ids,
      [
        "system",
        "inter",
        "google-sans-flex",
        "ibm-plex-sans",
        "source-sans-3",
        "noto-sans",
        "atkinson-hyperlegible-next",
        "lexend",
        "opendyslexic",
        "jetbrains-mono",
        "arial",
        "georgia",
        "verdana",
      ]
    );
  }

  #[test]
  fn unknown_values_are_kept_and_show_the_system_font() {
    for value in ["comic-sans-from-the-future", "", "Inter"] {
      let font: UiFont = value.parse().unwrap();
      assert_eq!(font, UiFont::Unknown(value.to_owned()));
      assert_eq!(font.to_string(), value);
      assert_eq!(font.source(), FontSource::System);
      assert_eq!(font.display_name(), None);
    }
  }

  #[test]
  fn every_font_but_the_system_one_has_a_name() {
    for font in UiFont::ALL {
      assert_eq!(font.display_name().is_none(), font == UiFont::System, "{font}");
    }
  }

  #[test]
  fn groups_follow_the_order_of_the_list() {
    let groups: Vec<FontGroup> = UiFont::ALL.iter().map(UiFont::group).collect();
    let mut sorted = groups.clone();
    sorted.sort_by_key(|group| FontGroup::ALL.iter().position(|known| known == group));
    assert_eq!(groups, sorted);
  }
}
