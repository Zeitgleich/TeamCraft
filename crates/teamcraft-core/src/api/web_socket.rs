//! A browser `WebSocket` as an async stream of frames (gateway and device-linking relay).
//!
//! The socket's event handlers push into a channel; [`BrowserSocket::next`] reads it. Everything
//! runs on the page's (or worker's) single thread.

use futures_channel::mpsc;
use futures_util::StreamExt;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use super::ApiError;
use crate::platform::describe_js_error;

/// An open WebSocket. Closing happens on drop.
pub(crate) struct BrowserSocket {
  socket: WebSocket,
  events: mpsc::UnboundedReceiver<SocketEvent>,
  _handlers: Handlers,
}

/// A frame received from the server.
pub(crate) enum SocketFrame {
  Text(String),
  Binary(Vec<u8>),
}

enum SocketEvent {
  Open,
  Frame(SocketFrame),
  Error,
  Closed { code: u16 },
}

/// The callbacks registered on the socket; they must outlive it.
struct Handlers {
  _on_open: Closure<dyn FnMut(Event)>,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
  _on_error: Closure<dyn FnMut(Event)>,
  _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl BrowserSocket {
  /// Opens the socket and waits until the server accepted it.
  pub(crate) async fn connect(url: &str) -> Result<Self, ApiError> {
    let socket = WebSocket::new(url).map_err(|error| ApiError::Network(describe_js_error(&error)))?;
    socket.set_binary_type(BinaryType::Arraybuffer);
    let (sender, events) = mpsc::unbounded();
    let handlers = Handlers::attach(&socket, &sender);
    let mut connected = Self {
      socket,
      events,
      _handlers: handlers,
    };
    match connected.events.next().await {
      Some(SocketEvent::Open) => Ok(connected),
      Some(SocketEvent::Closed { code }) => Err(ApiError::Network(format!(
        "WebSocket closed while connecting (code {code})"
      ))),
      _ => Err(ApiError::Network("WebSocket connection failed".into())),
    }
  }

  pub(crate) fn send_text(&self, text: &str) -> Result<(), ApiError> {
    self
      .socket
      .send_with_str(text)
      .map_err(|error| ApiError::Network(describe_js_error(&error)))
  }

  pub(crate) fn send_binary(&self, bytes: &[u8]) -> Result<(), ApiError> {
    self
      .socket
      .send_with_u8_array(bytes)
      .map_err(|error| ApiError::Network(describe_js_error(&error)))
  }

  /// The next frame; `None` once the socket closed.
  pub(crate) async fn next(&mut self) -> Option<Result<SocketFrame, ApiError>> {
    loop {
      match self.events.next().await? {
        SocketEvent::Frame(frame) => return Some(Ok(frame)),
        // A close event always follows; report the error once, then end the stream.
        SocketEvent::Error => return Some(Err(ApiError::Network("WebSocket error".into()))),
        SocketEvent::Closed { .. } => return None,
        SocketEvent::Open => {},
      }
    }
  }
}

impl Drop for BrowserSocket {
  fn drop(&mut self) {
    self.socket.set_onopen(None);
    self.socket.set_onmessage(None);
    self.socket.set_onerror(None);
    self.socket.set_onclose(None);
    // Closing an already closed socket is a no-op; an error here leaves nothing to clean up.
    let _ = self.socket.close();
  }
}

impl Handlers {
  fn attach(socket: &WebSocket, sender: &mpsc::UnboundedSender<SocketEvent>) -> Self {
    // Sending fails only once the BrowserSocket (the receiver) is gone; then nobody listens.
    let open = sender.clone();
    let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
      let _ = open.unbounded_send(SocketEvent::Open);
    });
    let message = sender.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
      if let Some(frame) = frame_of(&event) {
        let _ = message.unbounded_send(SocketEvent::Frame(frame));
      }
    });
    let error = sender.clone();
    let on_error = Closure::<dyn FnMut(Event)>::new(move |_| {
      let _ = error.unbounded_send(SocketEvent::Error);
    });
    let close = sender.clone();
    let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
      let _ = close.unbounded_send(SocketEvent::Closed { code: event.code() });
    });
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    Self {
      _on_open: on_open,
      _on_message: on_message,
      _on_error: on_error,
      _on_close: on_close,
    }
  }
}

fn frame_of(event: &MessageEvent) -> Option<SocketFrame> {
  let data = event.data();
  if let Some(text) = data.as_string() {
    return Some(SocketFrame::Text(text));
  }
  // Binary frames arrive as ArrayBuffer (binary type set on connect); Blob never does.
  let buffer = data.dyn_into::<js_sys::ArrayBuffer>().ok()?;
  Some(SocketFrame::Binary(js_sys::Uint8Array::new(&buffer).to_vec()))
}
