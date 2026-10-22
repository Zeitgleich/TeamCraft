//! Operations helpers shared by the server binaries (`teamcraft-server`, `teamcraft-sfu`).
//!
//! - Container health checks: the images ship no `curl`, so `teamcraft-server healthcheck` and
//!   `teamcraft-sfu healthcheck` probe their own health endpoint with a minimal blocking HTTP
//!   probe (HTTP/1.0, status line only, `std` only).
//! - [`shutdown_signal`]: resolves on SIGINT or SIGTERM. In a container the process is PID 1 and
//!   the kernel ignores SIGTERM there unless a handler is installed, so every binary needs this.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Resolves when the process is asked to stop (Ctrl-C, or SIGTERM on Unix).
pub async fn shutdown_signal() {
  let ctrl_c = async {
    if tokio::signal::ctrl_c().await.is_err() {
      std::future::pending::<()>().await;
    }
  };
  #[cfg(unix)]
  let terminate = async {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
      Ok(mut stream) => {
        stream.recv().await;
      },
      Err(_) => std::future::pending::<()>().await,
    }
  };
  #[cfg(not(unix))]
  let terminate = std::future::pending::<()>();

  tokio::select! {
    () = ctrl_c => {},
    () = terminate => {},
  }
}

/// How long [`probe`] waits for connecting and for each read or write by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

/// Largest part of the response that is read; only the status line matters.
const MAX_RESPONSE_BYTES: usize = 512;

/// Why a probe failed.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
  #[error("connecting to {addr} failed: {source}")]
  Connect {
    addr: SocketAddr,
    source: std::io::Error,
  },
  #[error("talking to {addr} failed: {source}")]
  Io {
    addr: SocketAddr,
    source: std::io::Error,
  },
  #[error("{addr} answered with a malformed status line")]
  MalformedResponse { addr: SocketAddr },
  #[error("{addr} answered with HTTP status {status}")]
  Unhealthy { addr: SocketAddr, status: u16 },
}

/// The address to probe for a service that listens on `listen`: unspecified addresses (`[::]`,
/// `0.0.0.0`) are reached through loopback of the same family.
pub fn probe_target(listen: SocketAddr) -> SocketAddr {
  let ip = match listen.ip() {
    IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
    IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
    ip => ip,
  };
  SocketAddr::new(ip, listen.port())
}

/// `GET path` on `addr`; succeeds on any 2xx status.
pub fn probe(addr: SocketAddr, path: &str, timeout: Duration) -> Result<(), ProbeError> {
  let io_error = |source| ProbeError::Io { addr, source };
  let mut stream =
    TcpStream::connect_timeout(&addr, timeout).map_err(|source| ProbeError::Connect { addr, source })?;
  stream.set_read_timeout(Some(timeout)).map_err(io_error)?;
  stream.set_write_timeout(Some(timeout)).map_err(io_error)?;
  // One write: `write!` on the stream would send the request in several segments.
  let request = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n");
  stream.write_all(request.as_bytes()).map_err(io_error)?;

  let mut buffer = [0_u8; MAX_RESPONSE_BYTES];
  let mut filled = 0;
  // The status line ends at the first CRLF; read until we have it or the buffer is full.
  while filled < buffer.len() && !buffer[..filled].windows(2).any(|pair| pair == b"\r\n") {
    let read = stream.read(&mut buffer[filled..]).map_err(io_error)?;
    if read == 0 {
      break;
    }
    filled += read;
  }
  let status = parse_status(&buffer[..filled]).ok_or(ProbeError::MalformedResponse { addr })?;
  if (200..300).contains(&status) {
    Ok(())
  } else {
    Err(ProbeError::Unhealthy { addr, status })
  }
}

/// The status code of `HTTP/1.x <code> ...`.
fn parse_status(response: &[u8]) -> Option<u16> {
  let line_end = response
    .iter()
    .position(|byte| *byte == b'\r' || *byte == b'\n')?;
  let line = std::str::from_utf8(&response[..line_end]).ok()?;
  let mut parts = line.split(' ');
  parts.next().filter(|version| version.starts_with("HTTP/1."))?;
  parts.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
  use std::net::TcpListener;
  use std::thread;

  use super::*;

  const TIMEOUT: Duration = Duration::from_secs(2);

  mod probing {
    use super::*;

    #[test]
    fn success_status_passes_on_ipv4() {
      let addr = serve(
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok",
        Ipv4Addr::LOCALHOST.into(),
      );
      assert!(probe(addr, "/healthz", TIMEOUT).is_ok());
    }

    #[test]
    fn success_status_passes_on_ipv6() {
      // Hosts without IPv6 cannot bind `[::1]`; nothing to test there.
      let Some(addr) = try_serve("HTTP/1.1 204 No Content\r\n\r\n", Ipv6Addr::LOCALHOST.into()) else {
        return;
      };
      assert!(probe(addr, "/healthz", TIMEOUT).is_ok());
    }

    #[test]
    fn error_statuses_fail() {
      let addr = serve(
        "HTTP/1.1 503 Service Unavailable\r\n\r\n",
        Ipv4Addr::LOCALHOST.into(),
      );
      assert!(matches!(
        probe(addr, "/readyz", TIMEOUT),
        Err(ProbeError::Unhealthy { status: 503, .. })
      ));
    }

    #[test]
    fn garbage_and_closed_connections_fail() {
      let garbage = serve("not http at all\r\n", Ipv4Addr::LOCALHOST.into());
      assert!(matches!(
        probe(garbage, "/", TIMEOUT),
        Err(ProbeError::MalformedResponse { .. })
      ));
      // A peer that closes without answering may surface as an empty response or, when the close
      // races our request, as a reset; both must fail the probe.
      let silent = serve("", Ipv4Addr::LOCALHOST.into());
      assert!(matches!(
        probe(silent, "/", TIMEOUT),
        Err(ProbeError::MalformedResponse { .. } | ProbeError::Io { .. })
      ));
      let closed = {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.local_addr().unwrap()
      };
      assert!(matches!(
        probe(closed, "/", TIMEOUT),
        Err(ProbeError::Connect { .. })
      ));
    }

    #[test]
    fn the_request_names_the_path() {
      let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
      let addr = listener.local_addr().unwrap();
      let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 256];
        let read = stream.read(&mut request).unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        String::from_utf8_lossy(&request[..read]).into_owned()
      });
      probe(addr, "/readyz", TIMEOUT).unwrap();
      assert!(server.join().unwrap().starts_with("GET /readyz HTTP/1.0\r\n"));
    }
  }

  mod targets {
    use super::*;

    #[test]
    fn unspecified_addresses_map_to_loopback_of_the_same_family() {
      let v6 = SocketAddr::from((Ipv6Addr::UNSPECIFIED, 8080));
      let v4 = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 8080));
      assert_eq!(probe_target(v6), SocketAddr::from((Ipv6Addr::LOCALHOST, 8080)));
      assert_eq!(probe_target(v4), SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)));
    }

    #[test]
    fn concrete_addresses_are_kept() {
      let addr: SocketAddr = "192.0.2.7:9000".parse().unwrap();
      assert_eq!(probe_target(addr), addr);
    }
  }

  fn serve(response: &'static str, ip: IpAddr) -> SocketAddr {
    try_serve(response, ip).unwrap()
  }

  /// Answers one connection with `response`; `None` if the address cannot be bound.
  fn try_serve(response: &'static str, ip: IpAddr) -> Option<SocketAddr> {
    let listener = TcpListener::bind((ip, 0)).ok()?;
    let addr = listener.local_addr().ok()?;
    thread::spawn(move || {
      if let Ok((mut stream, _)) = listener.accept() {
        let mut request = [0_u8; 256];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(response.as_bytes());
      }
    });
    Some(addr)
  }
}
