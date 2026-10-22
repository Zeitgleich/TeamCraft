//! A minimal `fetch` wrapper shared by the OIDC flow and API calls.

use js_sys::Uint8Array;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, Request, RequestInit, RequestRedirect, Response};

pub(crate) struct FetchResponse {
  pub status: u16,
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
  let window = web_sys::window().ok_or("no window")?;
  let init = RequestInit::new();
  init.set_method(method);
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
  Ok(FetchResponse {
    status: response.status(),
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
