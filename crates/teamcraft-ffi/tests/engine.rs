//! The FFI surface without a server: opening, input validation and error mapping.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use teamcraft_ffi::{
  AppConfig, EventListener, LinkPlatform, TeamCraft, TeamCraftError, TokenProvider, login_brand,
};

/// A brand answer as the server sends it, with one asset path and one link that must be dropped.
const BRAND_JSON: &str = r##"{
  "id": "brand-a", "display_name": "Brand A", "origin": "https://chat.a.example.com",
  "assets": {"logo": "/api/v1/brand/assets/logo", "icon": "https://elsewhere.example.com/icon.png"},
  "colors": {"light": {"accent": "#0b7f78", "on_accent": "url(x)"}, "dark": {"accent": "#35c6ba"}},
  "login_title": {"en": "Welcome", "de": "Willkommen"},
  "links": {"privacy": "https://a.example.com/privacy", "terms": "javascript:alert(1)"}
}"##;

/// Nothing listens on the discard port, so every request fails fast as a network error.
const UNREACHABLE_SERVER: &str = "http://127.0.0.1:9";

#[tokio::test]
async fn a_fresh_device_needs_enrollment_and_reports_the_network() {
  let directory = temp_dir();
  let teamcraft = TeamCraft::new(config(&directory, [7; 32]), tokens(Some("token")), listener()).unwrap();
  assert!(matches!(
    teamcraft.account_state().await,
    Err(TeamCraftError::Network(_))
  ));
  assert!(matches!(
    teamcraft.conversations().await,
    Err(TeamCraftError::NotEnrolled)
  ));
  assert!(matches!(
    teamcraft.start().await,
    Err(TeamCraftError::NotEnrolled)
  ));
  assert!(matches!(
    teamcraft.wait_for_link_peer().await,
    Err(TeamCraftError::NoLinkInProgress)
  ));
  let code = teamcraft
    .start_device_link("Test phone".into(), LinkPlatform::Android)
    .await
    .unwrap();
  assert!(code.starts_with("tclink2:"));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[tokio::test]
async fn without_a_token_calls_report_signed_out() {
  let directory = temp_dir();
  let teamcraft = TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).unwrap();
  assert!(matches!(
    teamcraft.account_state().await,
    Err(TeamCraftError::SignedOut)
  ));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn bad_configuration_is_rejected() {
  let directory = temp_dir();
  let mut short_key = config(&directory, [7; 32]);
  short_key.database_key.truncate(16);
  assert!(matches!(
    TeamCraft::new(short_key, tokens(None), listener()),
    Err(TeamCraftError::InvalidInput(_))
  ));
  let mut escaping = config(&directory, [7; 32]);
  escaping.account = "../elsewhere".into();
  assert!(matches!(
    TeamCraft::new(escaping, tokens(None), listener()),
    Err(TeamCraftError::InvalidInput(_))
  ));
  std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn reopening_with_the_same_key_works() {
  let directory = temp_dir();
  drop(TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).unwrap());
  // An empty store has no record to fail on; this only checks reopening works with the same key.
  assert!(TeamCraft::new(config(&directory, [7; 32]), tokens(None), listener()).is_ok());
  std::fs::remove_dir_all(&directory).unwrap();
}

struct Tokens(Option<String>);

impl TokenProvider for Tokens {
  fn access_token(&self) -> Option<String> {
    self.0.clone()
  }
}

struct Listener;

impl EventListener for Listener {
  fn on_change(&self) {}
}

mod brand {
  use super::*;

  #[test]
  fn the_login_brand_is_checked_and_localized() {
    let brand = login_brand(
      BRAND_JSON.into(),
      "https://chat.example.com/".into(),
      "de-AT".into(),
    )
    .unwrap();
    assert_eq!(brand.display_name, "Brand A");
    assert!(!brand.is_product);
    // The icon is not the brand endpoint's own asset: dropped, the tile shows the initial.
    assert_eq!(brand.maybe_icon_url, None);
    assert_eq!(brand.maybe_icon_dark_url, None);
    assert_eq!(brand.maybe_title.as_deref(), Some("Willkommen"));
    assert_eq!(brand.maybe_text, None);
    assert_eq!(
      brand.maybe_logo_url.as_deref(),
      Some("https://chat.example.com/api/v1/brand/assets/logo")
    );
    // No dark logo: the light one in both themes.
    assert_eq!(brand.maybe_logo_dark_url, brand.maybe_logo_url);
    assert_eq!(
      brand.links.maybe_privacy.as_deref(),
      Some("https://a.example.com/privacy")
    );
    assert_eq!(brand.links.maybe_terms, None);
  }

  #[test]
  fn the_brand_endpoints_own_icons_reach_the_tile() {
    let json = r#"{"id": "brand-a", "display_name": "Brand A", "origin": "",
      "assets": {"icon": "/api/v1/brand/assets/icon", "icon_dark": "/api/v1/brand/assets/icon_dark"},
      "colors": {"light": {}, "dark": {}}, "login_title": {}, "links": {}}"#;
    let brand = login_brand(json.into(), "https://chat.example.com/".into(), "en".into()).unwrap();
    assert!(!brand.is_product);
    assert_eq!(
      brand.maybe_icon_url.as_deref(),
      Some("https://chat.example.com/api/v1/brand/assets/icon")
    );
    assert_eq!(
      brand.maybe_icon_dark_url.as_deref(),
      Some("https://chat.example.com/api/v1/brand/assets/icon_dark")
    );
  }

  #[test]
  fn the_builtin_brand_is_the_product() {
    let json = r#"{"id": "teamcraft", "display_name": "TeamCraft", "origin": "", "assets": {},
      "colors": {"light": {}, "dark": {}}, "login_title": {}, "links": {}}"#;
    let brand = login_brand(json.into(), "https://chat.example.com".into(), "en".into()).unwrap();
    assert!(brand.is_product);
  }

  #[test]
  fn the_login_brand_carries_only_checked_accent_colors() {
    let brand = login_brand(BRAND_JSON.into(), "https://chat.example.com".into(), "en".into()).unwrap();
    assert_eq!(brand.light.maybe_accent.as_deref(), Some("#0b7f78"));
    // Not a hex color: dropped, the app keeps its own.
    assert_eq!(brand.light.maybe_on_accent, None);
    assert_eq!(brand.dark.maybe_accent.as_deref(), Some("#35c6ba"));
    assert_eq!(brand.dark.maybe_accent_soft, None);
  }

  #[test]
  fn an_unusable_answer_has_no_brand() {
    for json in ["", "{", "[]", r#"{"id": "a", "display_name": " ", "origin": ""}"#] {
      assert_eq!(
        login_brand(json.into(), "https://chat.example.com".into(), "en".into()),
        None,
        "{json}"
      );
    }
  }
}

fn tokens(maybe_token: Option<&str>) -> Arc<dyn TokenProvider> {
  Arc::new(Tokens(maybe_token.map(str::to_string)))
}

fn listener() -> Arc<dyn EventListener> {
  Arc::new(Listener)
}

fn config(directory: &std::path::Path, key: [u8; 32]) -> AppConfig {
  AppConfig {
    server_url: UNREACHABLE_SERVER.into(),
    data_dir: directory.to_string_lossy().into_owned(),
    account: "tester".into(),
    database_key: key.to_vec(),
    push_gateway_url: None,
  }
}

fn temp_dir() -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos();
  let directory =
    std::env::temp_dir().join(format!("teamcraft-ffi-{nanos}-{:?}", std::thread::current().id()));
  std::fs::create_dir_all(&directory).unwrap();
  directory
}
