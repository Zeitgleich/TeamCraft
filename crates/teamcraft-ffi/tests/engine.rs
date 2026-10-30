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

/// Issue #999: the new device waits in the relay until another device answers; asking the other
/// devices for a prompt must go out meanwhile, not after the wait.
mod device_link {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  use tokio::net::{TcpListener, TcpStream};
  use tokio::sync::mpsc;

  use super::*;

  #[tokio::test]
  async fn asking_the_other_devices_goes_out_while_the_new_device_waits() {
    let directory = temp_dir();
    let (server_url, mut requests) = held_server().await;
    let teamcraft = Arc::new(
      TeamCraft::new(
        config_at(&directory, [7; 32], &server_url),
        tokens(Some("token")),
        listener(),
      )
      .unwrap(),
    );
    teamcraft
      .start_device_link("Test phone".into(), LinkPlatform::Android)
      .await
      .unwrap();
    let waiting = tokio::spawn({
      let teamcraft = teamcraft.clone();
      async move { teamcraft.wait_for_link_peer().await }
    });
    // The wait's first request is held unanswered: the wait goes on as long as the test runs.
    assert!(requests.recv().await.unwrap().starts_with("GET "));

    // The server's refusal is the answer: the request went out.
    assert!(teamcraft.announce_device_link().await.is_err());
    assert!(
      requests
        .recv()
        .await
        .unwrap()
        .starts_with("POST /api/v1/device-links ")
    );
    assert!(!waiting.is_finished(), "still waiting for the other device");

    // A new link ends the earlier wait.
    teamcraft
      .start_device_link("Test phone".into(), LinkPlatform::Android)
      .await
      .unwrap();
    assert!(matches!(
      waiting.await.unwrap(),
      Err(TeamCraftError::NoLinkInProgress)
    ));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A server that holds its first request unanswered and refuses every later one; the request
  /// lines arrive at the receiver in order.
  async fn held_server() -> (String, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (lines, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
      let mut held = Vec::new();
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        lines.send(request_line(&mut stream).await).unwrap();
        if held.is_empty() {
          held.push(stream);
        } else {
          let refusal = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
          stream.write_all(refusal.as_bytes()).await.unwrap();
        }
      }
    });
    (url, requests)
  }

  /// Reads the request's head; returns its first line.
  async fn request_line(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
      stream.read_exact(&mut byte).await.unwrap();
      head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    head.lines().next().unwrap_or_default().to_owned()
  }
}

/// Issue #846: the apps show profile pictures; the engine fetches one through the API's presigned
/// location and answers `None` for a person without one.
mod avatar {
  use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
  use tokio::net::TcpListener;

  use super::*;

  const PICTURE: &[u8] = b"\x89PNG picture bytes";
  const WITH_PICTURE: &str = "42";
  const WITHOUT_PICTURE: &str = "43";

  #[tokio::test]
  async fn a_picture_is_fetched_from_its_presigned_location() {
    let directory = temp_dir();
    let teamcraft = engine_at(&directory, &avatar_server().await);
    let picture = teamcraft.avatar_picture(WITH_PICTURE.into(), 1024).await.unwrap();
    assert_eq!(picture.as_deref(), Some(PICTURE));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[tokio::test]
  async fn a_person_without_a_picture_has_none() {
    let directory = temp_dir();
    let teamcraft = engine_at(&directory, &avatar_server().await);
    let picture = teamcraft
      .avatar_picture(WITHOUT_PICTURE.into(), 1024)
      .await
      .unwrap();
    assert_eq!(picture, None);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[tokio::test]
  async fn a_picture_larger_than_the_cap_is_refused() {
    let directory = temp_dir();
    let teamcraft = engine_at(&directory, &avatar_server().await);
    assert!(teamcraft.avatar_picture(WITH_PICTURE.into(), 4).await.is_err());
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[tokio::test]
  async fn an_invalid_user_id_is_refused_before_any_request() {
    let directory = temp_dir();
    let teamcraft = TeamCraft::new(config(&directory, [7; 32]), tokens(Some("token")), listener()).unwrap();
    assert!(matches!(
      teamcraft.avatar_picture("../me".into(), 1024).await,
      Err(TeamCraftError::InvalidInput(_))
    ));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  fn engine_at(directory: &std::path::Path, server_url: &str) -> Arc<TeamCraft> {
    TeamCraft::new(
      config_at(directory, [7; 32], server_url),
      tokens(Some("token")),
      listener(),
    )
    .unwrap()
  }

  /// A server with one person who has a picture (served from `/blob`) and one who has none.
  async fn avatar_server() -> String {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let base = url.clone();
    tokio::spawn(async move {
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let path = request_path(&mut stream).await;
        let (status, body) = if path == format!("/api/v1/users/{WITH_PICTURE}/avatar") {
          let location = format!(
            r#"{{"download_url":"{base}/blob","content_type":"image/png","size_bytes":{},"expires_at":0}}"#,
            PICTURE.len()
          );
          ("200 OK", location.into_bytes())
        } else if path == format!("/api/v1/users/{WITHOUT_PICTURE}/avatar") {
          let missing = r#"{"error_code":"avatar_not_found","message":"no picture"}"#;
          ("404 Not Found", missing.as_bytes().to_vec())
        } else if path == "/blob" {
          ("200 OK", PICTURE.to_vec())
        } else {
          ("404 Not Found", Vec::new())
        };
        let head = format!(
          "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
          body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
      }
    });
    url
  }

  /// Reads the request's head; returns the path of its first line.
  async fn request_path(stream: &mut tokio::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
      stream.read_exact(&mut byte).await.unwrap();
      head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let line = head.lines().next().unwrap_or_default();
    line.split(' ').nth(1).unwrap_or_default().to_owned()
  }
}

fn tokens(maybe_token: Option<&str>) -> Arc<dyn TokenProvider> {
  Arc::new(Tokens(maybe_token.map(str::to_string)))
}

fn listener() -> Arc<dyn EventListener> {
  Arc::new(Listener)
}

fn config(directory: &std::path::Path, key: [u8; 32]) -> AppConfig {
  config_at(directory, key, UNREACHABLE_SERVER)
}

fn config_at(directory: &std::path::Path, key: [u8; 32], server_url: &str) -> AppConfig {
  AppConfig {
    server_url: server_url.into(),
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
