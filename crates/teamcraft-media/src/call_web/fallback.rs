//! The browser's side of a call's WebSocket fallback (ARCHITECTURE.md §6.6): a session that delivers
//! the same [`Input`]s as a WebTransport one. The browser answers the SFU's pings by itself, also
//! while the page is throttled, so the SFU sees the path alive; the page proves it to itself the way
//! it does on Safari, by what it hears and the SFU's answers to its pings (`session_watch`).
//!
//! A browser's WebSocket can't drop what it has buffered, only refuse to add to it
//! ([`crate::fallback_queue::may_buffer`]). And `bufferedAmount` doesn't see what the browser already
//! passed on to its network process and the kernel, which hold seconds of video on a narrow uplink:
//! the camera filled it, and the session's pings and the SFU's answers waited behind it until the
//! SFU gave the session up (#1848). So the upload is paced like the SFU's download (#1810): video
//! waits in a [`MediaQueue`] (whole frames dropped when stale, with a keyframe asked of the own
//! encoder) and goes no faster than the path drains, measured with the session's own pings
//! ([`DrainPacer`]); audio and signaling aren't held back. The pacer's rate is also what the own
//! layers are planned for ([`crate::call_engine::CallEngine::set_uplink_capacity`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures::channel::{mpsc, oneshot};
use js_sys::{ArrayBuffer, Uint8Array};
use teamcraft_proto::{CallClientMessage, CallServerMessage, FallbackFrame};
use tracing::{debug, warn};
// The browser clock: `std` has none on wasm.
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};
use web_time::Instant;

use super::{Input, LEAVE_CLOSE_CODE, MAX_SIGNALING_BYTES, SessionEnd, WebCallError, session_end};
use crate::datagram::StreamId;
use crate::fallback_pacer::{DrainPacer, PacingConfig};
use crate::fallback_queue::{MediaQueue, MediaQueueConfig, may_buffer};

/// A close code as the WebSocket itself says "on purpose".
const NORMAL_CLOSURE: u16 = 1000;
/// Marks the pings of the upload's pacer: their answers go to it, not to the session watch, whose
/// pings count from zero.
pub(super) const PACER_PING_FLAG: u64 = 1 << 63;

/// An open socket and the handlers that feed the call task.
pub(super) struct FallbackSession {
  socket: WebSocket,
  on_message: Closure<dyn FnMut(MessageEvent)>,
  on_close: Closure<dyn FnMut()>,
  upload: RefCell<Box<UploadPacing>>,
}

/// The upload's queue and pacer (#1848).
struct UploadPacing {
  queue: MediaQueue,
  pacer: DrainPacer,
  next_probe: u64,
  maybe_probe_sent_at: Option<Instant>,
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
      upload: RefCell::new(Box::new(UploadPacing {
        queue: MediaQueue::new(MediaQueueConfig::default()),
        pacer: DrainPacer::new(PacingConfig::default()),
        next_probe: 0,
        maybe_probe_sent_at: None,
      })),
    })
  }

  /// A write that fails surfaces as the socket closing. Never held back, but counted by the pacer.
  pub(super) fn send_signal(&self, body: &[u8]) {
    self.write(&FallbackFrame::Signal(body).encode(), Instant::now());
  }

  /// Unreliable by design: queued for the pacer, dropped when stale or while the browser's buffer is
  /// too full. Audio goes at once.
  pub(super) fn send_datagram(&self, bytes: &[u8]) {
    let now = Instant::now();
    self.upload.borrow_mut().queue.push(bytes.to_vec(), now);
    self.pump(now);
  }

  /// Sends the pacer's ping when it is due, and what the pacer lets out of the queue now.
  pub(super) fn pump(&self, now: Instant) {
    self.ping_if_due(now);
    loop {
      let maybe_datagram = {
        let mut upload = self.upload.borrow_mut();
        let may_send_video = upload.pacer.wait_for(now).is_zero();
        upload.queue.pop_paced(may_send_video, now)
      };
      let Some(datagram) = maybe_datagram else {
        return;
      };
      if may_buffer(self.socket.buffered_amount(), &datagram) {
        self.write(&FallbackFrame::Datagram(&datagram).encode(), now);
      }
    }
  }

  /// Takes the SFU's answer to one of the pacer's pings; `false` for anybody else's. An SFU that
  /// says when it read the ping lets the pacer judge the upload alone (#1890).
  pub(super) fn on_pong(&self, id: u64, maybe_received_us: Option<u64>, now: Instant) -> bool {
    if id & PACER_PING_FLAG == 0 {
      return false;
    }
    let number = id & !PACER_PING_FLAG;
    let pacer = &mut self.upload.borrow_mut().pacer;
    match maybe_received_us {
      Some(received_us) => pacer.on_answer_received(number, received_us, now),
      None => pacer.on_answer(number, now),
    }
    true
  }

  /// What the upload drains, as the pacer measured it.
  pub(super) fn upload_rate_bps(&self) -> u32 {
    self.upload.borrow().pacer.rate_bps()
  }

  /// Own streams whose video the queue had to drop: their encoders owe a keyframe.
  pub(super) fn take_keyframe_needs(&self) -> Vec<StreamId> {
    self.upload.borrow_mut().queue.take_keyframe_needs()
  }

  fn ping_if_due(&self, now: Instant) {
    let number = {
      let mut upload = self.upload.borrow_mut();
      let interval = upload.pacer.probe_interval();
      if upload
        .maybe_probe_sent_at
        .is_some_and(|at| now.saturating_duration_since(at) < interval)
      {
        return;
      }
      upload.maybe_probe_sent_at = Some(now);
      upload.next_probe += 1;
      upload.next_probe
    };
    let Ok(body) = serde_json::to_vec(&CallClientMessage::Ping {
      id: PACER_PING_FLAG | number,
    }) else {
      return;
    };
    // The ping goes behind everything written before it; its answer says when that drained.
    self.upload.borrow_mut().pacer.on_probe_sent(number, now);
    self.write(&FallbackFrame::Signal(&body).encode(), now);
  }

  /// Writes a frame and counts it for the pacer.
  fn write(&self, frame: &[u8], now: Instant) {
    let _ = self.socket.send_with_u8_array(frame);
    self.upload.borrow_mut().pacer.on_written(
      frame
        .len()
        .saturating_sub(crate::fallback_pacer::FRAME_OVERHEAD_BYTES),
      now,
    );
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
