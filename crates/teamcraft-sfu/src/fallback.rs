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
//! Unjoined connections are the only ones anybody can open without a token, so they have limits that
//! can't touch anything else: a small cap of their own, a short handshake timeout, and a per-address
//! count separate from QUIC's. Exhausting them costs new fallback joins for a moment, never a joined
//! call, a QUIC session or the SFU's descriptors.
//!
//! The join token comes along with the upgrade too ([`teamcraft_proto::fallback_protocols`]), and
//! is checked there (#1297): a connection with a valid one leaves the anonymous limits at once for a
//! slot of the call's connections and a small count per device, and one with an invalid token gets
//! no upgrade. So somebody with many addresses but no token holds an unjoined slot for the handshake
//! and, as long as clients without the header are let in, for [`FallbackConfig::first_frame_timeout`]
//! more; with [`FallbackConfig::requires_upgrade_token`], for the handshake alone.
//!
//! Limits as on QUIC: the join timeout, the signaling message size and queue, and everything rooms
//! enforce (they don't know which transport a participant uses).
//!
//! A session here moves to QUIC once its client gets through there (#1309, [`crate::switch`]): the
//! writer sends what was queued before the move, hands the queue on and closes with
//! [`SFU_SWITCHED_CLOSE_CODE`]. What the client sent before it learned of the move may still be on
//! its way, so the connection is read a little longer ([`SWITCH_LINGER`]), until the client closes
//! it.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use socket2::SockRef;
use teamcraft_media::fallback_socket::{ControlFrames, FallbackSender, WriterConfig, WriterEnd, run_writer};
use teamcraft_proto::{
  CALL_FALLBACK_PATH, CALL_FALLBACK_PROTOCOL, CallClientMessage, CallServerMessage, ErrorBody, ErrorCode,
  FallbackFrame, MAX_FALLBACK_DATAGRAM_BYTES, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE,
  SFU_SWITCHED_CLOSE_CODE, parse_offered_protocols,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Bytes, Error as WebSocketError, Message, error::ProtocolError};
use tracing::{Instrument, debug, info_span, warn};

use crate::auth::{JoinGrant, JoinTokenVerifier};
use crate::config::FallbackConfig;
use crate::prejoin::PreJoinSlot;
use crate::quality::Transport;
use crate::room::{ParticipantHandle, ParticipantLink, Room, SESSION_CLOSE_TIMEOUT, SessionId};
use crate::server::{
  Admission, ParticipantSession, SessionEnd, Shared, admit, register_switch, release_slot,
};
use crate::signaling::{Dequeued, Handover, SignalQueue};

/// The close code of a session refused before it joined.
const REFUSED: u32 = 0;
/// What's left of a session after it ended, to get its last frames out.
const WRITER_FINISH_TIMEOUT: Duration = Duration::from_secs(1);
/// The WebSocket's write buffer: enough to batch a few datagrams into one TCP write, small enough
/// that what waits behind a slow peer waits in the queue, where it can still be dropped.
const WRITE_BUFFER_BYTES: usize = 16 * 1024;
const FORWARDED_FOR: &str = "x-forwarded-for";
/// How often the pacing rate reaches the layer allocation: as often as the pacer measures.
const LINK_CAPACITY_INTERVAL: Duration = Duration::from_millis(250);
/// After a failed `accept`, typically out of descriptors: the loop mustn't spin on it.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(50);
/// Descriptors the SFU needs besides the fallback's connections: its sockets, logs and requests.
const OTHER_DESCRIPTORS: usize = 64;
/// At most one log line per this long about connections that were refused.
const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);
/// Unjoined connections of one device that showed its token in the upgrade: a rejoin may overlap
/// with a connection still being set up, not more.
const MAX_UNJOINED_PER_DEVICE: usize = 2;
/// How long a connection whose session moved to QUIC is still read, for what its client sent before
/// it learned of the move; the client closes it as soon as it read the close.
const SWITCH_LINGER: Duration = Duration::from_secs(5);
/// What the kernel may hold of a connection's data that it hasn't sent yet (`TCP_NOTSENT_LOWAT`):
/// beyond it a write waits, and what waits can still be dropped in the queue (#1810). A few
/// datagrams, so a write never waits for the next round trip.
#[cfg(any(target_os = "android", target_os = "linux"))]
const NOT_SENT_BYTES: u32 = 16 * 1024;

type Sink = SplitSink<WebSocketStream<TcpStream>, Message>;
type Source = SplitStream<WebSocketStream<TcpStream>>;

/// A participant on the fallback, as rooms see it.
struct FallbackLink(FallbackSender);

/// A connection that passed the checks that cost nothing.
struct Accepted {
  stream: TcpStream,
  peer_ip: IpAddr,
  is_from_proxy: bool,
  unjoined_slot: OwnedSemaphorePermit,
  /// Taken at once for a client's own connection; a proxy's waits for the name it gives.
  maybe_address_slot: Option<PreJoinSlot>,
}

/// What the upgrade said about the join token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UpgradeToken {
  /// The client offered [`CALL_FALLBACK_PROTOCOL`], so the answer names it.
  answers_protocol: bool,
  /// A token that was valid at the upgrade, and whom it lets in.
  maybe_checked: Option<(String, JoinGrant)>,
}

/// A session whose connection is over and whose place in the call isn't yet.
struct EndedSession {
  room: Arc<Room>,
  grant: JoinGrant,
  session_id: SessionId,
  end: SessionEnd,
}

/// Says when connections get refused, without a line per refusal: whoever is causing them would
/// choose the size of the log.
#[derive(Debug, Default)]
struct RefusalLog {
  maybe_logged_at: Option<Instant>,
  unlogged: u64,
}

/// Signaling events of one session as frames for the writer, until the session moves away.
struct OutgoingFrames {
  maybe_queue: Option<SignalQueue>,
  sender: FallbackSender,
  /// The session moved to another connection (#1309).
  has_moved: Arc<AtomicBool>,
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

/// Accepts connections until the task is dropped. What is over a limit is closed at once, before a
/// byte is read: it costs a descriptor for a moment and nothing else.
pub(crate) async fn serve(listener: TcpListener, shared: Arc<Shared>) {
  let config = &shared.config.fallback;
  let joined_slots = Arc::new(Semaphore::new(config.max_connections));
  let unjoined_slots = Arc::new(Semaphore::new(config.max_unjoined_connections));
  let mut refusals = RefusalLog::default();
  let mut connection_number: u64 = 0;
  loop {
    let (stream, peer) = match listener.accept().await {
      Ok(accepted) => accepted,
      Err(error) => {
        // Out of descriptors, or a connection gone before it was accepted: the next may work.
        refusals.note(&error.to_string(), Instant::now());
        tokio::time::sleep(ACCEPT_ERROR_PAUSE).await;
        continue;
      },
    };
    let peer_ip = peer.ip().to_canonical();
    let Ok(unjoined_slot) = Arc::clone(&unjoined_slots).try_acquire_owned() else {
      refusals.note("too many unjoined connections", Instant::now());
      continue;
    };
    // A proxy speaks for many: its clients are counted once the upgrade has named them.
    let is_from_proxy = config.is_trusted_proxy(peer_ip);
    let maybe_address_slot = if is_from_proxy {
      None
    } else {
      let Some(slot) = shared
        .fallback_prejoin
        .try_acquire(peer_ip, shared.config.max_prejoin_sessions_per_ip)
      else {
        refusals.note("too many unjoined connections from one address", Instant::now());
        continue;
      };
      Some(slot)
    };
    connection_number = connection_number.wrapping_add(1);
    let shared = Arc::clone(&shared);
    let joined_slots = Arc::clone(&joined_slots);
    tokio::spawn(
      async move {
        let accepted = Accepted {
          stream,
          peer_ip,
          is_from_proxy,
          unjoined_slot,
          maybe_address_slot,
        };
        shared.fallback_connections.fetch_add(1, Ordering::Relaxed);
        let maybe_ended = handle_connection(accepted, &joined_slots, &shared).await;
        shared.fallback_connections.fetch_sub(1, Ordering::Relaxed);
        // After a loss this sleeps through the reconnect grace period. The connection and its slot
        // are gone by then: the wait mustn't count against anybody's rejoin.
        if let Some(ended) = maybe_ended {
          release_slot(&shared, &ended.room, ended.grant, ended.session_id, ended.end).await;
        }
      }
      .instrument(info_span!("fallback_session", connection_number)),
    );
  }
}

/// A connection from the upgrade to its end. Returns the session that has yet to give up its place
/// in the call, if it joined. Before the join, the upgrade has the handshake timeout and the first
/// frame the join timeout, and the connection holds an unjoined slot, which a client without a token
/// can't get past; a joined one holds a slot of its own, so unjoined ones can't take those away.
// tungstenite fixes the type of the handshake callback's error.
#[allow(clippy::result_large_err)]
async fn handle_connection(
  accepted: Accepted,
  joined_slots: &Arc<Semaphore>,
  shared: &Shared,
) -> Option<EndedSession> {
  let Accepted {
    stream,
    peer_ip,
    is_from_proxy,
    unjoined_slot,
    mut maybe_address_slot,
  } = accepted;
  let config = &shared.config;
  configure_socket(&stream, &config.fallback);
  let mut maybe_forwarded_ip = None;
  let mut maybe_upgrade_token = None;
  let upgrade = tokio_tungstenite::accept_hdr_async_with_config(
    stream,
    |request: &Request, mut response: Response| {
      if request.uri().path() != CALL_FALLBACK_PATH {
        return Err(not_found());
      }
      let token = check_upgrade_token(
        request,
        shared.verifier.as_ref(),
        config.fallback.requires_upgrade_token,
      )
      .map_err(status_response)?;
      if token.answers_protocol {
        response.headers_mut().insert(
          SEC_WEBSOCKET_PROTOCOL,
          HeaderValue::from_static(CALL_FALLBACK_PROTOCOL),
        );
      }
      if is_from_proxy {
        maybe_forwarded_ip = forwarded_for(request);
      } else if request.headers().contains_key(FORWARDED_FOR)
        && !shared.has_warned_of_forwarding.swap(true, Ordering::Relaxed)
      {
        warn!(
          %peer_ip,
          "X-Forwarded-For from an address that isn't a trusted proxy, ignored: behind a proxy, set \
           TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES"
        );
      }
      maybe_upgrade_token = token.maybe_checked;
      Ok(response)
    },
    Some(socket_config(config.max_signaling_message_bytes)),
  );
  let socket = match tokio::time::timeout(config.fallback.handshake_timeout, upgrade).await {
    Ok(Ok(socket)) => socket,
    Ok(Err(error)) => {
      debug!(%error, "fallback handshake failed");
      return None;
    },
    Err(_) => {
      debug!("fallback handshake timed out");
      return None;
    },
  };
  let (mut sink, mut source) = socket.split();
  if shared.rooms.is_going_away() {
    close_now(&mut sink, SFU_GOING_AWAY_CLOSE_CODE, b"going away").await;
    return None;
  }
  // A valid token in the upgrade: the connection counts against its device and the call's
  // connections from now on, not against what anybody may hold without one.
  let mut maybe_joined_slot = None;
  let mut maybe_device_slot = None;
  let mut unjoined_slot = Some(unjoined_slot);
  let first_frame_timeout = if let Some((_, grant)) = &maybe_upgrade_token {
    let Ok(joined_slot) = Arc::clone(joined_slots).try_acquire_owned() else {
      debug!("too many joined connections on the fallback");
      return None;
    };
    let Some(device_slot) = shared
      .fallback_device_prejoin
      .try_acquire_key((grant.call_id, grant.device_id), MAX_UNJOINED_PER_DEVICE)
    else {
      debug!(call_id = %grant.call_id, participant_id = %grant.device_id, "too many unjoined connections of one device");
      return None;
    };
    maybe_joined_slot = Some(joined_slot);
    maybe_device_slot = Some(device_slot);
    maybe_address_slot = None;
    unjoined_slot = None;
    config.join_timeout
  } else {
    config.fallback.first_frame_timeout
  };
  if is_from_proxy && maybe_upgrade_token.is_none() {
    let remote_ip = maybe_forwarded_ip.unwrap_or(peer_ip);
    maybe_address_slot = shared
      .fallback_prejoin
      .try_acquire(remote_ip, config.max_prejoin_sessions_per_ip);
    if maybe_address_slot.is_none() {
      // No close frame: this is no decision about the call, and the client tries again.
      debug!(%remote_ip, "too many unjoined connections from one address");
      return None;
    }
  }

  let first = tokio::time::timeout(first_frame_timeout, read_first_message(&mut source)).await;
  let Ok(Some(first)) = first else {
    close_now(&mut sink, REFUSED, b"expected join").await;
    return None;
  };
  if let Some((token, _)) = &maybe_upgrade_token
    && !is_join_with(&first, token)
  {
    // Somebody else's token, or none: what the upgrade checked has to be what joins.
    let error = ErrorBody::new(
      ErrorCode::InvalidJoinToken,
      "the join token differs from the upgrade's",
    );
    send_refusal(&mut sink, &CallServerMessage::Error { error }).await;
    close_now(&mut sink, REFUSED, b"refused").await;
    return None;
  }
  // A full SFU is no decision about the call either: a client that is rejoining tries again.
  let joined_slot = match maybe_joined_slot {
    Some(joined_slot) => joined_slot,
    None => {
      let Ok(joined_slot) = Arc::clone(joined_slots).try_acquire_owned() else {
        debug!("too many joined connections on the fallback");
        return None;
      };
      joined_slot
    },
  };
  let sender = FallbackSender::paced(config.fallback.queue, config.fallback.pacing);
  let (signal_tx, signal_rx) = mpsc::channel(config.signaling_queue_len.max(1));
  let handover = Arc::new(Handover::default());
  let handle = ParticipantHandle {
    link: Arc::new(FallbackLink(sender.clone())),
    signal_tx: signal_tx.clone(),
  };
  let admitted = match admit(shared, first, handle).await {
    Admission::Admitted(admitted) => admitted,
    Admission::Refused(refusal) => {
      send_refusal(&mut sink, &refusal.message()).await;
      close_now(&mut sink, REFUSED, b"refused").await;
      if let Err(error) = refusal.result {
        debug!(%error, "fallback session refused");
      }
      return None;
    },
  };
  drop(maybe_address_slot);
  drop(maybe_device_slot);
  drop(unjoined_slot);
  let (room, grant, session_id) = (admitted.room, admitted.grant, admitted.session_id);
  let client_ip = if is_from_proxy {
    maybe_forwarded_ip.unwrap_or(peer_ip)
  } else {
    peer_ip
  };
  shared
    .rooms
    .quality()
    .session_joined(Transport::Fallback, client_ip);
  // Dropped when the connection ends, before the slot is: a session that is lost can't move.
  let _registration = register_switch(
    shared,
    admitted.maybe_switch_secret,
    &room,
    grant,
    session_id,
    &handover,
  );
  let session = ParticipantSession::new(Arc::clone(&room), grant, signal_tx, shared);
  let writer_config = WriterConfig {
    ping_interval: config.fallback.ping_interval,
    write_timeout: config.fallback.write_timeout,
  };
  let has_moved = Arc::new(AtomicBool::new(false));
  let frames = OutgoingFrames {
    maybe_queue: Some(SignalQueue::new(signal_rx, handover)),
    sender: sender.clone(),
    has_moved: Arc::clone(&has_moved),
  };
  let writer = tokio::spawn(run_writer(sink, frames, sender.clone(), writer_config));
  let mut end = run_session(&session, &mut source, writer, &sender, config.idle_timeout).await;
  // Its next connection measures its own, or (QUIC) needs no cap.
  session.set_link_capacity(None);
  if has_moved.load(Ordering::Acquire) {
    linger(&session, &mut source, &sender).await;
    // Whatever happened to this connection, the session goes on on the other one.
    end = SessionEnd::Closed;
  }
  debug!(
    dropped_packets = sender.dropped_packets(),
    "media dropped on this connection"
  );
  drop(joined_slot);
  Some(EndedSession {
    room,
    grant,
    session_id,
    end,
  })
}

/// Reads and runs the session's periodic work until something ends it.
async fn run_session(
  session: &ParticipantSession,
  source: &mut Source,
  mut writer: tokio::task::JoinHandle<WriterEnd>,
  sender: &FallbackSender,
  idle_timeout: Duration,
) -> SessionEnd {
  let end = tokio::select! {
    end = read_frames(session, source, sender, idle_timeout) => match end {
      ReadEnd::Closed => SessionEnd::Closed,
      ReadEnd::Lost => SessionEnd::Lost,
    },
    () = session.periodic() => SessionEnd::Closed,
    () = report_link_capacity(session, sender) => SessionEnd::Closed,
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

/// The session moved to QUIC (#1309): what its client sent here before it learned of that still
/// counts. Read until the client closes, for [`SWITCH_LINGER`] at most.
async fn linger(session: &ParticipantSession, source: &mut Source, sender: &FallbackSender) {
  if tokio::time::timeout(SWITCH_LINGER, read_frames(session, source, sender, SWITCH_LINGER))
    .await
    .is_err()
  {
    debug!("the client didn't close the connection its session moved away from");
  }
}

/// The rate the writer paces to is what the connection drains: the session's layers follow it, not
/// only the feedback, which on TCP sees a full buffer late or not at all (#1810).
async fn report_link_capacity(session: &ParticipantSession, sender: &FallbackSender) {
  let mut interval = tokio::time::interval(LINK_CAPACITY_INTERVAL);
  loop {
    interval.tick().await;
    session.set_link_capacity(sender.pacing_rate_bps());
    // Video the queue dropped can't be shown until a keyframe (#1845).
    for stream_id in sender.take_keyframe_needs() {
      session.request_keyframe(MediaStreamId(stream_id.0));
    }
  }
}

async fn read_frames(
  session: &ParticipantSession,
  source: &mut Source,
  sender: &FallbackSender,
  idle_timeout: Duration,
) -> ReadEnd {
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
      // tungstenite answers pings by itself. These prove the path alive.
      Message::Ping(_) => {},
      // The answer to one of the writer's: it measures the path for the pacer.
      Message::Pong(payload) => sender.on_pong(&payload),
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

/// Reads the join token the client offered with the upgrade, and checks it. `Err` is the status to
/// answer with: a token that isn't one, or that isn't valid, gets no upgrade; nor does a client
/// without one if `requires_token`. A token is checked here only for what it says: the `join` that
/// follows still goes through [`crate::server::admit`], with the same token.
fn check_upgrade_token(
  request: &Request,
  verifier: &dyn JoinTokenVerifier,
  requires_token: bool,
) -> Result<UpgradeToken, StatusCode> {
  let mut lines = Vec::new();
  for value in request.headers().get_all(SEC_WEBSOCKET_PROTOCOL) {
    lines.push(value.to_str().map_err(|_| StatusCode::BAD_REQUEST)?);
  }
  let offered = parse_offered_protocols(lines).map_err(|_| StatusCode::BAD_REQUEST)?;
  let maybe_checked = match offered.maybe_token {
    Some(token) => match verifier.verify(&token) {
      Ok(grant) => Some((token, grant)),
      Err(error) => {
        debug!(%error, "fallback upgrade with an invalid join token");
        return Err(StatusCode::UNAUTHORIZED);
      },
    },
    None if requires_token => return Err(StatusCode::UNAUTHORIZED),
    None => None,
  };
  Ok(UpgradeToken {
    answers_protocol: offered.knows_call_protocol,
    maybe_checked,
  })
}

fn is_join_with(message: &CallClientMessage, upgrade_token: &str) -> bool {
  matches!(message, CallClientMessage::Join { token, .. } if token == upgrade_token)
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
  let socket = SockRef::from(stream);
  if let Err(error) = socket.set_send_buffer_size(config.socket_send_buffer_bytes) {
    debug!(%error, "could not size a fallback connection's send buffer");
  }
  #[cfg(any(target_os = "android", target_os = "linux"))]
  if let Err(error) = socket.set_tcp_notsent_lowat(NOT_SENT_BYTES) {
    debug!(%error, "could not bound what a fallback connection holds unsent");
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

/// The last entry of the last `X-Forwarded-For` line: what the proxy in front of us added itself,
/// as a new line or at the end of the one it got. Whatever a client wrote comes before it.
fn forwarded_for(request: &Request) -> Option<IpAddr> {
  let value = request
    .headers()
    .get_all(FORWARDED_FOR)
    .iter()
    .next_back()?
    .to_str()
    .ok()?;
  let last = value.rsplit(',').next()?.trim();
  last.parse::<IpAddr>().ok().map(|ip| ip.to_canonical())
}

fn not_found() -> ErrorResponse {
  status_response(StatusCode::NOT_FOUND)
}

fn status_response(status: StatusCode) -> ErrorResponse {
  let mut response = ErrorResponse::new(None);
  *response.status_mut() = status;
  response
}

/// Warns when the listener is on every address while only a proxy on this machine is trusted
/// ([`FallbackConfig::listens_wider_than_its_proxies`]): the container image's `[::]:8082` run with
/// host networking, or behind a proxy in another container that isn't listed (#1226).
pub(crate) fn warn_if_listening_wider_than_its_proxies(config: &FallbackConfig, addr: SocketAddr) {
  if config.listens_wider_than_its_proxies(addr) {
    warn!(
      %addr,
      "the call fallback listens on every address but trusts only a proxy on this machine: with \
       host networking set TEAMCRAFT_SFU_FALLBACK_BIND to a loopback address (it is plain HTTP for \
       the proxy alone), behind a proxy in another container list it in \
       TEAMCRAFT_SFU_FALLBACK_TRUSTED_PROXIES"
    );
  }
}

/// Warns when the process may run out of descriptors before the fallback hits its limits. Running
/// out would also break what QUIC calls need: the roster reports and the health endpoint.
pub(crate) fn warn_if_descriptors_are_short(config: &FallbackConfig) {
  let wanted = config.max_connections + config.max_unjoined_connections + OTHER_DESCRIPTORS;
  #[cfg(unix)]
  match nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_NOFILE) {
    Ok((soft, _)) if u64::try_from(wanted).is_ok_and(|wanted| wanted > soft) => warn!(
      wanted,
      limit = soft,
      "the descriptor limit is below what the call fallback may use, which could also affect QUIC \
       calls: raise RLIMIT_NOFILE (LimitNOFILE, ulimit -n) or lower \
       TEAMCRAFT_SFU_FALLBACK_MAX_CONNECTIONS"
    ),
    Ok(_) => {},
    Err(error) => debug!(%error, "could not read the descriptor limit"),
  }
  #[cfg(not(unix))]
  let _ = wanted;
}

impl RefusalLog {
  /// Counts one refusal; logs it and those since the last line, if that was long enough ago.
  fn note(&mut self, reason: &str, now: Instant) -> bool {
    self.unlogged += 1;
    let is_due = self
      .maybe_logged_at
      .is_none_or(|logged_at| now.saturating_duration_since(logged_at) >= REFUSAL_LOG_INTERVAL);
    if !is_due {
      return false;
    }
    warn!(
      count = self.unlogged,
      reason, "refused connections to the call fallback"
    );
    self.maybe_logged_at = Some(now);
    self.unlogged = 0;
    true
  }
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
      let message = match self.maybe_queue.as_mut()?.next().await? {
        Dequeued::Message(message) => *message,
        Dequeued::HandOver => {
          // Everything before the move went out: the queue goes on on the new connection, and
          // the client reads this one up to the close.
          if let Some(queue) = self.maybe_queue.take()
            && !queue.hand_over()
          {
            debug!("the connection the session moved to went first");
          }
          self.has_moved.store(true, Ordering::Release);
          self.sender.close(SFU_SWITCHED_CLOSE_CODE, b"switched");
          return None;
        },
      };
      match serde_json::to_vec(&message) {
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

  mod refusal_log {
    use super::*;

    #[test]
    fn a_flood_of_refusals_is_one_line_per_interval() {
      let start = Instant::now();
      let mut log = RefusalLog::default();
      assert!(log.note("x", start));
      for millis in 1..1000 {
        assert!(!log.note("x", start + Duration::from_millis(millis)));
      }
      assert_eq!(log.unlogged, 999);
      assert!(log.note("x", start + REFUSAL_LOG_INTERVAL));
      assert_eq!(log.unlogged, 0);
    }
  }

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

    #[test]
    fn of_several_lines_the_last_counts_as_the_proxy_added_it() {
      let request = HttpRequest::builder()
        .header("x-forwarded-for", "198.51.100.7")
        .header("x-forwarded-for", "203.0.113.9")
        .body(())
        .unwrap();
      assert_eq!(forwarded_for(&request), "203.0.113.9".parse().ok());
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

  mod upgrade_tokens {
    use teamcraft_proto::fallback_protocols;

    use super::*;
    use crate::auth::DevTokenVerifier;

    #[test]
    fn a_valid_token_is_checked_and_the_call_protocol_answered() {
      let line = fallback_protocols("dev:1:2:3").join(", ");
      let checked = check_upgrade_token(&request_with(Some(&line)), &DevTokenVerifier, false).unwrap();
      assert!(checked.answers_protocol);
      let (token, grant) = checked.maybe_checked.unwrap();
      assert_eq!(token, "dev:1:2:3");
      assert_eq!(grant.device_id.to_string(), "3");
    }

    #[test]
    fn a_client_of_before_gets_in_unless_a_token_is_required() {
      let unchecked = check_upgrade_token(&request_with(None), &DevTokenVerifier, false).unwrap();
      assert_eq!(
        unchecked,
        UpgradeToken {
          answers_protocol: false,
          maybe_checked: None
        }
      );
      assert_eq!(
        check_upgrade_token(&request_with(None), &DevTokenVerifier, true),
        Err(StatusCode::UNAUTHORIZED)
      );
    }

    #[test]
    fn a_broken_or_invalid_token_gets_no_upgrade() {
      assert_eq!(
        check_upgrade_token(
          &request_with(Some("teamcraft-call, teamcraft-join.%%")),
          &DevTokenVerifier,
          false
        ),
        Err(StatusCode::BAD_REQUEST)
      );
      let invalid = fallback_protocols("not a token").join(", ");
      assert_eq!(
        check_upgrade_token(&request_with(Some(&invalid)), &DevTokenVerifier, false),
        Err(StatusCode::UNAUTHORIZED)
      );
    }

    fn request_with(maybe_protocols: Option<&str>) -> Request {
      let mut builder = HttpRequest::builder();
      if let Some(protocols) = maybe_protocols {
        builder = builder.header("sec-websocket-protocol", protocols);
      }
      builder.body(()).unwrap()
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
