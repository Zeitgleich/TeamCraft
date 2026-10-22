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

use teamcraft_crypto::account::subresource_integrity;
use tracing::error;

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
const DOWNLOAD_PAGE: &str = "download.html";
const SRI_OPEN: &str = "{{sri:";

/// The fallback router: static files if a directory is configured, plain 404 otherwise.
pub fn router(config: &WebConfig) -> Router {
  let maybe_share = config
    .maybe_share_page_dir
    .as_deref()
    .and_then(|dir| match ShareBundle::load(dir) {
      Ok(bundle) => Some(bundle),
      Err(message) => {
        error!(%message, "the share download page cannot be served");
        None
      },
    });
  // Only the dedicated download page gets its own policy; the app shell keeps the app's.
  let share_policy = maybe_share
    .as_ref()
    .map(|_| policy_header(&config.share_page_content_security_policy));
  let Some(dir) = config.maybe_static_dir.clone() else {
    return match maybe_share {
      Some(bundle) => share_routes(Router::new(), bundle).layer(middleware::from_fn(move |request, next| {
        add_headers(None, share_policy.clone(), request, next)
      })),
      None => Router::new(),
    };
  };
  let files = ServeDir::new(&dir)
    .precompressed_br()
    .precompressed_gzip()
    .fallback(ServeFile::new(dir.join(INDEX)));
  let policy = Some(policy_header(&config.resolved_content_security_policy()));
  let router = match maybe_share {
    Some(bundle) => share_routes(Router::new(), bundle),
    None => Router::new().route("/f/{id}", get(share_page_handler).with_state(dir)),
  };
  router
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
      add_headers(policy.clone(), share_policy.clone(), request, next)
    }))
}

/// `/f/<id>` serves the small download page and `/f-assets/...` its files.
fn share_routes(router: Router, bundle: ShareBundle) -> Router {
  let assets = ServeDir::new(&bundle.dir);
  router
    .route("/f/{id}", get(bundle_page_handler).with_state(bundle.page))
    .nest_service("/f-assets", assets)
}

async fn bundle_page_handler(axum::extract::State(page): axum::extract::State<String>) -> Response {
  ([(CONTENT_TYPE, "text/html; charset=utf-8")], page).into_response()
}

/// The download page of share links with Subresource Integrity values filled in.
struct ShareBundle {
  dir: PathBuf,
  page: String,
}

impl ShareBundle {
  /// Reads `download.html` and replaces every `{{sri:<file>}}` by the SRI value of that file of
  /// the bundle. A missing file or a name outside the bundle is an error: the page must not be
  /// served without integrity checks.
  fn load(dir: &Path) -> Result<Self, String> {
    let template = std::fs::read_to_string(dir.join(DOWNLOAD_PAGE))
      .map_err(|error| format!("reading {DOWNLOAD_PAGE}: {error}"))?;
    let mut page = String::with_capacity(template.len() + 256);
    let mut rest = template.as_str();
    while let Some(start) = rest.find(SRI_OPEN) {
      page.push_str(&rest[..start]);
      let after = &rest[start + SRI_OPEN.len()..];
      let end = after.find("}}").ok_or("an unterminated {{sri:...}}")?;
      let name = &after[..end];
      let is_plain = !name.is_empty()
        && !name.starts_with('/')
        && !name
          .split('/')
          .any(|part| part.is_empty() || part == "." || part == "..")
        && name
          .bytes()
          .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/'));
      if !is_plain {
        return Err(format!("{name:?} is not a file name inside the bundle"));
      }
      let bytes = std::fs::read(dir.join(name)).map_err(|error| format!("reading {name}: {error}"))?;
      page.push_str(&subresource_integrity(&bytes));
      rest = &after[end + 2..];
    }
    page.push_str(rest);
    Ok(Self {
      dir: dir.to_owned(),
      page,
    })
  }
}

/// The header value of a policy. One that is not a valid header value (startup validation refuses
/// it, so this is a fallback) becomes the strictest policy: nothing is better than serving the
/// pages without any.
fn policy_header(policy: &str) -> HeaderValue {
  HeaderValue::from_str(policy).unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"))
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
async fn add_headers(
  policy: Option<HeaderValue>,
  share_policy: Option<HeaderValue>,
  request: Request,
  next: Next,
) -> Response {
  let path = request.uri().path().to_owned();
  let mut response = next.run(request).await;
  let headers = response.headers_mut();
  let is_share = path.starts_with("/f/") || path.starts_with("/f-assets/");
  if let Some(policy) = if is_share { share_policy.or(policy) } else { policy } {
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
  } else if path.starts_with("/f-assets/") && !has_content_hash(&path) {
    // Integrity values pin exact versions of these files.
    REVALIDATE
  } else if is_page || is_loader_script(&path) {
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

/// `init.js` and `sw.js` start the app and the service worker: a stale copy after a deploy would
/// point to removed assets, so they are revalidated like the page.
fn is_loader_script(path: &str) -> bool {
  matches!(path.rsplit('/').next(), Some("init.js" | "sw.js"))
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
