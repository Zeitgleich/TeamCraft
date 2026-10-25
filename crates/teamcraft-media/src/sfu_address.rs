//! Every address of the SFU's host, for racing the QUIC handshake (Happy Eyeballs, issue #420).
//! wtransport resolves a host to its first address only, so a broken IPv6 route would leave a
//! call without a connection although IPv4 works. The driver resolves all addresses itself and
//! pins each attempt to one of them with [`PinnedResolver`]; the URL keeps the host name for TLS.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;

use wtransport::config::{DnsLookupFuture, DnsResolver};

const DEFAULT_PORT: u16 = 443;

/// The addresses `sfu_url` (`https://host[:port]/...`) resolves to, in the resolver's order.
pub(crate) async fn sfu_addresses(sfu_url: &str) -> io::Result<Vec<SocketAddr>> {
  let (host, port) = host_and_port(sfu_url)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the SFU address has no host"))?;
  Ok(tokio::net::lookup_host((host.as_str(), port)).await?.collect())
}

/// Answers every lookup with one address.
pub(crate) struct PinnedResolver(pub(crate) SocketAddr);

impl DnsResolver for PinnedResolver {
  fn resolve(&self, _host: &str) -> Pin<Box<dyn DnsLookupFuture>> {
    let address = self.0;
    Box::pin(std::future::ready(Ok(Some(address))))
  }
}

impl fmt::Debug for PinnedResolver {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("PinnedResolver").field(&self.0).finish()
  }
}

/// Host and port of a URL; IPv6 literals without their brackets.
fn host_and_port(url: &str) -> Option<(String, u16)> {
  let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
  let authority = rest.split(['/', '?', '#']).next()?;
  let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
  let (host, maybe_port) = match authority.strip_prefix('[') {
    Some(bracketed) => {
      let (host, after) = bracketed.split_once(']')?;
      (host, after.strip_prefix(':'))
    },
    None => match authority.rsplit_once(':') {
      Some((host, port)) => (host, Some(port)),
      None => (authority, None),
    },
  };
  if host.is_empty() {
    return None;
  }
  let port = match maybe_port {
    Some(port) => port.parse().ok()?,
    None => DEFAULT_PORT,
  };
  Some((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn hosts_and_ports_come_out_of_sfu_urls() {
    assert_eq!(
      host_and_port("https://sfu.example.com:4433/call"),
      Some(("sfu.example.com".into(), 4433))
    );
    assert_eq!(
      host_and_port("https://sfu.example.com/call"),
      Some(("sfu.example.com".into(), 443))
    );
    assert_eq!(
      host_and_port("https://[2001:db8::1]:4433/call?x=1"),
      Some(("2001:db8::1".into(), 4433))
    );
    assert_eq!(host_and_port("https://:4433/"), None);
    assert_eq!(host_and_port("https://sfu.example.com:port/"), None);
  }

  #[tokio::test]
  async fn the_pinned_resolver_answers_its_address_for_any_name() {
    let address: SocketAddr = "192.0.2.7:4433".parse().unwrap();
    let resolved = PinnedResolver(address).resolve("sfu.example.com").await.unwrap();
    assert_eq!(resolved, Some(address));
  }

  #[tokio::test]
  async fn literal_addresses_resolve_without_dns() {
    let addresses = sfu_addresses("https://127.0.0.1:4433/call").await.unwrap();
    assert_eq!(addresses, vec!["127.0.0.1:4433".parse().unwrap()]);
  }
}
