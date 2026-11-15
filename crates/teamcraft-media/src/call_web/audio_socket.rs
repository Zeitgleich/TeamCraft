//! The browser's second fallback connection, for its own audio (#1979, ARCHITECTURE.md §6.6): on
//! one TCP connection a lost segment holds up the voice behind the camera's video, by up to a second
//! at each loss. Once the SFU hands the session its secret
//! ([`teamcraft_proto::CallServerMessage::AudioSocket`]), this opens a WebSocket of its own to the
//! same address, with the same join token in the upgrade, names the secret first
//! ([`teamcraft_proto::CallClientMessage::AttachAudio`]) and carries the own audio datagrams from
//! then on.
//!
//! It only ever helps: until it is open, while it has more than a moment of audio buffered, and
//! after it closed or was refused (an SFU from before sends no secret at all), the audio goes on the
//! session's connection as before. Nothing on it is read but its close; it closes with its session.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Array;
use teamcraft_proto::{Base64Bytes, CallClientMessage, FallbackFrame};
use tracing::debug;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::Closure;
use web_sys::{BinaryType, WebSocket};

/// A close code as the WebSocket itself says "on purpose".
const NORMAL_CLOSURE: u16 = 1000;
/// Audio the browser may hold for this connection before the session's connection takes the next
/// packets: half a second of the full voice. More is a connection that stalls, and its voice would
/// come too late anyway.
const MAX_BUFFERED_BYTES: u32 = 2048;

/// Where the session's audio connection goes, and the connection once it is attached.
pub(super) struct AudioSocket {
  url: String,
  protocols: Vec<String>,
  maybe_attached: RefCell<Option<Attached>>,
}

/// An audio connection and its handlers.
struct Attached {
  socket: WebSocket,
  /// It is open, and its first frame named the secret.
  is_open: Rc<Cell<bool>>,
  /// It closed or failed: the audio stays on the session's connection.
  has_ended: Rc<Cell<bool>>,
  on_open: Closure<dyn FnMut()>,
  on_end: Closure<dyn FnMut()>,
}

impl AudioSocket {
  /// For a session on `url`, upgraded with `protocols` (the join token, #1297).
  pub(super) fn new(url: &str, protocols: Vec<String>) -> Self {
    Self {
      url: url.to_owned(),
      protocols,
      maybe_attached: RefCell::new(None),
    }
  }

  /// Opens the audio connection for the session's `secret`, in place of any earlier one.
  pub(super) fn attach(&self, secret: Base64Bytes) {
    self.close();
    let Ok(attach) = serde_json::to_vec(&CallClientMessage::AttachAudio { secret }) else {
      return;
    };
    let protocols = Array::new();
    for protocol in &self.protocols {
      protocols.push(&JsValue::from_str(protocol));
    }
    let socket = match WebSocket::new_with_str_sequence(&self.url, &protocols) {
      Ok(socket) => socket,
      Err(error) => {
        debug!(?error, "the audio connection could not be opened");
        return;
      },
    };
    socket.set_binary_type(BinaryType::Arraybuffer);
    let is_open = Rc::new(Cell::new(false));
    let has_ended = Rc::new(Cell::new(false));
    let on_open = {
      let (socket, is_open) = (socket.clone(), Rc::clone(&is_open));
      Closure::<dyn FnMut()>::new(move || {
        // The first frame names the session; the audio follows it.
        let _ = socket.send_with_u8_array(&FallbackFrame::Signal(&attach).encode());
        is_open.set(true);
        debug!("the audio connection is open");
      })
    };
    let on_end = {
      let (is_open, has_ended) = (Rc::clone(&is_open), Rc::clone(&has_ended));
      Closure::<dyn FnMut()>::new(move || {
        if !has_ended.replace(true) {
          debug!("the audio connection ended: the audio goes on the session's connection");
        }
        is_open.set(false);
      })
    };
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_end.as_ref().unchecked_ref()));
    socket.set_onerror(Some(on_end.as_ref().unchecked_ref()));
    *self.maybe_attached.borrow_mut() = Some(Attached {
      socket,
      is_open,
      has_ended,
      on_open,
      on_end,
    });
  }

  /// Sends an audio datagram on the audio connection; `false` when it can't take it now, and the
  /// session's connection should.
  pub(super) fn try_send(&self, datagram: &[u8]) -> bool {
    let attached = self.maybe_attached.borrow();
    let Some(attached) = attached.as_ref() else {
      return false;
    };
    if !attached.is_open.get()
      || attached.has_ended.get()
      || attached.socket.buffered_amount() > MAX_BUFFERED_BYTES
    {
      return false;
    }
    attached
      .socket
      .send_with_u8_array(&FallbackFrame::Datagram(datagram).encode())
      .is_ok()
  }

  /// Closes the audio connection, if there is one.
  pub(super) fn close(&self) {
    let Some(attached) = self.maybe_attached.borrow_mut().take() else {
      return;
    };
    attached.socket.set_onopen(None);
    attached.socket.set_onclose(None);
    attached.socket.set_onerror(None);
    if let Err(error) = attached.socket.close_with_code(NORMAL_CLOSURE) {
      debug!(?error, "closing the audio connection failed");
    }
    // The handlers go with it; the socket may no longer fire.
    let _ = (&attached.on_open, &attached.on_end);
  }
}

impl Drop for AudioSocket {
  fn drop(&mut self) {
    self.close();
  }
}
