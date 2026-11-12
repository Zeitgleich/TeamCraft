//! The browser's side of a call's WebSocket fallback (ARCHITECTURE.md §6.6): a session that delivers
//! the same [`Input`]s as a WebTransport one. The browser answers the SFU's pings by itself, also
//! while the page is throttled, so the SFU sees the path alive; the page proves it to itself the way
//! it does on Safari, by what it hears and the SFU's answers to its pings (`session_watch`).
//!
//! A browser's WebSocket can't drop what it has buffered, only refuse to add to it
//! ([`crate::fallback_queue::may_buffer`]).

use std::cell::Cell;
use std::rc::Rc;

use futures::channel::{mpsc, oneshot};
use js_sys::{ArrayBuffer, Uint8Array};
use teamcraft_proto::{CallServerMessage, FallbackFrame};
use tracing::{debug, warn};
// The browser clock: `std` has none on wasm.
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};
use web_time::Instant;

use super::{Input, LEAVE_CLOSE_CODE, MAX_SIGNALING_BYTES, SessionEnd, WebCallError, session_end};
use crate::fallback_queue::may_buffer;

/// A close code as the WebSocket itself says "on purpose".
const NORMAL_CLOSURE: u16 = 1000;

/// An open socket and the handlers that feed the call task.
pub(super) struct FallbackSession {
  socket: WebSocket,
  on_message: Closure<dyn FnMut(MessageEvent)>,
  on_close: Closure<dyn FnMut()>,
}

impl FallbackSession {
  /// Resolves when the socket is open. `socket` is the caller's to close if this future is dropped.
  pub(super) async fn open(
    socket: WebSocket,
    inputs: &mpsc::UnboundedSender<Input>,
    generation: u64,
  ) -> Result<Self, WebCallError> {
    socket.set_binary_type(BinaryType::Arraybuffer);
    wait_until_open(&socket).await?;
    let has_ended = Rc::new(Cell::new(false));
    let on_message = message_handler(inputs.clone(), generation, Rc::clone(&has_ended));
    let on_close = close_handler(inputs.clone(), generation, has_ended);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
    Ok(Self {
      socket,
      on_message,
      on_close,
    })
  }

  /// A write that fails surfaces as the socket closing.
  pub(super) fn send_signal(&self, body: &[u8]) {
    let _ = self
      .socket
      .send_with_u8_array(&FallbackFrame::Signal(body).encode());
  }

  /// Unreliable by design: while the browser's buffer is too full, the datagram is dropped.
  pub(super) fn send_datagram(&self, bytes: &[u8]) {
    if may_buffer(self.socket.buffered_amount(), bytes) {
      let _ = self
        .socket
        .send_with_u8_array(&FallbackFrame::Datagram(bytes).encode());
    }
  }
}

impl Drop for FallbackSession {
  fn drop(&mut self) {
    // The handlers go with this value: the socket may still fire.
    self.socket.set_onmessage(None);
    self.socket.set_onclose(None);
    // Keeps the closures' last use in one place; they are dropped after this.
    let _ = (&self.on_message, &self.on_close);
  }
}

/// Closes a socket as a leave: the frame says so with its code, and a socket that can't send any
/// more still closes.
pub(super) fn close_as_leave(socket: &WebSocket) {
  let frame = FallbackFrame::Close {
    code: LEAVE_CLOSE_CODE,
    reason: b"leave",
  }
  .encode();
  let _ = socket.send_with_u8_array(&frame);
  if let Err(error) = socket.close_with_code(NORMAL_CLOSURE) {
    debug!(error = ?error, "closing a call socket failed");
  }
}

async fn wait_until_open(socket: &WebSocket) -> Result<(), WebCallError> {
  let (opened_tx, opened) = oneshot::channel::<bool>();
  let opened_tx = Rc::new(Cell::new(Some(opened_tx)));
  let settle = |is_open: bool| {
    let opened_tx = Rc::clone(&opened_tx);
    Closure::<dyn FnMut()>::new(move || {
      if let Some(sender) = opened_tx.take() {
        // Nobody waits any more when the race was decided without this socket.
        let _ = sender.send(is_open);
      }
    })
  };
  let (on_open, on_error) = (settle(true), settle(false));
  socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));
  socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));
  let is_open = opened.await.unwrap_or(false);
  socket.set_onopen(None);
  socket.set_onerror(None);
  if is_open {
    return Ok(());
  }
  // The browser says nothing more: why a WebSocket failed is hidden from pages.
  Err(WebCallError::Browser {
    context: "call socket",
    message: "could not connect".into(),
  })
}

fn message_handler(
  inputs: mpsc::UnboundedSender<Input>,
  generation: u64,
  has_ended: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(MessageEvent)> {
  Closure::new(move |event: MessageEvent| {
    let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
      return;
    };
    let bytes = Uint8Array::new(&buffer).to_vec();
    match FallbackFrame::decode(&bytes) {
      Ok(FallbackFrame::Datagram(_)) => {
        let mut bytes = bytes;
        bytes.remove(0);
        let _ = inputs.unbounded_send(Input::Datagram {
          generation,
          bytes,
          read_at: Instant::now(),
        });
      },
      Ok(FallbackFrame::Signal(body)) if body.len() > MAX_SIGNALING_BYTES => {
        warn!(length = body.len(), "signaling message too large");
      },
      Ok(FallbackFrame::Signal(body)) => match serde_json::from_slice::<CallServerMessage>(body) {
        Ok(message) => {
          let _ = inputs.unbounded_send(Input::Signal { generation, message });
        },
        Err(error) => warn!(%error, "invalid signaling message"),
      },
      Ok(FallbackFrame::Close { code, .. }) => {
        has_ended.set(true);
        let end = session_end(Some(code));
        let _ = inputs.unbounded_send(Input::SessionEnded { generation, end });
      },
      Ok(FallbackFrame::Unknown(kind)) => debug!(kind, "ignoring a frame of a newer SFU"),
      Err(error) => warn!(%error, "invalid frame on the call socket"),
    }
  })
}

/// A socket that closes without a close frame broke: the call rejoins.
fn close_handler(
  inputs: mpsc::UnboundedSender<Input>,
  generation: u64,
  has_ended: Rc<Cell<bool>>,
) -> Closure<dyn FnMut()> {
  Closure::new(move || {
    if !has_ended.replace(true) {
      let _ = inputs.unbounded_send(Input::SessionEnded {
        generation,
        end: SessionEnd::Lost,
      });
    }
  })
}
