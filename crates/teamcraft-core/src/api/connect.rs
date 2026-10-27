//! TCP connections for the native WebSockets: Happy Eyeballs (RFC 8305, issue #420) so a
//! black-holed IPv6 route doesn't use up the connect timeout before IPv4 is tried, and an HTTP
//! `CONNECT` tunnel through the proxy that [`super::proxy`] picks (issue #157). reqwest does both
//! for REST (hyper-util races the families after 300 ms).

use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use teamcraft_net::{ATTEMPT_DELAY, RaceError, interleave, race};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use url::{Host, Url};

/// Longest proxy answer to `CONNECT` that is read; real ones are a status line and a few headers.
const MAX_PROXY_RESPONSE_BYTES: usize = 8 * 1024;

/// Why a tunnel through the proxy failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TunnelError {
  #[error("connecting to the proxy failed: {0}")]
  Io(#[from] io::Error),
  #[error("the proxy refused the tunnel: {0}")]
  Refused(String),
  #[error("the proxy asks for credentials (407)")]
  AuthenticationRequired,
  #[error("the proxy answered something other than HTTP")]
  Malformed,
}

/// A TCP connection to the host of `url` (`ws`/`wss`), through `maybe_proxy` (an `http://` proxy
/// from [`super::proxy::tunnel_proxy_for`]) or directly.
pub(crate) async fn connect_via(url: &Url, maybe_proxy: Option<&Url>) -> Result<TcpStream, TunnelError> {
  let (host, port) = host_and_port(url).ok_or(TunnelError::Malformed)?;
  match maybe_proxy {
    Some(proxy) => {
      let (proxy_host, proxy_port) = host_and_port(proxy).ok_or(TunnelError::Malformed)?;
      tracing::debug!(proxy = %proxy_host, "connecting through the proxy");
      let stream = connect_host(&proxy_host, proxy_port).await?;
      tunnel(stream, &authority(&host, port), proxy).await
    },
    None => Ok(connect_host(&host, port).await?),
  }
}

/// Resolves `host` and races its addresses (Happy Eyeballs, `teamcraft-net`).
pub(crate) async fn connect_host(host: &str, port: u16) -> io::Result<TcpStream> {
  let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
  connect_first(interleave(addresses), ATTEMPT_DELAY).await
}

/// The first of `addresses` that accepts a TCP connection, raced as [`teamcraft_net::race`] does.
pub(crate) async fn connect_first(addresses: Vec<SocketAddr>, delay: Duration) -> io::Result<TcpStream> {
  race(addresses, delay, TcpStream::connect)
    .await
    .map_err(|error| match error {
      RaceError::NoAddress => io::Error::new(io::ErrorKind::NotFound, "the host has no address"),
      RaceError::Failed(error) => error,
    })
}

/// `CONNECT authority` on `stream`; the stream then carries the connection to the target.
async fn tunnel(mut stream: TcpStream, authority: &str, proxy: &Url) -> Result<TcpStream, TunnelError> {
  let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
  if !proxy.username().is_empty() {
    let user = percent_decoded(proxy.username());
    let password = percent_decoded(proxy.password().unwrap_or_default());
    let credentials = STANDARD.encode(format!("{user}:{password}"));
    request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
  }
  request.push_str("\r\n");
  stream.write_all(request.as_bytes()).await?;
  let head = read_head(&mut stream).await?;
  let status_line = head.lines().next().unwrap_or_default();
  let mut parts = status_line.split_whitespace();
  let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
    return Err(TunnelError::Malformed);
  };
  if !version.starts_with("HTTP/1.") {
    return Err(TunnelError::Malformed);
  }
  match code {
    code if code.starts_with('2') && code.len() == 3 => Ok(stream),
    "407" => Err(TunnelError::AuthenticationRequired),
    _ => Err(TunnelError::Refused(status_line.to_owned())),
  }
}

/// The proxy's answer up to the empty line. Byte by byte: what follows belongs to the tunnel.
async fn read_head(stream: &mut TcpStream) -> Result<String, TunnelError> {
  let mut head = Vec::with_capacity(256);
  while !head.ends_with(b"\r\n\r\n") {
    if head.len() >= MAX_PROXY_RESPONSE_BYTES {
      return Err(TunnelError::Malformed);
    }
    let mut byte = [0u8; 1];
    if stream.read(&mut byte).await? == 0 {
      return Err(TunnelError::Io(io::ErrorKind::UnexpectedEof.into()));
    }
    head.push(byte[0]);
  }
  String::from_utf8(head).map_err(|_| TunnelError::Malformed)
}

fn host_and_port(url: &Url) -> Option<(String, u16)> {
  let host = match url.host()? {
    Host::Ipv6(ip) => ip.to_string(),
    other => other.to_string(),
  };
  Some((
    host,
    url.port_or_known_default().or(match url.scheme() {
      "wss" => Some(443),
      "ws" => Some(80),
      _ => None,
    })?,
  ))
}

/// `host:port`, IPv6 literals in brackets.
fn authority(host: &str, port: u16) -> String {
  if host.contains(':') {
    format!("[{host}]:{port}")
  } else {
    format!("{host}:{port}")
  }
}

fn percent_decoded(text: &str) -> String {
  url::form_urlencoded::parse(format!("x={text}").as_bytes())
    .next()
    .map(|(_, value)| value.into_owned())
    .unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod black_hole {
  use std::net::SocketAddr;
  use std::time::Duration;

  use tokio::net::TcpStream;

  /// An IPv6 address on this machine that swallows connection attempts like a broken route: a
  /// listener on `[::1]` whose accept queue is full, so the kernel drops further SYNs. `None` where
  /// the machine has no IPv6 loopback (some CI sandboxes).
  pub(crate) struct BlackHole {
    pub(crate) address: SocketAddr,
    _listener: tokio::net::TcpListener,
    _queued: Vec<TcpStream>,
  }

  impl BlackHole {
    /// Most tries it takes to fill an accept queue of length 0 (Linux keeps one more).
    const MAX_QUEUED: usize = 8;
    /// How long a connect may take before it counts as swallowed. Timing-sensitive: a connect to
    /// loopback that the kernel accepts finishes in microseconds; only if one took longer than
    /// this (a machine stalled for 300 ms) would the queue count as full too early, the "black
    /// hole" then accept, and a fallback test fail spuriously.
    const HANG_PROBE: Duration = Duration::from_millis(300);

    pub(crate) async fn ipv6(port: u16) -> Option<Self> {
      let address = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port));
      let socket = tokio::net::TcpSocket::new_v6().ok()?;
      socket.bind(address).ok()?;
      let listener = socket.listen(0).ok()?;
      let mut queued = Vec::new();
      for _ in 0..Self::MAX_QUEUED {
        match tokio::time::timeout(Self::HANG_PROBE, TcpStream::connect(address)).await {
          Ok(Ok(stream)) => queued.push(stream),
          // Timed out: the queue is full and this attempt was swallowed.
          Err(_) => {
            return Some(Self {
              address,
              _listener: listener,
              _queued: queued,
            });
          },
          Ok(Err(_)) => return None,
        }
      }
      None
    }
  }
}

#[cfg(test)]
mod tests {
  use std::net::Ipv4Addr;

  use tokio::net::TcpListener;

  use super::*;

  #[test]
  fn ipv6_authorities_are_bracketed() {
    assert_eq!(authority("::1", 443), "[::1]:443");
    assert_eq!(authority("chat.example.com", 443), "chat.example.com:443");
    let url = Url::parse("wss://[2001:db8::1]/api/v1/gateway").unwrap();
    assert_eq!(host_and_port(&url), Some(("2001:db8::1".into(), 443)));
  }

  /// The 1.0.2 failure (issue #420): IPv6 swallows the SYN, IPv4 works. The WebSocket connects
  /// over IPv4 an attempt delay later instead of waiting out the connect timeout on IPv6.
  #[tokio::test]
  async fn a_black_holed_ipv6_address_falls_back_to_ipv4() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let working = listener.local_addr().unwrap();
    let Some(black_hole) = super::black_hole::BlackHole::ipv6(working.port()).await else {
      return;
    };
    let started = std::time::Instant::now();
    let connecting = connect_first(vec![black_hole.address, working], ATTEMPT_DELAY);
    let stream = tokio::time::timeout(Duration::from_secs(5), connecting)
      .await
      .expect("fell back within the timeout")
      .unwrap();
    assert_eq!(stream.peer_addr().unwrap(), working);
    assert!(
      started.elapsed() >= ATTEMPT_DELAY,
      "IPv4 started before the attempt delay"
    );
  }

  #[tokio::test]
  async fn the_tunnel_sends_connect_with_credentials_and_hands_over_the_stream() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let proxy = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.unwrap();
      let head = read_head_of(&mut socket).await;
      socket
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nfrom the target")
        .await
        .unwrap();
      head
    });
    let stream = TcpStream::connect(address).await.unwrap();
    let proxy_url = Url::parse(&format!("http://user%40corp:p%3Ass@{address}")).unwrap();
    let mut tunneled = tunnel(stream, "chat.example.com:443", &proxy_url).await.unwrap();
    let mut rest = String::new();
    tunneled.read_to_string(&mut rest).await.unwrap();
    assert_eq!(rest, "from the target");
    let head = proxy.await.unwrap();
    assert!(
      head.starts_with("CONNECT chat.example.com:443 HTTP/1.1\r\n"),
      "{head}"
    );
    let credentials = STANDARD.encode("user@corp:p:ss");
    assert!(
      head.contains(&format!("Proxy-Authorization: Basic {credentials}\r\n")),
      "{head}"
    );
  }

  #[tokio::test]
  async fn a_refusing_proxy_is_an_error() {
    for (answer, check) in [
      (
        "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
        (|error: &TunnelError| matches!(error, TunnelError::AuthenticationRequired))
          as fn(&TunnelError) -> bool,
      ),
      ("HTTP/1.1 403 Forbidden\r\n\r\n", |error| {
        matches!(error, TunnelError::Refused(_))
      }),
      ("SSH-2.0-OpenSSH\r\n\r\n", |error| {
        matches!(error, TunnelError::Malformed)
      }),
    ] {
      let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
      let address = listener.local_addr().unwrap();
      tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_head_of(&mut socket).await;
        socket.write_all(answer.as_bytes()).await.unwrap();
      });
      let stream = TcpStream::connect(address).await.unwrap();
      let proxy = Url::parse(&format!("http://{address}")).unwrap();
      let error = tunnel(stream, "chat.example.com:443", &proxy).await.unwrap_err();
      assert!(check(&error), "{answer}: {error}");
    }
  }

  async fn read_head_of(socket: &mut TcpStream) -> String {
    read_head(socket).await.unwrap()
  }
}
