//! The datagram writer of a WebTransport session in a JavaScript host: `createWritable()` (current
//! spec, Safari) or `writable` (earlier drafts, Chromium and Firefox), with stand-ins for the
//! duplex stream. Runs in Node by default and in a browser with `WASM_BINDGEN_USE_BROWSER=1` (see
//! crates/vendor/davey/VENDORED.md for the runner).

#![cfg(all(target_arch = "wasm32", target_os = "unknown", feature = "web-api"))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use js_sys::{Function, Object, Reflect};
use teamcraft_media::web_api::{WebTransport, datagram_writable, has_global};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

/// Evaluates a JavaScript function body that returns the stand-in.
fn stand_in(body: &str) -> JsValue {
  Function::new_no_args(body).call0(&JsValue::NULL).unwrap()
}

fn tag(stream: &JsValue) -> Option<String> {
  Reflect::get(stream, &JsValue::from_str("tag"))
    .unwrap()
    .as_string()
}

#[wasm_bindgen_test]
fn create_writable_is_used_where_it_exists() {
  let datagrams = stand_in(
    "const stream = new WritableStream(); stream.tag = 'created'; \
     return { createWritable() { return this === undefined ? null : stream; } };",
  );
  let stream = datagram_writable(&datagrams).unwrap();
  assert_eq!(tag(&stream).as_deref(), Some("created"));
  // What the call driver does with it.
  let writer = stream.get_writer().unwrap();
  let _ = writer.write_with_chunk(&js_sys::Uint8Array::from(&[1u8, 2, 3][..]));
}

#[wasm_bindgen_test]
fn create_writable_wins_over_writable() {
  let datagrams = stand_in(
    "const created = new WritableStream(); created.tag = 'created'; \
     const attribute = new WritableStream(); attribute.tag = 'attribute'; \
     return { writable: attribute, createWritable() { return created; } };",
  );
  assert_eq!(
    tag(&datagram_writable(&datagrams).unwrap()).as_deref(),
    Some("created")
  );
}

#[wasm_bindgen_test]
fn writable_is_used_without_create_writable() {
  let datagrams =
    stand_in("const stream = new WritableStream(); stream.tag = 'attribute'; return { writable: stream };");
  let stream = datagram_writable(&datagrams).unwrap();
  assert_eq!(tag(&stream).as_deref(), Some("attribute"));
  assert!(stream.get_writer().is_ok());
}

#[wasm_bindgen_test]
fn neither_is_an_error_not_a_panic() {
  assert!(datagram_writable(&stand_in("return {};")).is_err());
  assert!(datagram_writable(&stand_in("return { createWritable: 1, writable: null };")).is_err());
}

#[wasm_bindgen_test]
fn a_throwing_create_writable_is_an_error() {
  let datagrams = stand_in("return { createWritable() { throw new Error('closed'); } };");
  assert!(datagram_writable(&datagrams).is_err());
}

/// In a browser with WebTransport (skipped in Node): a session's own duplex stream yields a
/// writer before it connects, whichever shape the browser implements.
#[wasm_bindgen_test]
fn a_browser_session_offers_a_datagram_writer() {
  if !has_global("WebTransport") {
    return;
  }
  // Nothing listens there; the session fails later, the writer exists at once.
  let transport = WebTransport::new("https://127.0.0.1:9/", &Object::new()).unwrap();
  let stream = datagram_writable(&transport.datagrams()).unwrap();
  assert!(stream.get_writer().is_ok());
  let _ = transport.close(&Object::new());
}
