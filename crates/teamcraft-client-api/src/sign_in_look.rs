//! What the UI tells the desktop shell when it starts a browser sign-in, so the page the shell's
//! loopback listener shows after the redirect matches the app: the UI language, the theme and the
//! server's brand. Everything is optional; an older UI bundle sends nothing and the page falls back
//! to the browser's language and color scheme.

use serde::{Deserialize, Serialize};

/// The app's look at the moment the user started the sign-in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInLook {
  /// The UI language as a BCP 47 tag (`de`, `en`).
  #[serde(rename = "locale", default, skip_serializing_if = "Option::is_none")]
  pub maybe_locale: Option<String>,
  #[serde(default)]
  pub theme: LookTheme,
  /// The server's brand name as the sign-in card shows it.
  #[serde(rename = "brand_name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_brand_name: Option<String>,
  /// The brand image (`data:` URL) for the light theme; the shell checks it before use.
  #[serde(rename = "logo", default, skip_serializing_if = "Option::is_none")]
  pub maybe_logo: Option<String>,
  /// The brand image for the dark theme, when the brand has a separate one.
  #[serde(rename = "logo_dark", default, skip_serializing_if = "Option::is_none")]
  pub maybe_logo_dark: Option<String>,
}

/// The theme the user picked in the app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookTheme {
  /// Follow the system (the browser's `prefers-color-scheme`).
  #[default]
  System,
  Light,
  Dark,
  /// A theme a newer UI knows; treated like [`LookTheme::System`].
  #[serde(other)]
  Unknown,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_empty_look_is_the_default() {
    let look: SignInLook = serde_json::from_str("{}").unwrap();
    assert_eq!(look, SignInLook::default());
    assert_eq!(serde_json::to_string(&look).unwrap(), r#"{"theme":"system"}"#);
  }

  #[test]
  fn the_wire_names_are_stable() {
    let look: SignInLook = serde_json::from_str(
      r#"{"locale":"de","theme":"dark","brand_name":"Example Chat","logo":"data:image/png;base64,AA=="}"#,
    )
    .unwrap();
    assert_eq!(look.maybe_locale.as_deref(), Some("de"));
    assert_eq!(look.theme, LookTheme::Dark);
    assert_eq!(look.maybe_brand_name.as_deref(), Some("Example Chat"));
    assert!(look.maybe_logo.is_some() && look.maybe_logo_dark.is_none());
  }

  #[test]
  fn an_unknown_theme_is_tolerated() {
    let look: SignInLook = serde_json::from_str(r#"{"theme":"sepia"}"#).unwrap();
    assert_eq!(look.theme, LookTheme::Unknown);
  }
}
