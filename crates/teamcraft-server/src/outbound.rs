//! Guards for requests the server makes to addresses someone else chose: Web Push endpoints a
//! client registers, the holiday feed an admin sets. Such a request must not reach into the
//! server's own network (SSRF): the URL is checked by its text, and the client resolves names
//! through [`PublicOnlyResolver`], so the connection goes to an address that was checked right then.

use std::net::{IpAddr, SocketAddr};

use url::{Host, Url};

/// Why a URL is not one the server fetches from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlRefusal {
  #[error("not a URL")]
  NotAUrl,
  #[error("must use https")]
  NotHttps,
  #[error("must not contain credentials")]
  Credentials,
  #[error("points to a private address")]
  Private,
}

/// Checks a URL the server will fetch from by its text: https, no user or password, and a host
/// that is neither a private address nor a name for this machine or the local network. Names that
/// resolve to private addresses are refused by [`PublicOnlyResolver`] when connecting.
pub fn check_public_url(url: &Url) -> Result<(), UrlRefusal> {
  if url.scheme() != "https" {
    return Err(UrlRefusal::NotHttps);
  }
  if !url.username().is_empty() || url.password().is_some() {
    return Err(UrlRefusal::Credentials);
  }
  match url.host() {
    Some(host) if !is_private_host(&host) => Ok(()),
    _ => Err(UrlRefusal::Private),
  }
}

/// [`check_public_url`] of a URL as text.
pub fn check_public_url_text(text: &str) -> Result<Url, UrlRefusal> {
  let url: Url = text.parse().map_err(|_| UrlRefusal::NotAUrl)?;
  check_public_url(&url)?;
  Ok(url)
}

/// Whether a host is a private address or a name for this machine or the local network.
pub fn is_private_host(host: &Host<&str>) -> bool {
  match host {
    Host::Domain(name) => {
      let name = name.trim_end_matches('.').to_ascii_lowercase();
      name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        // A name without a dot is looked up in the local search domains.
        || !name.contains('.')
    },
    Host::Ipv4(ip) => is_private_ip(IpAddr::V4(*ip)),
    Host::Ipv6(ip) => is_private_ip(IpAddr::V6(*ip)),
  }
}

/// Whether the address is not a public unicast address: loopback, private, link-local, shared
/// (CGNAT), documentation, benchmarking, multicast, reserved, and the IPv6 transition ranges
/// (NAT64, 6to4, Teredo, IPv4-compatible) that can carry a private IPv4 address inside.
pub fn is_private_ip(ip: IpAddr) -> bool {
  match ip {
    IpAddr::V4(ip) => {
      let [a, b, c, _] = ip.octets();
      ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        // 0.0.0.0/8 "this network"
        || a == 0
        // 100.64.0.0/10 shared address space
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24 IETF protocol assignments, 198.18.0.0/15 benchmarking, 192.88.99.0/24
        // deprecated 6to4 relay anycast
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19))
        // 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24 documentation
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // 240.0.0.0/4 reserved
        || a >= 240
    },
    IpAddr::V6(ip) => {
      let segments = ip.segments();
      ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        // fc00::/7 unique local, fe80::/10 link-local, fec0::/10 site-local
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        // 64:ff9b::/96 and 64:ff9b:1::/48 NAT64, 2002::/16 6to4, 2001::/32 Teredo
        || (segments[0] == 0x64 && segments[1] == 0xff9b)
        || segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] == 0)
        // 2001:db8::/32 and 3fff::/20 documentation, 2001:20::/28 ORCHIDv2, 100::/64 discard-only
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || (segments[0] & 0xfff0) == 0x3ff0
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020)
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
        // ::a.b.c.d IPv4-compatible (and ::ffff:a.b.c.d mapped, checked as IPv4)
        || (segments[..6] == [0; 6])
        // ::ffff:0:a.b.c.d IPv4-translated (SIIT)
        || (segments[..4] == [0; 4] && segments[4] == 0xffff && segments[5] == 0)
        || ip.to_ipv4_mapped().is_some_and(|v4| is_private_ip(IpAddr::V4(v4)))
    },
  }
}

/// Resolves names for an HTTP client and drops every non-public address, so the connection goes
/// to an address that was checked right now (no gap in which a name could point somewhere else).
/// A name that only resolves to private addresses cannot be reached at all. Use it with
/// `no_proxy()`: a proxy resolves names itself, past this resolver.
pub struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
  fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
    Box::pin(async move {
      let addresses: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
        .await?
        .filter(|address| !is_private_ip(address.ip()))
        .collect();
      if addresses.is_empty() {
        return Err(std::io::Error::other("the name resolves only to private addresses").into());
      }
      Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn public_https_urls_pass() {
    for url in [
      "https://www.feiertage.example.com/de.ics",
      "https://calendar.example.org/holidays/basic.ics?token=abc",
      "https://[2606:4700::1111]/feed.ics",
      "https://8.8.8.8/feed.ics",
    ] {
      assert!(check_public_url_text(url).is_ok(), "{url}");
    }
  }

  #[test]
  fn private_plain_and_credentialed_urls_are_refused() {
    for (url, refusal) in [
      ("http://holidays.example.com/x.ics", UrlRefusal::NotHttps),
      ("ftp://holidays.example.com/x.ics", UrlRefusal::NotHttps),
      ("file:///etc/passwd", UrlRefusal::NotHttps),
      ("https://user:pw@holidays.example.com/x", UrlRefusal::Credentials),
      ("https://localhost/x", UrlRefusal::Private),
      ("https://LOCALHOST./x", UrlRefusal::Private),
      ("https://printer.local/x", UrlRefusal::Private),
      ("https://metadata.google.internal/x", UrlRefusal::Private),
      ("https://intranet/x", UrlRefusal::Private),
      ("https://127.0.0.1/x", UrlRefusal::Private),
      ("https://10.0.0.8/x", UrlRefusal::Private),
      ("https://169.254.169.254/latest/meta-data", UrlRefusal::Private),
      ("https://[::1]/x", UrlRefusal::Private),
      ("https://[::ffff:192.168.1.1]/x", UrlRefusal::Private),
      ("https://[fd12::1]/x", UrlRefusal::Private),
      ("not a url", UrlRefusal::NotAUrl),
    ] {
      assert_eq!(check_public_url_text(url).err(), Some(refusal), "{url}");
    }
  }

  #[test]
  fn public_addresses_are_not_private() {
    for ip in ["8.8.8.8", "142.250.0.1", "2606:4700::1111", "2a00:1450:4001::1"] {
      assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
    }
  }
}
