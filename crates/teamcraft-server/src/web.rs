//! Serves the web client: static files from a configured directory (the `trunk build` output of
//! `teamcraft-ui`) as the router's fallback, so every API route wins.
//!
//! - Unknown paths get `index.html` (client-side routing); unknown `/api/...` paths stay 404.
//! - Files whose name carries a content hash (`app-3f2a9c1b7d4e5f60.wasm`, as trunk writes them) are
//!   cached for a year; `index.html` and everything else must be revalidated.
//! - Every response carries the configured `Content-Security-Policy` and the usual protective
//!   headers. Scripts and styles are same-origin only, so there are no inline scripts to allow;
//!   trunk's `integrity` attributes (SRI) on the assets work unchanged.
//! - `/f/{id}` is the share-link download page: the same app shell, never cached and without a
//!   referrer, because the file key lives in the URL fragment.

use std::path::{Path, PathBuf};

use axum::Router;
use axum::extract::Request;
use axum::http::header::{
  CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::services::{ServeDir, ServeFile};

use crate::config::WebConfig;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";
const SHORT_LIVED: &str = "public, max-age=3600";
const NEVER_STORE: &str = "no-store";
const INDEX: &str = "index.html";

/// The fallback router: static files if a directory is configured, plain 404 otherwise.
pub fn router(config: &WebConfig) -> Router {
  let Some(dir) = config.maybe_static_dir.clone() else {
    return Router::new();
  };
  let files = ServeDir::new(&dir)
    .precompressed_br()
    .precompressed_gzip()
    .fallback(ServeFile::new(dir.join(INDEX)));
  let policy = HeaderValue::from_str(&config.content_security_policy).ok();
  Router::new()
    .route("/f/{id}", get(share_page_handler).with_state(dir))
    .route(
      "/api/{*rest}",
      get(api_not_found)
        .post(api_not_found)
        .put(api_not_found)
        .delete(api_not_found)
        .patch(api_not_found),
    )
    .fallback_service(files)
    .layer(middleware::from_fn(move |request, next| {
      add_headers(policy.clone(), request, next)
    }))
}

/// The app shell for the download page of a shared file.
async fn share_page_handler(axum::extract::State(dir): axum::extract::State<PathBuf>) -> Response {
  match tokio::fs::read(dir.join(INDEX)).await {
    Ok(page) => ([(CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response(),
    Err(_) => StatusCode::NOT_FOUND.into_response(),
  }
}

async fn api_not_found() -> StatusCode {
  StatusCode::NOT_FOUND
}

/// Adds cache and security headers according to what was requested.
async fn add_headers(policy: Option<HeaderValue>, request: Request, next: Next) -> Response {
  let path = request.uri().path().to_owned();
  let mut response = next.run(request).await;
  let headers = response.headers_mut();
  if let Some(policy) = policy {
    headers.insert(CONTENT_SECURITY_POLICY, policy);
  }
  headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
  headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
  let is_page = response
    .headers()
    .get(CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|kind| kind.starts_with("text/html"));
  let cache = if path.starts_with("/f/") {
    NEVER_STORE
  } else if is_page {
    REVALIDATE
  } else if has_content_hash(&path) {
    IMMUTABLE
  } else {
    SHORT_LIVED
  };
  if response.status().is_success() {
    response
      .headers_mut()
      .insert(CACHE_CONTROL, HeaderValue::from_static(cache));
  }
  response
}

/// Whether the file name ends in a hash before the extension: `name-<16 or more hex digits>.ext`.
fn has_content_hash(path: &str) -> bool {
  let name = Path::new(path)
    .file_name()
    .and_then(|name| name.to_str())
    .unwrap_or("");
  let Some((stem, _extension)) = name.rsplit_once('.') else {
    return false;
  };
  // `.wasm.br` and `.js.gz` keep the hash in the middle part.
  let stem = stem
    .strip_suffix(".wasm")
    .or_else(|| stem.strip_suffix(".js"))
    .unwrap_or(stem);
  // wasm-bindgen names the binary `<name>-<hash>_bg.wasm`.
  let stem = stem.strip_suffix("_bg").unwrap_or(stem);
  stem
    .rsplit_once('-')
    .is_some_and(|(_, hash)| hash.len() >= 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hashed_assets_are_recognized() {
    for hashed in [
      "/teamcraft-ui-3f2a9c1b7d4e5f60.js",
      "/assets/teamcraft-ui-3f2a9c1b7d4e5f60_bg.wasm",
      "/teamcraft-ui-3f2a9c1b7d4e5f60.css",
      "/a-0123456789abcdef0123.wasm",
    ] {
      assert!(has_content_hash(hashed), "{hashed}");
    }
    for plain in [
      "/index.html",
      "/favicon.ico",
      "/app.js",
      "/app-v2.js",
      "/app-xyz.js",
      "/dir/",
      "/",
    ] {
      assert!(!has_content_hash(plain), "{plain}");
    }
  }
}
