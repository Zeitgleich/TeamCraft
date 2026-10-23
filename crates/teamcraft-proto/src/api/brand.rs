//! Brands (docs/branding.md): the presentation of one host name of the server.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;

wire_enum! {
  /// An image of a brand. The slot names are the last segment of
  /// `GET /api/v1/brand/assets/{slot}`.
  pub enum BrandAssetSlot {
    Logo => "logo",
    LogoDark => "logo_dark",
    Icon => "icon",
    IconDark => "icon_dark",
    Favicon => "favicon",
    FaviconDark => "favicon_dark",
    /// The provider icon on the sign-in button.
    SsoIcon => "sso_icon",
  }
}

/// `GET /api/v1/brand`: how the server presents itself on the host the request arrived on.
/// Everything in it comes from the server configuration; clients ignore values they cannot
/// validate (colors that are not hex, `origin` that differs from the URL they asked).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandResponse {
  /// Stable identifier of the brand (`[a-z0-9-]`).
  pub id: String,
  pub display_name: String,
  /// The organization behind the brand, for the footer of the sign-in page.
  #[serde(
    rename = "organization_name",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_organization_name: Option<String>,
  /// The brand's canonical origin, `https://<first host name>`. Informational.
  pub origin: String,
  /// Asset URLs relative to the server, by slot name. Slots without a file are absent.
  #[serde(default)]
  pub assets: BTreeMap<String, String>,
  #[serde(default)]
  pub colors: BrandColors,
  /// Language code -> text. `en` is the fallback.
  #[serde(default)]
  pub login_title: BTreeMap<String, String>,
  #[serde(default)]
  pub login_text: BTreeMap<String, String>,
  #[serde(default)]
  pub links: BrandLinks,
}

/// Color overrides for the light and the dark theme. Keys are token names (`accent`,
/// `accent_strong`, `on_accent`, `accent_soft`, `sidebar`, `sidebar_current`, `chrome`), values are
/// `#rgb` / `#rrggbb`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandColors {
  #[serde(default)]
  pub light: BTreeMap<String, String>,
  #[serde(default)]
  pub dark: BTreeMap<String, String>,
}

/// `GET /api/v1/brands` (signed in): the brands of this server, for tagging spaces and for the
/// accent color of a tagged space. No host names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListBrandsResponse {
  pub brands: Vec<BrandSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandSummary {
  pub id: String,
  pub display_name: String,
  #[serde(default)]
  pub colors: BrandColors,
}

/// Color keys a brand may set and the CSS custom properties (design tokens of the UI) each one
/// drives. Nothing else is brandable: status and danger colors keep their meaning.
pub const BRAND_COLOR_TOKENS: &[(&str, &[&str])] = &[
  ("accent", &["--accent", "--focus"]),
  ("accent_strong", &["--accent-strong"]),
  ("on_accent", &["--on-accent"]),
  ("accent_soft", &["--accent-soft"]),
  ("sidebar", &["--sb-bg"]),
  ("sidebar_current", &["--sb-current"]),
  ("chrome", &["--chrome"]),
];

/// The CSS custom properties a color key sets; `None` for a key that is not brandable.
pub fn brand_color_properties(key: &str) -> Option<&'static [&'static str]> {
  BRAND_COLOR_TOKENS
    .iter()
    .find(|(name, _)| *name == key)
    .map(|(_, properties)| *properties)
}

/// Whether a value is `#rgb` or `#rrggbb`. The only color syntax brands may use, so that no
/// configured text can break out of a CSS declaration.
pub fn is_brand_hex_color(value: &str) -> bool {
  let Some(digits) = value.strip_prefix('#') else {
    return false;
  };
  matches!(digits.len(), 3 | 6) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Links shown on the login screen and in the footer; each is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BrandLinks {
  #[serde(rename = "support", default, skip_serializing_if = "Option::is_none")]
  pub maybe_support: Option<String>,
  #[serde(rename = "privacy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_privacy: Option<String>,
  #[serde(rename = "terms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_terms: Option<String>,
  #[serde(rename = "imprint", default, skip_serializing_if = "Option::is_none")]
  pub maybe_imprint: Option<String>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn brand_response_wire_format_is_stable() {
    let brand = BrandResponse {
      id: "brand-a".into(),
      display_name: "Brand A".into(),
      maybe_organization_name: None,
      origin: "https://chat.example.com".into(),
      assets: BTreeMap::from([("logo".to_owned(), "/api/v1/brand/assets/logo".to_owned())]),
      colors: BrandColors {
        light: BTreeMap::from([("accent".to_owned(), "#0b7f78".to_owned())]),
        dark: BTreeMap::new(),
      },
      login_title: BTreeMap::from([("en".to_owned(), "Welcome".to_owned())]),
      login_text: BTreeMap::new(),
      links: BrandLinks {
        maybe_support: Some("https://support.example.com".into()),
        ..BrandLinks::default()
      },
    };
    assert_eq!(
      serde_json::to_string(&brand).unwrap(),
      r##"{"id":"brand-a","display_name":"Brand A","origin":"https://chat.example.com","assets":{"logo":"/api/v1/brand/assets/logo"},"colors":{"light":{"accent":"#0b7f78"},"dark":{}},"login_title":{"en":"Welcome"},"login_text":{},"links":{"support":"https://support.example.com"}}"##
    );
  }

  #[test]
  fn only_plain_hex_colors_are_accepted() {
    for ok in ["#fff", "#0B7f78", "#000000"] {
      assert!(is_brand_hex_color(ok), "{ok}");
    }
    for bad in [
      "",
      "#",
      "#ffff",
      "fff",
      "#ggg",
      "red",
      "#fff;}",
      "#12345g",
      "url(x)",
      "#fff }body{",
    ] {
      assert!(!is_brand_hex_color(bad), "{bad}");
    }
    assert_eq!(
      brand_color_properties("accent"),
      Some(&["--accent", "--focus"][..])
    );
    assert_eq!(brand_color_properties("danger"), None);
  }

  #[test]
  fn a_minimal_response_and_unknown_slots_parse() {
    let parsed: BrandResponse =
      serde_json::from_str(r#"{"id":"x","display_name":"X","origin":"https://x.example.com"}"#).unwrap();
    assert!(parsed.assets.is_empty() && parsed.colors == BrandColors::default());
    assert_eq!(
      BrandAssetSlot::from("hero".to_owned()),
      BrandAssetSlot::Unknown("hero".into())
    );
    assert_eq!(
      BrandAssetSlot::from("logo_dark".to_owned()),
      BrandAssetSlot::LogoDark
    );
  }
}
