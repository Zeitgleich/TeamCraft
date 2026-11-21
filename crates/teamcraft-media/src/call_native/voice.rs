//! The call's own audio on a QUIC connection of its own (#2141), marked DSCP Expedited Forwarding.
//!
//! A home router's Wi-Fi (WMM) and many office switches serve EF in their voice queue (#1989), but
//! only a connection that carries nothing but voice may be marked so (RFC 8837), and quinn marks
//! nothing per datagram: one QUIC packet may bundle the voice with video. So once the SFU offers it
//! ([`CallServerMessage::AudioSocket`], after every join and switch), the client opens a second
//! connection to the same SFU address on a socket that marks everything it sends
//! ([`crate::marked_udp_socket`]), names the session's secret first (`attach_audio`), and sends the
//! own audio datagrams there. Everything else stays on the session's connection, and so does the
//! audio while the voice connection isn't up.
//!
//! Its life follows the session's connection: a network change moves it to a fresh socket too
//! (QUIC migration, as the session's); when it breaks it reconnects with backoff, the audio on the
//! session's connection meanwhile; a close by the SFU (the session ended, a refused or replaced
//! attach, the SFU going away) ends it for good, and the session's next connection brings a new
//! secret. It closes with the link.
//!
//! Unix only, where the socket can mark (see `marked_udp_socket`): elsewhere the offer is ignored.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_net::retry::{Jitter, RetryPolicy};
use teamcraft_net::traffic::TrafficClass;
use teamcraft_proto::{Base64Bytes, CallClientMessage};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracing::debug;
use wtransport::endpoint::endpoint_side::Client;
use wtransport::{Connection, Endpoint};

use super::{LEAVE, LinkEnd, NativeCallError, bind_wildcard, connect_to, link_end, write_framed};
use crate::call_resilience::ResilienceConfig;

/// Whether this platform can mark a QUIC connection's datagrams (see `marked_udp_socket`).
const MARKS_QUIC_VOICE: bool = cfg!(unix);
/// A voice connection that lasted this long counts as working: the next break starts the backoff
/// over.
const STABLE_AFTER: Duration = Duration::from_secs(30);

/// A session's voice connection, from the SFU's offer until the link closes.
pub(super) struct VoiceLink {
  target: VoiceTarget,
  maybe_attached: Mutex<Option<AttachedVoice>>,
}

/// Where the voice connection goes: the session connection's SFU address, so both meet the same
/// SFU (a host name may stand for several).
#[derive(Clone)]
struct VoiceTarget {
  sfu_url: String,
  address: SocketAddr,
  certificate_hashes: Vec<[u8; 32]>,
  resilience: ResilienceConfig,
}

/// A voice connection that connects, runs or waits to reconnect.
struct AttachedVoice {
  task: JoinHandle<()>,
  shared: Arc<VoiceShared>,
}

#[derive(Default)]
struct VoiceShared {
  /// The connection while it carries the voice.
  maybe_up: Mutex<Option<UpVoice>>,
  /// Cuts a reconnect's wait short: a network came back.
  retry_now: Notify,
}

struct UpVoice {
  /// The connection's own endpoint, for moving it to a new socket.
  endpoint: Endpoint<Client>,
  connection: Connection,
}

impl VoiceLink {
  pub(super) fn new(
    sfu_url: &str,
    address: SocketAddr,
    certificate_hashes: &[[u8; 32]],
    resilience: ResilienceConfig,
  ) -> Self {
    Self {
      target: VoiceTarget {
        sfu_url: sfu_url.to_owned(),
        address,
        certificate_hashes: certificate_hashes.to_vec(),
        resilience,
      },
      maybe_attached: Mutex::new(None),
    }
  }

  /// Opens the voice connection for the session's `secret`, in place of any earlier one. Runs in
  /// the background; nothing waits for it.
  pub(super) fn attach(&self, secret: Base64Bytes) {
    if !MARKS_QUIC_VOICE {
      debug!("no marked voice connection on this platform: the voice stays on the session's");
      return;
    }
    self.close();
    let shared = Arc::new(VoiceShared::default());
    let task = tokio::spawn(run_voice(self.target.clone(), secret, Arc::clone(&shared)));
    *lock(&self.maybe_attached) = Some(AttachedVoice { task, shared });
  }

  /// Sends an audio datagram on the voice connection; `false` when it isn't up (or refuses this
  /// datagram), and the session's connection should take it.
  pub(super) fn try_send(&self, datagram: &[u8]) -> bool {
    let attached = lock(&self.maybe_attached);
    let Some(attached) = attached.as_ref() else {
      return false;
    };
    let up = lock(&attached.shared.maybe_up);
    up.as_ref()
      .is_some_and(|up| up.connection.send_datagram(datagram).is_ok())
  }

  /// The network changed: the voice connection moves to a fresh socket of its own, as the
  /// session's does; one that waits to reconnect tries at once.
  pub(super) fn rebind(&self) {
    let attached = lock(&self.maybe_attached);
    let Some(attached) = attached.as_ref() else {
      return;
    };
    let up = lock(&attached.shared.maybe_up);
    let Some(up) = up.as_ref() else {
      attached.shared.retry_now.notify_one();
      return;
    };
    let moved = up.endpoint.local_addr().and_then(|current| {
      let socket = bind_wildcard(current.is_ipv6())?;
      up.endpoint.rebind_abstract(voice_socket(socket)?)
    });
    if let Err(error) = moved {
      debug!(%error, "could not move the voice connection to a new socket");
    }
  }

  /// Closes the voice connection, if there is one.
  pub(super) fn close(&self) {
    let Some(attached) = lock(&self.maybe_attached).take() else {
      return;
    };
    attached.task.abort();
    if let Some(up) = lock(&attached.shared.maybe_up).take() {
      up.connection.close(LEAVE.into(), b"leave");
    }
  }

  /// For the tests, which run where the voice connection exists (unix).
  #[cfg(test)]
  #[cfg(unix)]
  fn is_up(&self) -> bool {
    lock(&self.maybe_attached)
      .as_ref()
      .is_some_and(|attached| lock(&attached.shared.maybe_up).is_some())
  }
}

impl Drop for VoiceLink {
  fn drop(&mut self) {
    self.close();
  }
}

impl fmt::Debug for VoiceLink {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("VoiceLink").finish_non_exhaustive()
  }
}

/// A socket for the voice connection: marked Expedited Forwarding where the platform can.
pub(super) fn voice_socket(
  socket: std::net::UdpSocket,
) -> std::io::Result<Arc<dyn wtransport::quinn::AsyncUdpSocket>> {
  #[cfg(unix)]
  {
    Ok(Arc::new(crate::marked_udp_socket::MarkedUdpSocket::new(
      socket,
      TrafficClass::Voice,
    )?))
  }
  #[cfg(not(unix))]
  {
    use wtransport::quinn::Runtime;
    wtransport::quinn::TokioRuntime.wrap_udp_socket(socket)
  }
}

/// Connects, attaches and runs the voice connection; reconnects with backoff after a loss, until a
/// close by the SFU ends it or the link aborts it.
async fn run_voice(target: VoiceTarget, secret: Base64Bytes, shared: Arc<VoiceShared>) {
  let policy = RetryPolicy::default();
  // Spread only, no secret: the process's random hash keys differ per device and call.
  let mut jitter = Jitter::new(RandomState::new().build_hasher().finish());
  let mut failures: u32 = 0;
  loop {
    match connect_voice(&target, &secret).await {
      Ok((endpoint, connection)) => {
        let started = Instant::now();
        *lock(&shared.maybe_up) = Some(UpVoice {
          endpoint,
          connection: connection.clone(),
        });
        debug!("the voice connection is up");
        let error = connection.closed().await;
        *lock(&shared.maybe_up) = None;
        let end = link_end(&error);
        if end != LinkEnd::Lost {
          debug!(
            ?end,
            "the voice connection ended: the audio goes on the session's connection"
          );
          return;
        }
        debug!(%error, "the voice connection broke: the audio goes on the session's connection");
        if started.elapsed() >= STABLE_AFTER {
          failures = 0;
        }
      },
      Err(error) => debug!(%error, "the voice connection could not be opened"),
    }
    let delay = policy.delay(failures, &mut jitter, None);
    failures = failures.saturating_add(1);
    tokio::select! {
      () = tokio::time::sleep(delay) => {},
      () = shared.retry_now.notified() => {},
    }
  }
}

/// A marked connection to the session's SFU address whose first message names the secret.
async fn connect_voice(
  target: &VoiceTarget,
  secret: &Base64Bytes,
) -> Result<(Endpoint<Client>, Connection), NativeCallError> {
  let (endpoint, connection) = connect_to(
    &target.sfu_url,
    target.address,
    &target.certificate_hashes,
    &target.resilience,
    TrafficClass::Voice,
  )
  .await?;
  let (mut send, _recv) = connection
    .open_bi()
    .await
    .map_err(|error| NativeCallError::Connect(error.to_string()))?
    .await
    .map_err(|error| NativeCallError::Connect(error.to_string()))?;
  let attach = CallClientMessage::AttachAudio {
    secret: secret.clone(),
  };
  write_framed(&mut send, &attach)
    .await
    .ok_or_else(|| NativeCallError::Connect("could not name the voice connection's session".into()))?;
  Ok((endpoint, connection))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // Nothing spans a panic point while it is held.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
  use std::net::Ipv4Addr;

  use teamcraft_proto::CallServerMessage;
  use wtransport::endpoint::endpoint_side::Server;
  use wtransport::{Identity, RecvStream, SendStream, ServerConfig};

  use super::super::{Link, LinkPath};
  use super::*;
  use crate::rtp::PayloadType;

  const SECRET: [u8; 32] = [5; 32];
  const WAIT: Duration = Duration::from_secs(10);

  /// The own audio leaves on the voice connection once it is up, video stays on the session's; the
  /// voice connection follows a network change; after the SFU closed it, the audio is back on the
  /// session's connection.
  #[tokio::test]
  async fn the_own_audio_takes_the_voice_connection_and_video_stays() {
    let (server, url, hash) = server();
    // The WebTransport handshake needs the server's accept.
    let (resilience, hashes) = (ResilienceConfig::default(), [hash]);
    let (link, session) = tokio::join!(Link::open(&url, &hashes, &resilience), accept(&server));
    let link = link.unwrap();
    // The session's stream opens with the client's first message; the SFU answers with the offer.
    link.signal_out.send(CallClientMessage::Ping { id: 1 }).unwrap();
    let (mut send, mut recv) = session.accept_bi().await.unwrap();
    assert!(matches!(
      read_client(&mut recv).await,
      CallClientMessage::Ping { id: 1 }
    ));
    let offer = CallServerMessage::AudioSocket {
      secret: Base64Bytes::new(SECRET.to_vec()),
    };
    write_server(&mut send, &offer).await;

    let voice = accept(&server).await;
    let (_voice_send, mut voice_recv) = voice.accept_bi().await.unwrap();
    match read_client(&mut voice_recv).await {
      CallClientMessage::AttachAudio { secret } => assert_eq!(secret.as_slice(), SECRET),
      other => panic!("expected attach_audio, got {other:?}"),
    }
    let LinkPath::Quic {
      voice: voice_link, ..
    } = &link.path
    else {
      panic!("a QUIC link");
    };
    wait_until(|| voice_link.is_up()).await;

    link.send_datagram(&audio());
    link.send_datagram(&video());
    assert_eq!(
      received(&voice).await,
      audio(),
      "the audio took the voice connection"
    );
    assert_eq!(received(&session).await, video(), "video stayed");

    // A network change moves both connections to fresh sockets; the audio keeps its own.
    link.rebind().unwrap();
    link.send_datagram(&audio());
    assert_eq!(received(&voice).await, audio());

    // The SFU ends the voice connection (its session's next connection brings a new secret).
    voice.close(0u32.into(), b"closing");
    wait_until(|| !voice_link.is_up()).await;
    link.send_datagram(&audio());
    assert_eq!(
      received(&session).await,
      audio(),
      "back on the session's connection"
    );
  }

  #[test]
  fn only_audio_counts_as_the_own_voice() {
    assert!(crate::fallback_queue::is_audio(&audio()));
    assert!(!crate::fallback_queue::is_audio(&video()));
  }

  fn server() -> (Endpoint<Server>, String, [u8; 32]) {
    let identity = Identity::self_signed(["localhost"]).unwrap();
    let hash: [u8; 32] = *identity.certificate_chain().as_slice()[0].hash().as_ref();
    let config = ServerConfig::builder()
      .with_bind_address(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
      .with_identity(identity)
      .build();
    let server = Endpoint::server(config).unwrap();
    let port = server.local_addr().unwrap().port();
    (server, format!("https://localhost:{port}/call"), hash)
  }

  async fn accept(server: &Endpoint<Server>) -> Connection {
    let accepting = async { server.accept().await.await.unwrap().accept().await.unwrap() };
    tokio::time::timeout(WAIT, accepting)
      .await
      .expect("a connection in time")
  }

  async fn read_client(recv: &mut RecvStream) -> CallClientMessage {
    let mut length = [0u8; 4];
    recv.read_exact(&mut length).await.unwrap();
    let mut body = vec![0u8; u32::from_be_bytes(length) as usize];
    recv.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
  }

  async fn write_server(send: &mut SendStream, message: &CallServerMessage) {
    let body = serde_json::to_vec(message).unwrap();
    let mut framed = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    framed.extend_from_slice(&body);
    send.write_all(&framed).await.unwrap();
  }

  async fn received(connection: &Connection) -> Vec<u8> {
    let datagram = tokio::time::timeout(WAIT, connection.receive_datagram())
      .await
      .expect("a datagram in time")
      .unwrap();
    datagram.payload().to_vec()
  }

  async fn wait_until(condition: impl Fn() -> bool) {
    let waiting = async {
      while !condition() {
        tokio::time::sleep(Duration::from_millis(5)).await;
      }
    };
    tokio::time::timeout(WAIT, waiting)
      .await
      .expect("the voice connection's state in time");
  }

  fn audio() -> Vec<u8> {
    datagram(PayloadType::OPUS.value(), 100)
  }

  fn video() -> Vec<u8> {
    datagram(100, 1000)
  }

  /// A 5-byte datagram header without a transport sequence number, then an RTP header's start.
  fn datagram(payload_type: u8, length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    bytes[5] = 0x80;
    bytes[6] = payload_type;
    bytes
  }
}
