//! WebTransport endpoint and per-session handling.
//!
//! Session flow: the client connects to `/call`, opens one bidirectional stream for signaling and
//! sends [`CallClientMessage::Join`] with its token. Afterwards media flows as QUIC datagrams.

use std::{
  net::SocketAddr,
  sync::Arc,
  time::{Duration, Instant},
};

use teamcraft_proto::{Base64Bytes, CallClientMessage, CallEvent, CallServerMessage, ErrorBody, ErrorCode};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{Instrument, debug, info, info_span, warn};
use wtransport::{
  Connection, Endpoint, Identity, RecvStream, SendStream, ServerConfig,
  config::Ipv6DualStackConfig,
  endpoint::{IncomingSession, endpoint_side::Server},
  tls::Sha256Digest,
};

use crate::{
  auth::{JoinGrant, JoinTokenVerifier},
  config::SfuConfig,
  reporting::{NoReporter, ParticipantReporter},
  room::{ParticipantHandle, Room, RoomError, Rooms},
  signaling::{EventSequence, Outgoing, SignalingError, read_message, write_message},
};

/// How often senders get feedback on their datagrams (uplink bandwidth estimation) and layer
/// allocations are re-run.
const PERIODIC_INTERVAL: Duration = Duration::from_millis(100);

/// HTTP path of the WebTransport session.
pub const CALL_PATH: &str = "/call";

/// A bound SFU, ready to [`Sfu::run`].
pub struct Sfu {
  endpoint: Endpoint<Server>,
  certificate_hash: Sha256Digest,
  shared: Arc<Shared>,
}

struct Shared {
  config: SfuConfig,
  rooms: Rooms,
  verifier: Arc<dyn JoinTokenVerifier>,
  reporter: Arc<dyn ParticipantReporter>,
}

/// One joined participant's session.
struct ParticipantSession {
  connection: Connection,
  room: Arc<Room>,
  grant: JoinGrant,
  signal_tx: mpsc::UnboundedSender<Outgoing>,
  max_bytes: usize,
}

#[derive(Debug, Error)]
pub enum SfuError {
  #[error("binding the endpoint failed: {0}")]
  Bind(#[source] std::io::Error),
  #[error("the TLS identity has no certificate")]
  NoCertificate,
}

#[derive(Debug, Error)]
enum SessionError {
  #[error("connection failed: {0}")]
  Connection(#[from] wtransport::error::ConnectionError),
  #[error("signaling failed: {0}")]
  Signaling(#[from] SignalingError),
  #[error("client did not join in time")]
  JoinTimeout,
  #[error("first message was not a join")]
  ExpectedJoin,
  #[error(transparent)]
  Room(#[from] RoomError),
}

impl Sfu {
  pub fn bind(
    config: SfuConfig,
    identity: Identity,
    verifier: Arc<dyn JoinTokenVerifier>,
  ) -> Result<Self, SfuError> {
    let certificate_hash = identity
      .certificate_chain()
      .as_slice()
      .first()
      .ok_or(SfuError::NoCertificate)?
      .hash();
    let builder = ServerConfig::builder();
    let builder = match config.bind_addr {
      SocketAddr::V6(addr) => builder.with_bind_address_v6(addr, Ipv6DualStackConfig::Allow),
      SocketAddr::V4(_) => builder.with_bind_address(config.bind_addr),
    };
    let server_config = builder
      .with_identity(identity)
      .keep_alive_interval(Some(std::time::Duration::from_secs(3)))
      .build();
    let endpoint = Endpoint::server(server_config).map_err(SfuError::Bind)?;
    let rooms = Rooms::new(config.clone());
    Ok(Self {
      endpoint,
      certificate_hash,
      shared: Arc::new(Shared {
        config,
        rooms,
        verifier,
        reporter: Arc::new(NoReporter),
      }),
    })
  }

  /// Reports joins and leaves to `teamcraft-server` (call start, ringing, roster). Call before
  /// [`Self::run`].
  pub fn with_reporter(mut self, reporter: Arc<dyn ParticipantReporter>) -> Self {
    if let Some(shared) = Arc::get_mut(&mut self.shared) {
      shared.reporter = reporter;
    }
    self
  }

  pub fn local_addr(&self) -> Result<SocketAddr, SfuError> {
    self.endpoint.local_addr().map_err(SfuError::Bind)
  }

  /// SHA-256 of the leaf certificate, for `serverCertificateHashes` in browsers.
  pub fn certificate_hash(&self) -> Sha256Digest {
    self.certificate_hash.clone()
  }

  /// Accepts sessions until the task is dropped.
  pub async fn run(self) {
    let mut session_number: u64 = 0;
    loop {
      session_number = session_number.wrapping_add(1);
      let incoming = self.endpoint.accept().await;
      let shared = Arc::clone(&self.shared);
      tokio::spawn(
        async move {
          if let Err(error) = handle_session(incoming, &shared).await {
            debug!(%error, "session ended");
          }
        }
        .instrument(info_span!("session", session_number)),
      );
    }
  }
}

async fn handle_session(incoming: IncomingSession, shared: &Shared) -> Result<(), SessionError> {
  let request = incoming.await?;
  if request.path() != CALL_PATH {
    request.not_found().await;
    return Ok(());
  }
  let connection = request.accept().await?;
  // Dual-stack sockets report IPv4 peers as IPv4-mapped IPv6; log them in their canonical form.
  debug!(remote_ip = %connection.remote_address().ip().to_canonical(), "session accepted");
  let (mut send, mut recv) = tokio::time::timeout(shared.config.join_timeout, connection.accept_bi())
    .await
    .map_err(|_| SessionError::JoinTimeout)??;

  let max_bytes = shared.config.max_signaling_message_bytes;
  let first = tokio::time::timeout(
    shared.config.join_timeout,
    read_message::<CallClientMessage>(&mut recv, max_bytes),
  )
  .await
  .map_err(|_| SessionError::JoinTimeout)??;
  let CallClientMessage::Join { token, capabilities } = first else {
    reject(&mut send, ErrorCode::ProtocolViolation, "expected join").await;
    return Err(SessionError::ExpectedJoin);
  };
  let grant = match shared.verifier.verify(&token) {
    Ok(grant) => grant,
    Err(error) => {
      reject(&mut send, ErrorCode::InvalidJoinToken, &error.to_string()).await;
      return Ok(());
    },
  };

  let (signal_tx, signal_rx) = mpsc::unbounded_channel();
  let handle = ParticipantHandle {
    connection: connection.clone(),
    signal_tx: signal_tx.clone(),
  };
  let room = match shared.rooms.join(grant, capabilities, handle) {
    Ok(room) => room,
    Err(error) => {
      reject(&mut send, ErrorCode::AlreadyJoined, &error.to_string()).await;
      return Err(error.into());
    },
  };

  shared.reporter.joined(grant);
  let session = ParticipantSession {
    connection,
    room,
    grant,
    signal_tx,
    max_bytes,
  };
  let result = session.run(send, recv, signal_rx).await;
  shared.rooms.leave(&session.room, grant.device_id);
  shared.reporter.left(grant);
  result
}

impl ParticipantSession {
  /// Runs until the client disconnects or violates the protocol.
  async fn run(
    &self,
    send: SendStream,
    mut recv: RecvStream,
    signal_rx: mpsc::UnboundedReceiver<Outgoing>,
  ) -> Result<(), SessionError> {
    let writer = tokio::spawn(write_signaling(send, signal_rx));
    let signaling = async {
      loop {
        let message = read_message::<CallClientMessage>(&mut recv, self.max_bytes).await?;
        self.handle_client_message(message);
      }
    };
    let media = async {
      loop {
        let datagram = self.connection.receive_datagram().await?;
        if let Err(error) =
          self
            .room
            .forward_datagram(self.grant.device_id, &datagram.payload(), Instant::now())
        {
          debug!(participant_id = %self.grant.device_id, %error, "dropping invalid datagram");
        }
      }
    };
    let periodic = async {
      let mut interval = tokio::time::interval(PERIODIC_INTERVAL);
      loop {
        interval.tick().await;
        self.room.on_timer(self.grant.device_id, Instant::now());
        if let Some(feedback) = self.room.take_uplink_feedback(self.grant.device_id) {
          // Fails only when the writer task ended, i.e. the session is closing anyway.
          let _ = self
            .signal_tx
            .send(Outgoing::Event(CallEvent::TransportFeedback { feedback }));
        }
      }
    };
    let result: Result<(), SessionError> = tokio::select! {
      result = signaling => result,
      result = media => result,
      () = periodic => Ok(()),
    };
    writer.abort();
    info!(
      call_id = %self.grant.call_id,
      participant_id = %self.grant.device_id,
      "session closed"
    );
    result
  }

  fn handle_client_message(&self, message: CallClientMessage) {
    let participant_id = self.grant.device_id;
    match message {
      CallClientMessage::DaveKeyPackage {
        key_package,
        maybe_key_binding,
      } => {
        let result = self.room.on_key_package(
          participant_id,
          key_package.into_vec(),
          maybe_key_binding.map(Base64Bytes::into_vec),
        );
        if let Err(error) = result {
          warn!(participant_id = %participant_id, %error, "rejected key package");
          self.send_error(ErrorCode::InvalidKeyPackage, error.to_string());
        }
      },
      CallClientMessage::DaveCommit {
        epoch,
        commit,
        maybe_welcome,
      } => {
        self.room.on_commit(
          participant_id,
          epoch,
          commit.into_vec(),
          maybe_welcome.map(Base64Bytes::into_vec),
        );
      },
      CallClientMessage::Join { .. } => {
        self.send_error(ErrorCode::ProtocolViolation, "already joined".to_owned())
      },
      CallClientMessage::TransportFeedback { feedback } => {
        self
          .room
          .on_transport_feedback(participant_id, &feedback, Instant::now());
      },
      CallClientMessage::PublishVideo { kind, codec, layers } => {
        let result = self.room.publish_video(participant_id, kind, codec, layers);
        self.report(result);
      },
      CallClientMessage::UnpublishVideo { stream_id } => {
        let result = self.room.unpublish_video(participant_id, stream_id);
        self.report(result);
      },
      CallClientMessage::Subscribe {
        stream_id,
        maybe_max_layer,
      } => {
        let result = self
          .room
          .subscribe(participant_id, stream_id, maybe_max_layer, Instant::now());
        self.report(result);
      },
      CallClientMessage::Unknown => debug!(participant_id = %participant_id, "ignoring unknown message"),
    }
  }

  /// Turns a rejected request into a `protocol_violation` error for the client.
  fn report(&self, result: Result<(), RoomError>) {
    if let Err(error) = result {
      debug!(participant_id = %self.grant.device_id, %error, "request rejected");
      self.send_error(ErrorCode::ProtocolViolation, error.to_string());
    }
  }

  fn send_error(&self, error_code: ErrorCode, message: String) {
    // Fails only when the writer task ended, i.e. the session is closing anyway.
    let _ = self
      .signal_tx
      .send(Outgoing::Error(ErrorBody::new(error_code, message)));
  }
}

async fn write_signaling(mut send: SendStream, mut signal_rx: mpsc::UnboundedReceiver<Outgoing>) {
  let mut sequence = EventSequence::default();
  while let Some(outgoing) = signal_rx.recv().await {
    if let Err(error) = write_message(&mut send, &sequence.frame(outgoing)).await {
      debug!(%error, "signaling write failed");
      return;
    }
  }
}

async fn reject(send: &mut SendStream, error_code: ErrorCode, message: &str) {
  let error = CallServerMessage::Error {
    error: ErrorBody::new(error_code, message),
  };
  // Best effort: the session is being closed anyway.
  if write_message(send, &error).await.is_ok() {
    let _ = send.finish().await;
  }
}
