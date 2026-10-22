//! TCP listeners shared by the SFU's HTTP endpoints (health check, development page).

use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;

const LISTEN_BACKLOG: i32 = 128;

/// Binds a TCP listener; IPv6 addresses also accept IPv4 (IPv4-mapped).
pub fn bind_dual_stack(addr: SocketAddr) -> std::io::Result<TcpListener> {
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
