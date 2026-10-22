use std::io;
use std::net::SocketAddr;

use tokio::net::TcpListener;

/// Binds the HTTP listener, dual stack for IPv6 addresses (see [`teamcraft_ops::bind_dual_stack`]).
pub fn bind_listener(addr: SocketAddr) -> io::Result<TcpListener> {
  teamcraft_ops::bind_dual_stack(addr)
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
