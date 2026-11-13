//! The browser's call datagrams on WebTransport, through a [`SendQueue`] (#1871), as the native
//! clients send theirs (`crate::quic_send`).
//!
//! A write to the browser's datagram stream that isn't awaited waits in the stream's own queue,
//! oldest first, as long as it takes: when the uplink collapsed (5 Mbit/s to 120 kbit/s from one
//! second to the next), the camera's datagrams of the seconds before stood in front of every voice
//! packet, and the call was silent for as long as the link stayed down. So the stream gets a
//! datagram only while it has room ([`WritableStreamDefaultWriter::desired_size`]), and the rest
//! waits in the [`SendQueue`]: audio first, stale video dropped whole.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Reflect, Uint8Array};
use tracing::debug;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::WritableStreamDefaultWriter;
use web_time::Instant;

use crate::send_queue::{SendQueue, SendQueueConfig};

/// How many datagrams the browser may hold for sending (`outgoingHighWaterMark`): a few packets,
/// so that what waits waits in the [`SendQueue`], where it can be judged.
const BROWSER_HIGH_WATER_MARK: f64 = 8.0;

/// The datagram side of a WebTransport call session.
pub(super) struct QuicDatagrams(Rc<RefCell<State>>);

struct State {
  writer: WritableStreamDefaultWriter,
  queue: SendQueue,
  /// A wait for the stream to have room again is running.
  is_waiting: bool,
}

impl QuicDatagrams {
  /// `stream` is the transport's `datagrams`; `writer` writes to its writable.
  pub(super) fn new(stream: &JsValue, writer: WritableStreamDefaultWriter) -> Self {
    // Ignored if the browser doesn't have it: the stream's own high water mark applies then.
    let _ = Reflect::set(
      stream,
      &JsValue::from_str("outgoingHighWaterMark"),
      &JsValue::from_f64(BROWSER_HIGH_WATER_MARK),
    );
    Self(Rc::new(RefCell::new(State {
      writer,
      queue: SendQueue::new(SendQueueConfig::uplink()),
      is_waiting: false,
    })))
  }

  pub(super) fn send(&self, bytes: &[u8]) {
    self.0.borrow_mut().queue.push(bytes.to_vec(), Instant::now());
    flush(&self.0);
  }
}

/// Hands the stream what it has room for, and waits for room for the rest.
fn flush(state: &Rc<RefCell<State>>) {
  let mut guard = state.borrow_mut();
  let now = Instant::now();
  while has_room(&guard.writer) {
    let Some(datagram) = guard.queue.pop(now) else {
      break;
    };
    // Unreliable by design: a datagram the browser can't send is lost like one on the way, and
    // the write's promise says nothing the transport feedback doesn't.
    let _ = guard
      .writer
      .write_with_chunk(&Uint8Array::from(datagram.as_slice()));
  }
  if guard.queue.is_empty() || guard.is_waiting {
    return;
  }
  guard.is_waiting = true;
  let ready = guard.writer.ready();
  drop(guard);
  let state = Rc::clone(state);
  spawn_local(async move {
    if let Err(error) = JsFuture::from(ready).await {
      // The stream errored or closed: the session ends, and nothing more goes out here.
      debug!(?error, "the datagram stream won't take more");
      return;
    }
    state.borrow_mut().is_waiting = false;
    flush(&state);
  });
}

/// Whether the stream takes another datagram without queueing it: `desiredSize` above zero.
fn has_room(writer: &WritableStreamDefaultWriter) -> bool {
  writer
    .desired_size()
    .ok()
    .flatten()
    .is_some_and(|size| size > 0.0)
}
