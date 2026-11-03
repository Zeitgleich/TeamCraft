//! The WebSocket fallback for networks that block UDP (ARCHITECTURE.md §6.6): the same calls as on
//! the QUIC endpoint, over one TCP connection that a browser can open and a reverse proxy can pass.
//!
//! Plain HTTP on its own listener: TLS terminates at the proxy, as for the server. A session is
//! admitted as a QUIC one is: the first frame is `join`, with the same token and the same checks
//! ([`crate::server::admit`]). From then on it is a participant like any other, with a
//! [`ParticipantLink`] that queues instead of sending datagrams. What differs is what TCP makes
//! different: each connection's memory is bounded by its [`FallbackSender`]'s queue and the
//! WebSocket limits below, a peer that stops reading is dropped after the write timeout, and the
//! number of connections has a limit of its own.
//!
//! Limits as on QUIC: pre-join sessions per address, the join timeout, the signaling message size
//! and queue, and everything rooms enforce (they don't know which transport a participant uses).

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use socket2::SockRef;
use teamcraft_media::fallback_socket::{ControlFrames, FallbackSender, WriterConfig, WriterEnd, run_writer};
use teamcraft_proto::{
  CALL_FALLBACK_PATH, CallClientMessage, CallServerMessage, FallbackFrame, MAX_FALLBACK_DATAGRAM_BYTES,
  SFU_GOING_AWAY_CLOSE_CODE,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Bytes, Error as WebSocketError, Message, error::ProtocolError};
use tracing::{Instrument, debug, info_span, warn};

use crate::config::FallbackConfig;
use crate::room::{ParticipantHandle, ParticipantLink, SESSION_CLOSE_TIMEOUT};
use crate::server::{Admission, PRE_JOIN_LIMIT, ParticipantSession, SessionEnd, Shared, admit, release_slot};
use crate::signaling::{EventSequence, Outgoing};

/// The close code of a session refused before it joined.
const REFUSED: u32 = 0;
/// What's left of a session after it ended, to get its last frames out.
const WRITER_FINISH_TIMEOUT: Duration = Duration::from_secs(1);
/// The WebSocket's write buffer: enough to batch a few datagrams into one TCP write, small enough
/// that what waits behind a slow peer waits in the queue, where it can still be dropped.
const WRITE_BUFFER_BYTES: usize = 16 * 1024;

type Sink = SplitSink<WebSocketStream<TcpStream>, Message>;
type Source = SplitStream<WebSocketStream<TcpStream>>;

/// A participant on the fallback, as rooms see it.
struct FallbackLink(FallbackSender);

/// Signaling events of one session as frames for the writer.
struct OutgoingFrames {
  outgoing: mpsc::Receiver<Outgoing>,
  sequence: EventSequence,
}

/// Why a session on the fallback stopped reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadEnd {
  /// The peer closed on purpose, or broke the protocol: as a QUIC session that failed on its own,
  /// no slot is kept.
  Closed,
  /// The connection broke, or nothing arrived for the idle timeout: the client is expected back.
  Lost,
}

/// Accepts connections until the task is dropped. One over the limit is closed at once.
pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>) {
  let limit = Arc::new(Semaphore::new(shared.config.fallback.max_connections));
  let mut connection_number: u64 = 0;
  loop {
    let (stream, peer) = match listener.accept().await {
      Ok(accepted) => accepted,
      Err(error) => {
        // Out of descriptors, or a connection gone before it was accepted: the next may work.
        debug!(%error, "accepting a fallback connection failed");
        tokio::time::sleep(Duration::from_millis(50)).await;
        continue;
      },
    };
    let Ok(permit) = Arc::clone(&limit).try_acquire_owned() else {
      warn!("too many fallback connections, refusing one");
      continue;
    };
    connection_number = connection_number.wrapping_add(1);
    let shared = Arc::clone(&shared);
    tokio::spawn(
      async move {
        shared.fallback_connections.fetch_add(1, Ordering::Relaxed);
        handle_connection(stream, peer, &shared).await;
        shared.fallback_connections.fetch_sub(1, Ordering::Relaxed);
        drop(permit);
      }
      .instrument(info_span!("fallback_session", connection_number)),
    );
  }
}

/// A connection from the upgrade to its end. Everything before the join has the join timeout.
// tungstenite fixes the type of the handshake callback's error.
#[allow(clippy::result_large_err)]
async fn handle_connection(stream: TcpStream, peer: SocketAddr, shared: &Shared) {
  let config = &shared.config;
  configure_socket(&stream, &config.fallback);
  let mut maybe_forwarded_ip = None;
  let upgrade = tokio_tungstenite::accept_hdr_async_with_config(
    stream,
    |request: &Request, response: Response| {
      if request.uri().path() != CALL_FALLBACK_PATH {
        return Err(not_found());
      }
      if config.fallback.trust_forwarded_for {
        maybe_forwarded_ip = forwarded_for(request);
      }
      Ok(response)
    },
    Some(socket_config(config.max_signaling_message_bytes)),
  );
  let socket = match tokio::time::timeout(config.join_timeout, upgrade).await {
    Ok(Ok(socket)) => socket,
    Ok(Err(error)) => {
      debug!(%error, "fallback handshake failed");
      return;
    },
    Err(_) => {
      debug!("fallback handshake timed out");
      return;
    },
  };
  let (mut sink, mut source) = socket.split();
  if shared.rooms.is_going_away() {
    close_now(&mut sink, SFU_GOING_AWAY_CLOSE_CODE, b"going away").await;
    return;
  }
  let remote_ip = maybe_forwarded_ip.unwrap_or_else(|| peer.ip().to_canonical());
  let Some(prejoin_slot) = shared
    .prejoin
    .try_acquire(remote_ip, config.max_prejoin_sessions_per_ip)
  else {
    close_now(&mut sink, PRE_JOIN_LIMIT, b"too many sessions").await;
    return;
  };
  debug!(%remote_ip, "fallback session accepted");

  let first = tokio::time::timeout(config.join_timeout, read_first_message(&mut source)).await;
  let Ok(Some(first)) = first else {
    close_now(&mut sink, REFUSED, b"expected join").await;
    return;
  };
  let sender = FallbackSender::new(config.fallback.queue);
  let (signal_tx, signal_rx) = mpsc::channel(config.signaling_queue_len.max(1));
  let handle = ParticipantHandle {
    link: Arc::new(FallbackLink(sender.clone())),
    signal_tx: signal_tx.clone(),
  };
  let admitted = match admit(shared, first, handle) {
    Admission::Admitted(admitted) => admitted,
    Admission::Refused(refusal) => {
      send_refusal(&mut sink, &refusal.message()).await;
      close_now(&mut sink, REFUSED, b"refused").await;
      if let Err(error) = refusal.result {
        debug!(%error, "fallback session refused");
      }
      return;
    },
  };
  drop(prejoin_slot);
  let (room, grant, session_id) = (admitted.room, admitted.grant, admitted.session_id);
  let session = ParticipantSession::new(Arc::clone(&room), grant, signal_tx, shared);
  let writer_config = WriterConfig {
    ping_interval: config.fallback.ping_interval,
    write_timeout: config.fallback.write_timeout,
  };
  let frames = OutgoingFrames {
    outgoing: signal_rx,
    sequence: EventSequence::default(),
  };
  let writer = tokio::spawn(run_writer(sink, frames, sender.clone(), writer_config));
  let end = run_session(&session, source, writer, &sender, config.idle_timeout).await;
  debug!(
    dropped_packets = sender.dropped_packets(),
    "media dropped on this connection"
  );
  drop(session);
  release_slot(shared, &room, grant, session_id, end).await;
}

/// Reads and runs the session's periodic work until something ends it.
async fn run_session(
  session: &ParticipantSession,
  mut source: Source,
  mut writer: tokio::task::JoinHandle<WriterEnd>,
  sender: &FallbackSender,
  idle_timeout: Duration,
) -> SessionEnd {
  let end = tokio::select! {
    end = read_frames(session, &mut source, idle_timeout) => match end {
      ReadEnd::Closed => SessionEnd::Closed,
      ReadEnd::Lost => SessionEnd::Lost,
    },
    () = session.periodic() => SessionEnd::Closed,
    written = &mut writer => match written {
      // The SFU closed it (a host control, a replaced connection).
      Ok(WriterEnd::Closed) => SessionEnd::Closed,
      Ok(WriterEnd::Broken) | Err(_) => SessionEnd::Lost,
    },
  };
  // A close tells the client not to come back, which only a deliberate end may: after a loss the
  // socket just goes, and the client rejoins. The last frames get out, but not for long.
  if end == SessionEnd::Closed {
    sender.close(REFUSED, b"closing");
    if !writer.is_finished()
      && tokio::time::timeout(WRITER_FINISH_TIMEOUT, &mut writer)
        .await
        .is_err()
    {
      debug!("the last frames did not get out in time");
    }
  }
  writer.abort();
  debug!(?end, "fallback session closed");
  end
}

async fn read_frames(session: &ParticipantSession, source: &mut Source, idle_timeout: Duration) -> ReadEnd {
  loop {
    let Ok(maybe_item) = tokio::time::timeout(idle_timeout, source.next()).await else {
      debug!("nothing arrived on the fallback connection for the idle timeout");
      return ReadEnd::Lost;
    };
    let message = match maybe_item {
      Some(Ok(message)) => message,
      None => return ReadEnd::Lost,
      Some(Err(error)) => return read_error_end(&error),
    };
    match message {
      Message::Binary(bytes) => {
        if let Some(end) = handle_frame(session, &bytes) {
          return end;
        }
      },
      // tungstenite answers pings by itself. These prove the path alive, nothing more.
      Message::Ping(_) | Message::Pong(_) => {},
      Message::Close(maybe_frame) => {
        // A page that goes away closes with 1001 and hasn't time for more; anything else (or none
        // at all) didn't mean to leave the call.
        let is_on_purpose =
          maybe_frame.is_some_and(|frame| matches!(frame.code, CloseCode::Normal | CloseCode::Away));
        return if is_on_purpose {
          ReadEnd::Closed
        } else {
          ReadEnd::Lost
        };
      },
      // Never sent by a client of ours.
      Message::Text(_) | Message::Frame(_) => return ReadEnd::Closed,
    }
  }
}

/// `Some` if this frame ends the session.
fn handle_frame(session: &ParticipantSession, bytes: &[u8]) -> Option<ReadEnd> {
  match FallbackFrame::decode(bytes) {
    Ok(FallbackFrame::Datagram(datagram)) => session.on_datagram(datagram),
    Ok(FallbackFrame::Signal(body)) => match serde_json::from_slice::<CallClientMessage>(body) {
      Ok(message) => session.handle_client_message(message),
      // As on the QUIC signaling stream: whoever sends this isn't a client of ours.
      Err(error) => {
        debug!(%error, "invalid signaling message on the fallback");
        return Some(ReadEnd::Closed);
      },
    },
    Ok(FallbackFrame::Close { .. }) => return Some(ReadEnd::Closed),
    Ok(FallbackFrame::Unknown(kind)) => debug!(kind, "ignoring a frame of a newer client"),
    Err(error) => {
      debug!(%error, "invalid frame on the fallback");
      return Some(ReadEnd::Closed);
    },
  }
  None
}

/// A reset or a socket that just ended is a loss. Anything else is the peer's doing: frames over the
/// limit, bad masking, a broken protocol.
fn read_error_end(error: &WebSocketError) -> ReadEnd {
  match error {
    WebSocketError::Io(_)
    | WebSocketError::ConnectionClosed
    | WebSocketError::AlreadyClosed
    | WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake) => ReadEnd::Lost,
    other => {
      debug!(error = %other, "the peer broke the WebSocket protocol");
      ReadEnd::Closed
    },
  }
}

/// The first signaling message, skipping pings. `None` if the connection ends or anything else comes.
async fn read_first_message(source: &mut Source) -> Option<CallClientMessage> {
  loop {
    match source.next().await? {
      Ok(Message::Binary(bytes)) => match FallbackFrame::decode(&bytes) {
        Ok(FallbackFrame::Signal(body)) => return serde_json::from_slice(body).ok(),
        _ => return None,
      },
      Ok(Message::Ping(_) | Message::Pong(_)) => {},
      _ => return None,
    }
  }
}

/// Best effort: the session is being closed anyway.
async fn close_now(sink: &mut Sink, code: u32, reason: &[u8]) {
  let frame = FallbackFrame::Close { code, reason }.encode();
  let closing = async {
    sink.send(Message::Binary(Bytes::from(frame))).await?;
    sink.close().await
  };
  if !matches!(
    tokio::time::timeout(WRITER_FINISH_TIMEOUT, closing).await,
    Ok(Ok(()))
  ) {
    debug!("the close did not reach the fallback connection");
  }
}

/// Best effort, like the refusal on QUIC's signaling stream.
async fn send_refusal(sink: &mut Sink, error: &CallServerMessage) {
  let Ok(body) = serde_json::to_vec(error) else {
    return;
  };
  let frame = FallbackFrame::Signal(&body).encode();
  let _ = tokio::time::timeout(
    WRITER_FINISH_TIMEOUT,
    sink.send(Message::Binary(Bytes::from(frame))),
  )
  .await;
}

/// What TCP holds back shouldn't be more than the queue in front of it can still drop, and a voice
/// packet shouldn't wait for more to send along.
fn configure_socket(stream: &TcpStream, config: &FallbackConfig) {
  if let Err(error) = stream.set_nodelay(true) {
    debug!(%error, "could not turn off Nagle's algorithm on a fallback connection");
  }
  // The kernel's auto-tuned buffer can hold seconds of media on a narrow link.
  if let Err(error) = SockRef::from(stream).set_send_buffer_size(config.socket_send_buffer_bytes) {
    debug!(%error, "could not size a fallback connection's send buffer");
  }
}

/// Bounds what one peer can make the socket hold, in both directions. A message is at most a
/// signaling message and its kind byte, or a datagram.
fn socket_config(max_signaling_message_bytes: usize) -> WebSocketConfig {
  let max_message_bytes = max_signaling_message_bytes.max(MAX_FALLBACK_DATAGRAM_BYTES) + 1;
  WebSocketConfig::default()
    .max_message_size(Some(max_message_bytes))
    .max_frame_size(Some(max_message_bytes))
    .write_buffer_size(WRITE_BUFFER_BYTES)
    .max_write_buffer_size(WRITE_BUFFER_BYTES + max_message_bytes)
}

/// The last entry of `X-Forwarded-For`: what the proxy in front of us added itself. Whatever a
/// client wrote there comes before it.
fn forwarded_for(request: &Request) -> Option<IpAddr> {
  let value = request.headers().get("x-forwarded-for")?.to_str().ok()?;
  let last = value.rsplit(',').next()?.trim();
  last.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
}

fn not_found() -> ErrorResponse {
  let mut response = ErrorResponse::new(None);
  *response.status_mut() = StatusCode::NOT_FOUND;
  response
}

impl ParticipantLink for FallbackLink {
  fn send_datagram(&self, bytes: &[u8]) {
    self.0.send_datagram(bytes);
  }

  fn close(&self, code: u32, reason: &[u8]) {
    self.0.close(code, reason);
  }

  fn close_gracefully<'a>(
    &'a self,
    code: u32,
    reason: &'a [u8],
  ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
      self.0.close(code, reason);
      if tokio::time::timeout(SESSION_CLOSE_TIMEOUT, self.0.wait_finished())
        .await
        .is_err()
      {
        debug!("the peer did not take the close in time");
      }
    })
  }
}

impl ControlFrames for OutgoingFrames {
  async fn next_frame(&mut self) -> Option<Vec<u8>> {
    loop {
      let outgoing = self.outgoing.recv().await?;
      match serde_json::to_vec(&self.sequence.frame(outgoing)) {
        Ok(body) => return Some(FallbackFrame::Signal(&body).encode()),
        Err(error) => warn!(%error, "could not encode a signaling message"),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use tokio_tungstenite::tungstenite::http::Request as HttpRequest;

  use super::*;

  mod forwarded_addresses {
    use super::*;

    #[test]
    fn the_last_entry_counts_whatever_the_client_put_before_it() {
      let request = request_with("198.51.100.7, 203.0.113.9");
      assert_eq!(forwarded_for(&request), "203.0.113.9".parse().ok());
    }

    #[test]
    fn ipv6_and_ipv4_mapped_addresses_come_out_canonical() {
      assert_eq!(
        forwarded_for(&request_with("2001:db8::1")),
        "2001:db8::1".parse().ok()
      );
      assert_eq!(
        forwarded_for(&request_with("::ffff:192.0.2.1")),
        "192.0.2.1".parse().ok()
      );
    }

    #[test]
    fn garbage_is_no_address() {
      assert_eq!(forwarded_for(&request_with("not an address")), None);
      assert_eq!(forwarded_for(&request_with("192.0.2.1,")), None);
    }

    fn request_with(value: &str) -> Request {
      HttpRequest::builder()
        .header("x-forwarded-for", value)
        .body(())
        .unwrap()
    }
  }

  mod read_errors {
    use super::*;

    #[test]
    fn a_broken_connection_is_a_loss() {
      for error in [
        WebSocketError::ConnectionClosed,
        WebSocketError::AlreadyClosed,
        WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake),
        WebSocketError::Io(std::io::ErrorKind::ConnectionReset.into()),
      ] {
        assert_eq!(read_error_end(&error), ReadEnd::Lost, "{error}");
      }
    }

    #[test]
    fn breaking_the_protocol_keeps_no_slot() {
      let error = WebSocketError::Protocol(ProtocolError::UnmaskedFrameFromClient);
      assert_eq!(read_error_end(&error), ReadEnd::Closed);
    }
  }

  #[test]
  fn the_message_limit_fits_a_signaling_message_or_a_datagram_and_its_kind() {
    let small = socket_config(100);
    assert_eq!(small.max_message_size, Some(MAX_FALLBACK_DATAGRAM_BYTES + 1));
    let large = socket_config(256 * 1024);
    assert_eq!(large.max_message_size, Some(256 * 1024 + 1));
    assert_eq!(large.max_frame_size, Some(256 * 1024 + 1));
  }
}
