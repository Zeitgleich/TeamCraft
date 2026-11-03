//! A minimal `fetch` wrapper shared by the OIDC flow and API calls.

use js_sys::Uint8Array;
use teamcraft_core::api::dpop::DPOP_NONCE_HEADER;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestCache, RequestInit, RequestRedirect, Response};

pub(crate) struct FetchResponse {
  pub status: u16,
  pub maybe_content_type: Option<String>,
  /// `DPoP-Nonce` and `Date`, which a cross-origin answer only shows when it says so (an identity
  /// provider's often doesn't).
  pub maybe_dpop_nonce: Option<String>,
  pub maybe_date: Option<String>,
  /// `Age`: the answer comes out of a cache on the way, and its `Date` is that old.
  pub maybe_age: Option<String>,
  pub body: Vec<u8>,
}

/// Performs one request. Redirects are not followed: neither the token endpoint nor the API redirects,
/// and following them would let a hostile response steer where a bearer token is sent.
pub(crate) async fn fetch(
  method: &str,
  url: &str,
  headers: &[(String, String)],
  maybe_body: Option<Vec<u8>>,
) -> Result<FetchResponse, String> {
  fetch_with(method, url, headers, maybe_body, None).await
}

/// `GET` past the browser's cache, for an answer whose `Date` and headers have to be today's.
pub(crate) async fn fetch_uncached(url: &str, headers: &[(String, String)]) -> Result<FetchResponse, String> {
  fetch_with("GET", url, headers, None, Some(RequestCache::NoStore)).await
}

async fn fetch_with(
  method: &str,
  url: &str,
  headers: &[(String, String)],
  maybe_body: Option<Vec<u8>>,
  maybe_cache: Option<RequestCache>,
) -> Result<FetchResponse, String> {
  let window = web_sys::window().ok_or("no window")?;
  let init = RequestInit::new();
  init.set_method(method);
  if let Some(cache) = maybe_cache {
    init.set_cache(cache);
  }
  init.set_redirect(RequestRedirect::Error);
  let header_map = Headers::new().map_err(describe)?;
  for (name, value) in headers {
    header_map.append(name, value).map_err(describe)?;
  }
  init.set_headers(&header_map);
  if let Some(body) = maybe_body {
    init.set_body(&Uint8Array::from(body.as_slice()));
  }
  let request = Request::new_with_str_and_init(url, &init).map_err(describe)?;
  let response = JsFuture::from(window.fetch_with_request(&request))
    .await
    .map_err(describe)?;
  let response: Response = response
    .dyn_into()
    .map_err(|_| "fetch did not return a response".to_owned())?;
  let buffer = JsFuture::from(response.array_buffer().map_err(describe)?)
    .await
    .map_err(describe)?;
  let maybe_content_type = response.headers().get("Content-Type").ok().flatten();
  Ok(FetchResponse {
    status: response.status(),
    maybe_content_type,
    maybe_dpop_nonce: response.headers().get(DPOP_NONCE_HEADER).ok().flatten(),
    maybe_date: response.headers().get("Date").ok().flatten(),
    maybe_age: response.headers().get("Age").ok().flatten(),
    body: Uint8Array::new(&buffer).to_vec(),
  })
}

fn describe(value: JsValue) -> String {
  value
    .as_string()
    .or_else(|| {
      js_sys::JSON::stringify(&value)
        .ok()
        .and_then(|text| text.as_string())
    })
    .unwrap_or_else(|| "unknown error".to_owned())
}
