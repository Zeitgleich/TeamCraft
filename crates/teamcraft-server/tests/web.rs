//! Serving the web client from a directory: routing, caching and security headers.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use teamcraft_server::testing::TestApp;

const INDEX: &str =
  "<!doctype html><title>TeamCraft</title><script type=\"module\" src=\"/app-3f2a9c1b7d4e5f60.js\"></script>";
const SCRIPT: &str = "console.log('app');";

struct Site {
  dir: PathBuf,
}

impl Site {
  fn create() -> Self {
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-web-test-{}-{}",
      std::process::id(),
      rand_suffix()
    ));
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), INDEX).unwrap();
    std::fs::write(dir.join("app-3f2a9c1b7d4e5f60.js"), SCRIPT).unwrap();
    std::fs::write(
      dir.join("assets/app-3f2a9c1b7d4e5f60_bg.wasm"),
      b"\0asm\x01\0\0\0",
    )
    .unwrap();
    std::fs::write(dir.join("favicon.ico"), b"ico").unwrap();
    Self { dir }
  }
}

impl Drop for Site {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

fn rand_suffix() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos()
}

async fn start(site: &Site) -> Option<TestApp> {
  let dir = site.dir.clone();
  TestApp::start_with(move |settings| settings.web.maybe_static_dir = Some(dir)).await
}

async fn get(app: &TestApp, path: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
  let response = app
    .http
    .get(format!("{}{path}", app.base_url))
    .send()
    .await
    .unwrap();
  let (status, headers) = (response.status(), response.headers().clone());
  (status, headers, response.bytes().await.unwrap().to_vec())
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
  headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

#[tokio::test]
async fn the_shell_is_served_at_the_root_and_for_client_side_routes_without_caching() {
  let site = Site::create();
  let Some(app) = start(&site).await else { return };

  for path in ["/", "/index.html", "/c/some/channel", "/meeting/42"] {
    let (status, headers, body) = get(&app, path).await;
    assert_eq!(status, StatusCode::OK, "{path}");
    assert_eq!(String::from_utf8(body).unwrap(), INDEX, "{path}");
    assert!(
      header(&headers, "content-type").starts_with("text/html"),
      "{path}"
    );
    assert_eq!(header(&headers, "cache-control"), "no-cache", "{path}");
    let csp = header(&headers, "content-security-policy");
    assert!(
      csp.contains("default-src 'self'")
        && csp.contains("'wasm-unsafe-eval'")
        && csp.contains("frame-ancestors 'none'"),
      "{csp}"
    );
    assert_eq!(header(&headers, "x-content-type-options"), "nosniff");
    assert_eq!(header(&headers, "referrer-policy"), "no-referrer");
  }
  app.stop().await;
}

#[tokio::test]
async fn hashed_assets_are_cached_for_a_year_and_other_files_briefly() {
  let site = Site::create();
  let Some(app) = start(&site).await else { return };

  let (status, headers, body) = get(&app, "/app-3f2a9c1b7d4e5f60.js").await;
  assert_eq!((status, body.as_slice()), (StatusCode::OK, SCRIPT.as_bytes()));
  assert_eq!(
    header(&headers, "cache-control"),
    "public, max-age=31536000, immutable"
  );
  assert!(header(&headers, "content-type").contains("javascript"));
  assert!(
    headers.get("etag").is_some() || headers.get("last-modified").is_some(),
    "revalidation headers"
  );

  let (_, wasm, _) = get(&app, "/assets/app-3f2a9c1b7d4e5f60_bg.wasm").await;
  assert_eq!(
    header(&wasm, "cache-control"),
    "public, max-age=31536000, immutable"
  );
  assert_eq!(header(&wasm, "content-type"), "application/wasm");
  let (_, icon, _) = get(&app, "/favicon.ico").await;
  assert_eq!(header(&icon, "cache-control"), "public, max-age=3600");
  app.stop().await;
}

#[tokio::test]
async fn the_api_wins_and_unknown_api_paths_are_not_the_app() {
  let site = Site::create();
  let Some(app) = start(&site).await else { return };

  let (status, _, body) = get(&app, "/api/v1/openapi.json").await;
  assert_eq!(status, StatusCode::OK);
  assert!(String::from_utf8(body).unwrap().contains("\"paths\""));
  assert_eq!(get(&app, "/healthz").await.0, StatusCode::OK);
  for path in ["/api/v1/nonexistent", "/api/other"] {
    let (status, _, body) = get(&app, path).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    assert!(!String::from_utf8_lossy(&body).contains("TeamCraft"), "{path}");
  }
  app.stop().await;
}

#[tokio::test]
async fn the_download_page_of_a_shared_file_is_the_shell_and_never_stored() {
  let site = Site::create();
  let Some(app) = start(&site).await else { return };

  let (status, headers, body) = get(&app, "/f/1234567890").await;

  assert_eq!(status, StatusCode::OK);
  assert_eq!(String::from_utf8(body).unwrap(), INDEX);
  assert_eq!(header(&headers, "cache-control"), "no-store");
  assert_eq!(header(&headers, "referrer-policy"), "no-referrer");
  assert!(header(&headers, "content-security-policy").contains("default-src 'self'"));
  app.stop().await;
}

#[tokio::test]
async fn paths_cannot_escape_the_directory() {
  let site = Site::create();
  let Some(app) = start(&site).await else { return };
  std::fs::write(
    site.dir.parent().unwrap().join("teamcraft-web-secret.txt"),
    "secret",
  )
  .unwrap();

  for path in [
    "/../teamcraft-web-secret.txt",
    "/%2e%2e/teamcraft-web-secret.txt",
    "/..%2fteamcraft-web-secret.txt",
  ] {
    let (_, _, body) = get(&app, path).await;
    assert!(!String::from_utf8_lossy(&body).contains("secret"), "{path}");
  }
  std::fs::remove_file(site.dir.parent().unwrap().join("teamcraft-web-secret.txt")).unwrap();
  app.stop().await;
}

#[tokio::test]
async fn without_a_directory_only_the_api_is_served() {
  let Some(app) = TestApp::start().await else { return };
  assert_eq!(get(&app, "/").await.0, StatusCode::NOT_FOUND);
  assert_eq!(get(&app, "/f/abc").await.0, StatusCode::NOT_FOUND);
  assert_eq!(get(&app, "/healthz").await.0, StatusCode::OK);
  app.stop().await;
}
