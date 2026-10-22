//! TCP listeners shared by the SFU's HTTP endpoints (health check, development page).

use std::net::SocketAddr;

use tokio::net::TcpListener;

/// Binds a TCP listener; IPv6 addresses also accept IPv4 (IPv4-mapped).
pub fn bind_dual_stack(addr: SocketAddr) -> std::io::Result<TcpListener> {
  teamcraft_ops::bind_dual_stack(addr)
}
