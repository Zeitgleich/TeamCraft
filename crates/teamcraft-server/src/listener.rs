use std::io;
use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;

const LISTEN_BACKLOG: i32 = 1024;

/// Binds the HTTP listener. An IPv6 address is bound with `IPV6_V6ONLY` off, so `[::]` accepts IPv4
/// (as IPv4-mapped addresses) and IPv6 regardless of the host's `bindv6only` default.
pub fn bind_listener(addr: SocketAddr) -> io::Result<TcpListener> {
  let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
  if addr.is_ipv6() {
    socket.set_only_v6(false)?;
  }
  socket.set_reuse_address(true)?;
  socket.set_nonblocking(true)?;
  socket.bind(&addr.into())?;
  socket.listen(LISTEN_BACKLOG)?;
  TcpListener::from_std(socket.into())
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, Ipv6Addr};

  use tokio::net::TcpStream;

  use super::*;

  #[tokio::test]
  async fn dual_stack_listener_accepts_ipv4_and_ipv6() {
    // Hosts without IPv6 cannot bind `[::]`; nothing to test there.
    let Ok(listener) = bind_listener((Ipv6Addr::UNSPECIFIED, 0).into()) else {
      return;
    };
    let port = listener.local_addr().unwrap().port();

    let v6 = TcpStream::connect((Ipv6Addr::LOCALHOST, port)).await;
    let v4 = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await;
    assert!(v6.is_ok(), "IPv6 connection failed: {v6:?}");
    assert!(v4.is_ok(), "IPv4 connection failed: {v4:?}");
  }

  #[tokio::test]
  async fn ipv4_only_listener_works() {
    let listener = bind_listener((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.is_ok());
  }

  #[tokio::test]
  async fn ipv6_loopback_listener_works() {
    let Ok(listener) = bind_listener((Ipv6Addr::LOCALHOST, 0).into()) else {
      return;
    };
    let port = listener.local_addr().unwrap().port();
    assert!(TcpStream::connect((Ipv6Addr::LOCALHOST, port)).await.is_ok());
  }
}
