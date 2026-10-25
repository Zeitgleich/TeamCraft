//! Blob transfers in the browser with `XMLHttpRequest`: fetch reports no upload progress, and
//! reqwest reads browser downloads whole. The presigned URLs carry their own authorization, so no
//! token is involved. Dropping a transfer aborts it (a cancelled upload stops sending).

use std::cell::RefCell;
use std::rc::Rc;

use futures_channel::oneshot;
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{ProgressEvent, XmlHttpRequest, XmlHttpRequestResponseType};

use super::{ApiError, Interception};
use crate::transfers::ByteCounter;

/// Uploads `bytes` with `PUT`, adding the bytes sent to `sent`.
pub async fn put(url: &str, bytes: Vec<u8>, sent: ByteCounter) -> Result<(), ApiError> {
  send_put(url, bytes, sent).await.map(|_| ())
}

/// Uploads one part of an upload in parts like [`put`]; resolves to the part's `ETag` (the storage's
/// CORS rule must expose it).
pub async fn put_part(url: &str, bytes: Vec<u8>, sent: ByteCounter) -> Result<String, ApiError> {
  let xhr = send_put(url, bytes, sent).await?;
  xhr.get_response_header("etag").ok().flatten().ok_or_else(|| {
    ApiError::Decode("the storage answered a part without an ETag (CORS ExposeHeaders)".into())
  })
}

async fn send_put(url: &str, bytes: Vec<u8>, sent: ByteCounter) -> Result<XmlHttpRequest, ApiError> {
  let mut request = Transfer::open("PUT", url)?;
  let upload: web_sys::XmlHttpRequestEventTarget = request.xhr.upload().map_err(js_error)?.into();
  request.on_progress(&upload, sent, None);
  let body = Uint8Array::from(bytes.as_slice());
  drop(bytes);
  request
    .xhr
    .send_with_opt_buffer_source(Some(&body))
    .map_err(js_error)?;
  request.finish().await
}

/// Downloads with `GET`, counting the bytes received into `received`; fails once more than
/// `maybe_max_bytes` arrived.
pub async fn get(
  url: &str,
  maybe_max_bytes: Option<u64>,
  received: ByteCounter,
) -> Result<Vec<u8>, ApiError> {
  let mut request = Transfer::open("GET", url)?;
  request
    .xhr
    .set_response_type(XmlHttpRequestResponseType::Arraybuffer);
  let target: web_sys::XmlHttpRequestEventTarget = request.xhr.clone().unchecked_into();
  request.on_progress(&target, received, maybe_max_bytes);
  request.xhr.send().map_err(js_error)?;
  let xhr = request.finish().await?;
  if is_html(&xhr) {
    return Err(ApiError::Intercepted {
      kind: Interception::CaptivePortal,
      detail: "an HTML page answered instead of the file".into(),
    });
  }
  let buffer: ArrayBuffer = xhr
    .response()
    .map_err(js_error)?
    .dyn_into()
    .map_err(|_| ApiError::Decode("the download is not binary".into()))?;
  let bytes = Uint8Array::new(&buffer).to_vec();
  if let Some(max_bytes) = maybe_max_bytes
    && bytes.len() as u64 > max_bytes
  {
    return Err(ApiError::BlobTooLarge { max_bytes });
  }
  Ok(bytes)
}

/// One request and the listeners that end it.
struct Transfer {
  xhr: XmlHttpRequest,
  ended: oneshot::Receiver<Ending>,
  sender: Rc<RefCell<Option<oneshot::Sender<Ending>>>>,
  /// Kept alive while the request runs; the browser calls them.
  listeners: Vec<Closure<dyn FnMut(ProgressEvent)>>,
  is_finished: bool,
}

/// How a request ended.
enum Ending {
  Loaded,
  Failed,
  TooLarge(u64),
}

impl Transfer {
  fn open(method: &str, url: &str) -> Result<Self, ApiError> {
    let xhr = XmlHttpRequest::new().map_err(js_error)?;
    xhr.open(method, url).map_err(js_error)?;
    let (sender, ended) = oneshot::channel();
    let mut transfer = Self {
      xhr,
      ended,
      sender: Rc::new(RefCell::new(Some(sender))),
      listeners: Vec::new(),
      is_finished: false,
    };
    let target: web_sys::XmlHttpRequestEventTarget = transfer.xhr.clone().unchecked_into();
    transfer.on(&target, "load", |_| Some(Ending::Loaded));
    for event in ["error", "abort", "timeout"] {
      transfer.on(&target, event, |_| Some(Ending::Failed));
    }
    Ok(transfer)
  }

  /// Counts the bytes moved on top of what `counter` had (the parts before this one); stops the
  /// request once more than `maybe_max_bytes` arrived.
  fn on_progress(
    &mut self,
    target: &web_sys::XmlHttpRequestEventTarget,
    counter: ByteCounter,
    maybe_max_bytes: Option<u64>,
  ) {
    let xhr = self.xhr.clone();
    let before = counter.get();
    self.on(target, "progress", move |event| {
      let loaded = event.loaded() as u64;
      counter.set(before + loaded);
      let max_bytes = maybe_max_bytes?;
      if loaded <= max_bytes {
        return None;
      }
      // Deliberately ignored: the request ends as too large either way.
      let _ = xhr.abort();
      Some(Ending::TooLarge(max_bytes))
    });
  }

  fn on(
    &mut self,
    target: &web_sys::XmlHttpRequestEventTarget,
    event: &str,
    mut ends: impl FnMut(&ProgressEvent) -> Option<Ending> + 'static,
  ) {
    let sender = self.sender.clone();
    let listener = Closure::<dyn FnMut(ProgressEvent)>::new(move |event: ProgressEvent| {
      if let Some(ending) = ends(&event)
        && let Some(sender) = sender.borrow_mut().take()
      {
        // Deliberately ignored: nobody waits any more when the transfer was dropped.
        let _ = sender.send(ending);
      }
    });
    if let Err(error) = target.add_event_listener_with_callback(event, listener.as_ref().unchecked_ref()) {
      tracing::debug!(?error, event, "could not listen to the transfer");
    }
    self.listeners.push(listener);
  }

  /// Waits for the end; a 2xx status is success.
  async fn finish(&mut self) -> Result<XmlHttpRequest, ApiError> {
    let ending = (&mut self.ended).await;
    self.is_finished = true;
    match ending {
      Ok(Ending::Loaded) => {},
      Ok(Ending::TooLarge(max_bytes)) => return Err(ApiError::BlobTooLarge { max_bytes }),
      Ok(Ending::Failed) | Err(_) => return Err(ApiError::Network("the transfer did not complete".into())),
    }
    let status = self.xhr.status().map_err(js_error)?;
    if (200..300).contains(&status) {
      return Ok(self.xhr.clone());
    }
    if status == 511 {
      return Err(ApiError::Intercepted {
        kind: Interception::CaptivePortal,
        detail: "HTTP 511 network authentication required".into(),
      });
    }
    Err(ApiError::Status {
      status,
      maybe_body: None,
    })
  }
}

impl Drop for Transfer {
  fn drop(&mut self) {
    if !self.is_finished {
      // Deliberately ignored: a request that already ended can't be aborted.
      let _ = self.xhr.abort();
    }
  }
}

fn is_html(xhr: &XmlHttpRequest) -> bool {
  xhr
    .get_response_header("content-type")
    .ok()
    .flatten()
    .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
}

fn js_error(error: wasm_bindgen::JsValue) -> ApiError {
  ApiError::Network(format!("{error:?}"))
}
