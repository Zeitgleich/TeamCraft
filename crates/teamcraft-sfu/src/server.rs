//! WebTransport endpoint and per-session handling.
//!
//! Session flow: the client connects to `/call`, opens one bidirectional stream for signaling and
//! sends [`ClientMessage::Join`] with its token. Afterwards media flows as QUIC datagrams.

use std::{net::SocketAddr, sync::Arc, time::Instant};

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
  room::{ParticipantHandle, Room, RoomError, Rooms},
  signaling::{
    ClientMessage, ServerMessage, SignalingError, SignalingErrorCode, read_message, write_message,
  },
};

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
}

/// One joined participant's session.
struct ParticipantSession {
  connection: Connection,
  room: Arc<Room>,
  grant: JoinGrant,
  signal_tx: mpsc::UnboundedSender<ServerMessage>,
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
      }),
    })
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
    read_message::<ClientMessage>(&mut recv, max_bytes),
  )
  .await
  .map_err(|_| SessionError::JoinTimeout)??;
  let ClientMessage::Join { token } = first else {
    reject(&mut send, SignalingErrorCode::ProtocolViolation, "expected join").await;
    return Err(SessionError::ExpectedJoin);
  };
  let grant = match shared.verifier.verify(&token) {
    Ok(grant) => grant,
    Err(error) => {
      reject(&mut send, SignalingErrorCode::InvalidToken, &error.to_string()).await;
      return Ok(());
    },
  };

  let (signal_tx, signal_rx) = mpsc::unbounded_channel();
  let handle = ParticipantHandle {
    connection: connection.clone(),
    signal_tx: signal_tx.clone(),
  };
  let room = match shared.rooms.join(grant.call_id, grant.participant_id, handle) {
    Ok(room) => room,
    Err(error) => {
      reject(&mut send, SignalingErrorCode::AlreadyJoined, &error.to_string()).await;
      return Err(error.into());
    },
  };

  let session = ParticipantSession {
    connection,
    room,
    grant,
    signal_tx,
    max_bytes,
  };
  let result = session.run(send, recv, signal_rx).await;
  shared.rooms.leave(&session.room, grant.participant_id);
  result
}

impl ParticipantSession {
  /// Runs until the client disconnects or violates the protocol.
  async fn run(
    &self,
    send: SendStream,
    mut recv: RecvStream,
    signal_rx: mpsc::UnboundedReceiver<ServerMessage>,
  ) -> Result<(), SessionError> {
    let writer = tokio::spawn(write_signaling(send, signal_rx));
    let signaling = async {
      loop {
        let message = read_message::<ClientMessage>(&mut recv, self.max_bytes).await?;
        self.handle_client_message(message);
      }
    };
    let media = async {
      loop {
        let datagram = self.connection.receive_datagram().await?;
        if let Err(error) =
          self
            .room
            .forward_datagram(self.grant.participant_id, &datagram.payload(), Instant::now())
        {
          debug!(participant_id = %self.grant.participant_id, %error, "dropping invalid datagram");
        }
      }
    };
    let result: Result<(), SessionError> = tokio::select! {
      result = signaling => result,
      result = media => result,
    };
    writer.abort();
    info!(
      call_id = self.grant.call_id.0,
      participant_id = %self.grant.participant_id,
      "session closed"
    );
    result
  }

  fn handle_client_message(&self, message: ClientMessage) {
    let participant_id = self.grant.participant_id;
    match message {
      ClientMessage::DaveKeyPackage { key_package } => {
        if let Err(error) = self.room.on_key_package(participant_id, key_package) {
          warn!(participant_id = %participant_id, %error, "rejected key package");
          self.send_error(SignalingErrorCode::InvalidKeyPackage, error.to_string());
        }
      },
      ClientMessage::DaveCommit {
        epoch,
        commit,
        maybe_welcome,
      } => {
        self.room.on_commit(participant_id, epoch, commit, maybe_welcome);
      },
      ClientMessage::Join { .. } => {
        self.send_error(SignalingErrorCode::ProtocolViolation, "already joined".to_owned())
      },
      ClientMessage::Unknown => debug!(participant_id = %participant_id, "ignoring unknown message"),
    }
  }

  fn send_error(&self, error_code: SignalingErrorCode, message: String) {
    // Fails only when the writer task ended, i.e. the session is closing anyway.
    let _ = self.signal_tx.send(ServerMessage::Error { error_code, message });
  }
}

async fn write_signaling(mut send: SendStream, mut signal_rx: mpsc::UnboundedReceiver<ServerMessage>) {
  while let Some(message) = signal_rx.recv().await {
    if let Err(error) = write_message(&mut send, &message).await {
      debug!(%error, "signaling write failed");
      return;
    }
  }
}

async fn reject(send: &mut SendStream, error_code: SignalingErrorCode, message: &str) {
  let error = ServerMessage::Error {
    error_code,
    message: message.to_owned(),
  };
  // Best effort: the session is being closed anyway.
  if write_message(send, &error).await.is_ok() {
    let _ = send.finish().await;
  }
}
