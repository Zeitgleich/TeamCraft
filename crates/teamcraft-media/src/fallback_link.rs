//! The native clients' side of a call's WebSocket fallback (ARCHITECTURE.md §6.6): opening the
//! socket and reading from it. The call driver sees a link like its QUIC one.

use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{Sink, Stream, StreamExt};
use teamcraft_proto::{CallClientMessage, CallServerMessage, FallbackFrame, SFU_GOING_AWAY_CLOSE_CODE};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Bytes, Error as WebSocketError, Message};
use tracing::{debug, warn};

use crate::fallback_socket::ControlFrames;

/// How a call socket is opened: plain, only to a loopback address, in development tools and tests.
/// The apps bring their own, with their trust store and the system's proxy.
pub trait FallbackOpener: fmt::Debug + Send + Sync {
  fn open(&self, url: &str) -> Pin<Box<dyn Future<Output = Result<FallbackSocket, String>> + Send>>;
}

/// An open WebSocket, already upgraded.
pub struct FallbackSocket {
  pub sink: Pin<Box<dyn Sink<Message, Error = WebSocketError> + Send>>,
  pub stream: Pin<Box<dyn Stream<Item = Result<Message, WebSocketError>> + Send>>,
}

impl FallbackSocket {
  /// From an open WebSocket, whatever stream it runs on.
  pub fn new<S>(socket: S) -> Self
  where
    S:
      Sink<Message, Error = WebSocketError> + Stream<Item = Result<Message, WebSocketError>> + Send + 'static,
  {
    let (sink, stream) = socket.split();
    Self {
      sink: Box::pin(sink),
      stream: Box::pin(stream),
    }
  }
}

/// [`FallbackOpener`] without TLS, for loopback addresses.
#[derive(Debug)]
pub(crate) struct LoopbackOpener;

/// How a call socket ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SocketEnd {
  /// The SFU closed it on purpose with this code.
  Closed(u32),
  /// It broke, or closed without saying why: a proxy restarted, a connection reset.
  Lost,
}

/// What a call socket delivers.
pub(crate) enum SocketInput {
  Signal(CallServerMessage),
  Datagram(Bytes),
}

/// Signaling messages from the call task as frames for the writer.
pub(crate) struct SignalFrames(pub(crate) mpsc::UnboundedReceiver<CallClientMessage>);

impl ControlFrames for SignalFrames {
  async fn next_frame(&mut self) -> Option<Vec<u8>> {
    loop {
      let message = self.0.recv().await?;
      match serde_json::to_vec(&message) {
        Ok(body) => return Some(FallbackFrame::Signal(&body).encode()),
        Err(error) => warn!(%error, "could not encode a signaling message"),
      }
    }
  }
}

impl FallbackOpener for LoopbackOpener {
  fn open(&self, url: &str) -> Pin<Box<dyn Future<Output = Result<FallbackSocket, String>> + Send>> {
    let url = url.to_owned();
    Box::pin(async move {
      if !url.starts_with("ws://") {
        return Err("no way to open a TLS call socket here".to_owned());
      }
      let addresses = crate::sfu_address::sfu_addresses(&url)
        .await
        .map_err(|error| error.to_string())?;
      let stream = TcpStream::connect(addresses.as_slice())
        .await
        .map_err(|error| error.to_string())?;
      let (socket, _) = tokio_tungstenite::client_async(url.as_str(), stream)
        .await
        .map_err(|error| error.to_string())?;
      Ok(FallbackSocket::new(socket))
    })
  }
}

/// A call socket carries the join token in the clear unless it is TLS: `wss://`, or `ws://` to the
/// machine itself (development, a test). A server can't talk a client out of this.
pub(crate) fn is_allowed_url(url: &str) -> bool {
  if url.starts_with("wss://") {
    return true;
  }
  let Some(rest) = url.strip_prefix("ws://") else {
    return false;
  };
  let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
  let host = match authority.strip_prefix('[') {
    Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
    None => authority.split(':').next().unwrap_or_default(),
  };
  host == "localhost"
    || host
      .parse::<IpAddr>()
      .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// Reads until the socket ends. Every frame counts for the path's liveness, pings included (the
/// SFU's come every second): the same evidence QUIC's packet count gives.
pub(crate) async fn read_socket(
  mut stream: Pin<Box<dyn Stream<Item = Result<Message, WebSocketError>> + Send>>,
  received: Arc<AtomicU64>,
  mut deliver: impl FnMut(SocketInput) -> bool + Send,
) -> SocketEnd {
  while let Some(item) = stream.next().await {
    let message = match item {
      Ok(message) => message,
      Err(error) => {
        debug!(%error, "call socket read failed");
        return SocketEnd::Lost;
      },
    };
    received.fetch_add(1, Ordering::Relaxed);
    let bytes = match message {
      Message::Binary(bytes) => bytes,
      // The WebSocket's own close: the frame with the reason would have come first.
      Message::Close(_) => return SocketEnd::Lost,
      Message::Ping(_) | Message::Pong(_) | Message::Text(_) | Message::Frame(_) => continue,
    };
    match decode(bytes) {
      Ok(Decoded::Input(input)) => {
        if !deliver(input) {
          return SocketEnd::Lost;
        }
      },
      Ok(Decoded::Closed(code)) => return SocketEnd::Closed(code),
      Ok(Decoded::Nothing) => {},
      Err(error) => warn!(%error, "invalid frame on the call socket"),
    }
  }
  SocketEnd::Lost
}

enum Decoded {
  Input(SocketInput),
  Closed(u32),
  Nothing,
}

fn decode(bytes: Bytes) -> Result<Decoded, teamcraft_proto::FallbackFrameError> {
  Ok(match FallbackFrame::decode(&bytes)? {
    FallbackFrame::Signal(body) => match serde_json::from_slice(body) {
      Ok(message) => Decoded::Input(SocketInput::Signal(message)),
      Err(error) => {
        warn!(%error, "invalid signaling message");
        Decoded::Nothing
      },
    },
    // Without its kind byte, without copying.
    FallbackFrame::Datagram(_) => Decoded::Input(SocketInput::Datagram(bytes.slice(1..))),
    FallbackFrame::Close { code, .. } => Decoded::Closed(code),
    FallbackFrame::Unknown(kind) => {
      debug!(kind, "ignoring a frame of a newer SFU");
      Decoded::Nothing
    },
  })
}

/// Whether a close with this code asks the client to rejoin rather than end the call.
pub(crate) fn is_going_away(code: u32) -> bool {
  code == SFU_GOING_AWAY_CLOSE_CODE
}

/// A [`FallbackOpener`] from a function, for the apps: their HTTP stack opens the socket.
pub fn fallback_opener<F, Opened>(open: F) -> Arc<dyn FallbackOpener>
where
  F: Fn(String) -> Opened + Send + Sync + 'static,
  Opened: Future<Output = Result<FallbackSocket, String>> + Send + 'static,
{
  Arc::new(FnOpener(open))
}

struct FnOpener<F>(F);

impl<F> fmt::Debug for FnOpener<F> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("FnOpener")
  }
}

impl<F, Opened> FallbackOpener for FnOpener<F>
where
  F: Fn(String) -> Opened + Send + Sync,
  Opened: Future<Output = Result<FallbackSocket, String>> + Send + 'static,
{
  fn open(&self, url: &str) -> Pin<Box<dyn Future<Output = Result<FallbackSocket, String>> + Send>> {
    Box::pin((self.0)(url.to_owned()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod urls {
    use super::*;

    #[test]
    fn tls_is_always_allowed() {
      assert!(is_allowed_url("wss://sfu.example.com/call/fallback"));
      assert!(is_allowed_url("wss://[2001:db8::1]:8443/call/fallback"));
    }

    #[test]
    fn plain_only_reaches_the_machine_itself() {
      assert!(is_allowed_url("ws://localhost:8082/call/fallback"));
      assert!(is_allowed_url("ws://127.0.0.1:8082/call/fallback"));
      assert!(is_allowed_url("ws://[::1]:8082/call/fallback"));
      assert!(is_allowed_url("ws://[::ffff:127.0.0.1]:8082/call/fallback"));
    }

    #[test]
    fn plain_to_anywhere_else_is_refused() {
      for url in [
        "ws://sfu.example.com/call/fallback",
        "ws://192.0.2.1:8082/call/fallback",
        "ws://[2001:db8::1]:8082/call/fallback",
        "ws://localhost.example.com/call/fallback",
        "ws://127.0.0.1.example.com/call/fallback",
        "http://localhost/call/fallback",
        "https://sfu.example.com/call/fallback",
        "",
      ] {
        assert!(!is_allowed_url(url), "{url}");
      }
    }
  }

  mod reading {
    use futures_util::stream;

    use super::*;

    #[tokio::test]
    async fn a_close_frame_says_why_and_every_frame_counts_as_life() {
      let frames = vec![
        Ok(Message::Ping(Bytes::new())),
        Ok(binary(FallbackFrame::Datagram(b"media").encode())),
        Ok(binary(vec![0x7F])),
        Ok(binary(Vec::new())),
        Ok(binary(FallbackFrame::Signal(b"not json").encode())),
        Ok(binary(
          FallbackFrame::Close {
            code: SFU_GOING_AWAY_CLOSE_CODE,
            reason: b"going away",
          }
          .encode(),
        )),
      ];
      let received = Arc::new(AtomicU64::new(0));
      let mut datagrams = Vec::new();
      let end = read_socket(Box::pin(stream::iter(frames)), Arc::clone(&received), |input| {
        if let SocketInput::Datagram(bytes) = input {
          datagrams.push(bytes);
        }
        true
      })
      .await;
      assert_eq!(end, SocketEnd::Closed(SFU_GOING_AWAY_CLOSE_CODE));
      assert!(is_going_away(SFU_GOING_AWAY_CLOSE_CODE));
      assert_eq!(datagrams, vec![Bytes::from_static(b"media")]);
      assert_eq!(received.load(Ordering::Relaxed), 6);
    }

    #[tokio::test]
    async fn a_socket_that_just_ends_is_lost_not_closed() {
      let received = Arc::new(AtomicU64::new(0));
      let end = read_socket(Box::pin(stream::empty()), received, |_| true).await;
      assert_eq!(end, SocketEnd::Lost);
    }

    #[tokio::test]
    async fn the_websockets_own_close_is_lost_too() {
      let received = Arc::new(AtomicU64::new(0));
      let frames = vec![Ok(Message::Close(None))];
      let end = read_socket(Box::pin(stream::iter(frames)), received, |_| true).await;
      assert_eq!(end, SocketEnd::Lost);
    }

    fn binary(frame: Vec<u8>) -> Message {
      Message::Binary(Bytes::from(frame))
    }
  }
}
