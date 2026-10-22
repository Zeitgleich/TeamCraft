//! The brand a client shows (docs/branding.md §5): the server's `GET /api/v1/brand` answer, checked
//! and made safe to apply. All platforms (web, desktop, mobile) run the same checks: only known
//! color keys with plain hex values, only asset paths of the brand endpoint, only https links.
//! Fetching and applying are the platform's job.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  BrandAssetSlot, BrandLinks, BrandResponse, brand_color_properties, is_brand_hex_color,
};

/// Path prefix of brand images on the server.
const ASSET_PREFIX: &str = "/api/v1/brand/assets/";
const MAX_NAME_CHARS: usize = 100;
const MAX_TEXT_CHARS: usize = 1000;
/// Name shown while no brand is known (and for a server without brands).
const FALLBACK_NAME: &str = "TeamCraft";

/// A checked brand, ready to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandView {
  pub id: String,
  pub display_name: String,
  /// Absolute image URLs, by slot name (`logo`, `logo_dark`, `icon`, ...).
  pub assets: BTreeMap<String, String>,
  /// CSS custom properties for the light theme, e.g. `("--accent", "#0b7f78")`.
  pub light: Vec<(String, String)>,
  /// CSS custom properties for the dark theme.
  pub dark: Vec<(String, String)>,
  pub login_title: BTreeMap<String, String>,
  pub login_text: BTreeMap<String, String>,
  pub links: BrandLinks,
}

impl BrandView {
  /// What a client shows without a brand: the product name and the built-in colors.
  pub fn fallback() -> Self {
    Self {
      id: String::new(),
      display_name: FALLBACK_NAME.to_owned(),
      assets: BTreeMap::new(),
      light: Vec::new(),
      dark: Vec::new(),
      login_title: BTreeMap::new(),
      login_text: BTreeMap::new(),
      links: BrandLinks::default(),
    }
  }

  /// Checks a server answer. `server_url` is the base URL the client asked (empty: same origin);
  /// asset URLs are built on it. `None` if the answer is unusable (no name).
  ///
  /// The `origin` field is informational and not compared with `server_url`: a brand may have
  /// several host names, and the user chose the server they asked. What the answer can change is
  /// presentation only, and each part is limited to a safe form.
  pub fn from_response(response: BrandResponse, server_url: &str) -> Option<Self> {
    let display_name = response.display_name.trim();
    if display_name.is_empty() || display_name.chars().count() > MAX_NAME_CHARS {
      return None;
    }
    let base = server_url.trim_end_matches('/');
    let assets = response
      .assets
      .into_iter()
      .filter(|(slot, path)| {
        let is_known_slot = !matches!(BrandAssetSlot::from(slot.clone()), BrandAssetSlot::Unknown(_));
        is_known_slot && path == &format!("{ASSET_PREFIX}{slot}")
      })
      .map(|(slot, path)| (slot, format!("{base}{path}")))
      .collect();
    Some(Self {
      id: response
        .id
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(32)
        .collect(),
      display_name: display_name.to_owned(),
      assets,
      light: css_properties(&response.colors.light),
      dark: css_properties(&response.colors.dark),
      login_title: clean_texts(response.login_title),
      login_text: clean_texts(response.login_text),
      links: BrandLinks {
        maybe_support: safe_link(response.links.maybe_support),
        maybe_privacy: safe_link(response.links.maybe_privacy),
        maybe_terms: safe_link(response.links.maybe_terms),
        maybe_imprint: safe_link(response.links.maybe_imprint),
      },
    })
  }

  /// The CSS custom properties for the effective theme.
  pub fn properties(&self, is_dark: bool) -> &[(String, String)] {
    if is_dark { &self.dark } else { &self.light }
  }

  /// The image of a slot; in the dark theme the `_dark` variant if there is one.
  pub fn asset(&self, slot: &str, is_dark: bool) -> Option<&str> {
    if is_dark && let Some(url) = self.assets.get(&format!("{slot}_dark")) {
      return Some(url);
    }
    self.assets.get(slot).map(String::as_str)
  }

  pub fn login_title(&self, language: &str) -> Option<&str> {
    pick(&self.login_title, language)
  }

  pub fn login_text(&self, language: &str) -> Option<&str> {
    pick(&self.login_text, language)
  }
}

/// The key under which a client stores the brand of a server (`localStorage`, settings file).
pub fn cache_key(server_url: &str) -> String {
  format!("teamcraft.brand:{}", server_url.trim_end_matches('/'))
}

/// Text for a language: the exact tag, its primary subtag (`de-AT` → `de`), then `en`.
fn pick<'a>(texts: &'a BTreeMap<String, String>, language: &str) -> Option<&'a str> {
  let primary = language.split('-').next().unwrap_or(language);
  [language, primary, "en"]
    .into_iter()
    .find_map(|tag| texts.get(tag))
    .map(String::as_str)
}

fn css_properties(colors: &BTreeMap<String, String>) -> Vec<(String, String)> {
  colors
    .iter()
    .filter(|(_, value)| is_brand_hex_color(value))
    .flat_map(|(key, value)| {
      brand_color_properties(key)
        .unwrap_or_default()
        .iter()
        .map(|property| ((*property).to_owned(), value.clone()))
    })
    .collect()
}

fn clean_texts(texts: BTreeMap<String, String>) -> BTreeMap<String, String> {
  texts
    .into_iter()
    .filter(|(language, text)| {
      !language.is_empty()
        && language.len() <= 16
        && language.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && text.chars().count() <= MAX_TEXT_CHARS
    })
    .collect()
}

/// A link is shown only when it is an https URL (http for localhost) without credentials.
fn safe_link(maybe_link: Option<String>) -> Option<String> {
  let link = maybe_link?;
  let (scheme, rest) = link.split_once("://")?;
  let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
  let host = authority.rsplit('@').next().unwrap_or("");
  let is_local = host == "localhost" || host.starts_with("localhost:");
  let is_scheme_ok = scheme == "https" || (scheme == "http" && is_local);
  (is_scheme_ok && !authority.contains('@') && !host.is_empty() && !link.chars().any(char::is_whitespace))
    .then_some(link)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::BrandColors;

  use super::*;

  fn response() -> BrandResponse {
    BrandResponse {
      id: "brand-a".into(),
      display_name: "Brand A".into(),
      origin: "https://chat.a.example.com".into(),
      assets: BTreeMap::from([
        ("logo".to_owned(), "/api/v1/brand/assets/logo".to_owned()),
        (
          "logo_dark".to_owned(),
          "/api/v1/brand/assets/logo_dark".to_owned(),
        ),
      ]),
      colors: BrandColors {
        light: BTreeMap::from([("accent".to_owned(), "#0b7f78".to_owned())]),
        dark: BTreeMap::from([("accent".to_owned(), "#35c6ba".to_owned())]),
      },
      login_title: BTreeMap::from([
        ("en".to_owned(), "Welcome".to_owned()),
        ("de".to_owned(), "Willkommen".to_owned()),
      ]),
      login_text: BTreeMap::new(),
      links: BrandLinks {
        maybe_support: Some("https://support.a.example.com".into()),
        ..BrandLinks::default()
      },
    }
  }

  #[test]
  fn a_good_answer_becomes_css_properties_and_absolute_asset_urls() {
    let view = BrandView::from_response(response(), "https://chat.a.example.com/").unwrap();
    assert_eq!(
      view.properties(false),
      [
        ("--accent".to_owned(), "#0b7f78".to_owned()),
        ("--focus".to_owned(), "#0b7f78".to_owned())
      ]
    );
    assert_eq!(
      view.properties(true)[0],
      ("--accent".to_owned(), "#35c6ba".to_owned())
    );
    assert_eq!(
      view.asset("logo", false),
      Some("https://chat.a.example.com/api/v1/brand/assets/logo")
    );
    assert_eq!(
      view.asset("logo", true),
      Some("https://chat.a.example.com/api/v1/brand/assets/logo_dark")
    );
    assert_eq!(view.asset("icon", true), None);
    // Same-origin web client: relative URLs stay relative.
    assert_eq!(
      BrandView::from_response(response(), "")
        .unwrap()
        .asset("logo", false),
      Some("/api/v1/brand/assets/logo")
    );
  }

  #[test]
  fn hostile_values_are_dropped() {
    let mut hostile = response();
    hostile.colors.light = BTreeMap::from([
      (
        "accent".to_owned(),
        "red; background:url(//evil.example)".to_owned(),
      ),
      ("danger".to_owned(), "#ff0000".to_owned()),
      ("chrome".to_owned(), "#123".to_owned()),
    ]);
    hostile.assets = BTreeMap::from([
      ("logo".to_owned(), "https://evil.example/logo.png".to_owned()),
      ("icon".to_owned(), "/api/v1/brand/assets/../../me".to_owned()),
      ("hero".to_owned(), "/api/v1/brand/assets/hero".to_owned()),
    ]);
    hostile.links = BrandLinks {
      maybe_support: Some("javascript:alert(1)".into()),
      maybe_privacy: Some("https://user:pw@example.com/".into()),
      maybe_terms: Some("http://example.com/terms".into()),
      maybe_imprint: Some("https://example.com/imprint".into()),
    };
    let view = BrandView::from_response(hostile, "https://chat.a.example.com").unwrap();
    assert_eq!(view.light, [("--chrome".to_owned(), "#123".to_owned())]);
    assert!(view.assets.is_empty(), "{:?}", view.assets);
    assert_eq!(view.links.maybe_support, None);
    assert_eq!(view.links.maybe_privacy, None);
    assert_eq!(view.links.maybe_terms, None);
    assert_eq!(
      view.links.maybe_imprint.as_deref(),
      Some("https://example.com/imprint")
    );
  }

  #[test]
  fn a_nameless_answer_is_unusable() {
    let mut nameless = response();
    nameless.display_name = "  ".into();
    assert_eq!(BrandView::from_response(nameless, ""), None);
  }

  #[test]
  fn login_text_follows_the_language_with_fallbacks() {
    let view = BrandView::from_response(response(), "").unwrap();
    assert_eq!(view.login_title("de"), Some("Willkommen"));
    assert_eq!(view.login_title("de-AT"), Some("Willkommen"));
    assert_eq!(view.login_title("fr"), Some("Welcome"));
    assert_eq!(view.login_text("en"), None);
  }

  #[test]
  fn the_cache_is_keyed_by_the_server_url() {
    assert_eq!(
      cache_key("https://chat.example.com/"),
      cache_key("https://chat.example.com")
    );
    assert_ne!(
      cache_key("https://chat.example.com"),
      cache_key("https://chat.example.org")
    );
  }
}
