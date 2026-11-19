//! The domain list of the link preview policy (e2ee.md §12): what an entry may be, how the admin
//! console stores it, and which links it covers. One implementation for the console, the server's
//! check of a published policy, and every client (senders and receivers both enforce it).
//!
//! An entry is a host name in its ASCII form (`example.com`, `xn--bcher-kva.example`) and covers
//! that host and every subdomain of it, on label boundaries: `example.com` covers
//! `docs.example.com` but not `evil-example.com` or `example.com.evil.example`. An IPv4 address
//! covers only itself, also written as an IPv4-mapped IPv6 address. Other IPv6 literals can't be
//! listed: they never match a list entry.
//!
//! Whatever the mode and the list say, links to hosts that aren't public never get a preview:
//! `localhost` and its subdomains, and loopback, private, link-local, shared, multicast and reserved
//! addresses ([`is_public_address`]). Otherwise a page could point its image or a redirect at the
//! sender's intranet and have the sender's device post what it finds there into the conversation.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

use crate::{LinkPreviewMode, LinkPreviewPolicy};

/// The most entries a domain list may have (the server refuses longer lists).
pub const MAX_LINK_PREVIEW_DOMAINS: usize = 500;
/// The longest host name in its ASCII form (RFC 1035).
const MAX_HOST_CHARS: usize = 253;
const MAX_LABEL_CHARS: usize = 63;

/// Why a link preview policy can't be signed or stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkPreviewPolicyError {
  #[error("the domain list has more than {MAX_LINK_PREVIEW_DOMAINS} entries")]
  TooManyDomains,
  /// The entry at this position is not a host name in its stored form.
  #[error("domain list entry {0} is not a host name")]
  InvalidDomain(usize),
  #[error("previews only of listed sites need at least one site")]
  EmptyAllowList,
}

/// A list entry, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListedHost {
  /// Lower-case ASCII (punycode) without a trailing dot.
  Domain(String),
  Ipv4(Ipv4Addr),
}

impl LinkPreviewPolicy {
  /// Whether the policy can be signed and stored: every entry in its stored form
  /// ([`normalize_link_preview_domain`]), at most [`MAX_LINK_PREVIEW_DOMAINS`] of them, and at
  /// least one when only listed sites get previews. The mode itself is not judged here.
  pub fn check(&self) -> Result<(), LinkPreviewPolicyError> {
    if self.domains.len() > MAX_LINK_PREVIEW_DOMAINS {
      return Err(LinkPreviewPolicyError::TooManyDomains);
    }
    if let Some(index) = self
      .domains
      .iter()
      .position(|entry| normalize_link_preview_domain(entry).as_deref() != Some(entry.as_str()))
    {
      return Err(LinkPreviewPolicyError::InvalidDomain(index));
    }
    if self.mode == LinkPreviewMode::AllowList && self.domains.is_empty() {
      return Err(LinkPreviewPolicyError::EmptyAllowList);
    }
    Ok(())
  }

  /// Whether links to `url` may get a preview. Only http(s) links to public hosts; `Off` and
  /// modes this client doesn't know allow none.
  pub fn allows(&self, url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
      return false;
    }
    let Some(host) = url.host().map(LinkHost::of) else {
      return false;
    };
    if !host.is_public() {
      return false;
    }
    let is_listed = self.lists(&host);
    match self.mode {
      LinkPreviewMode::AllowList => is_listed,
      LinkPreviewMode::BlockList => !is_listed,
      LinkPreviewMode::Off | LinkPreviewMode::Unknown(_) => false,
    }
  }

  /// Whether an entry covers `host`. Entries that aren't host names (a policy from an older
  /// console) cover nothing.
  fn lists(&self, host: &LinkHost<'_>) -> bool {
    self
      .domains
      .iter()
      .filter_map(|entry| parse_entry(entry))
      .any(|entry| match (host, entry) {
        (LinkHost::Domain(name), ListedHost::Domain(listed)) => {
          *name == listed
            || name
              .strip_suffix(listed.as_str())
              .is_some_and(|prefix| prefix.ends_with('.'))
        },
        (LinkHost::Ipv4(address), ListedHost::Ipv4(listed)) => *address == listed,
        _ => false,
      })
  }
}

/// The host of a link, as matched against the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkHost<'a> {
  /// As the URL parser gives special-scheme hosts: lower case and in punycode. Without trailing
  /// dots: a fully qualified name is the same host.
  Domain(&'a str),
  /// IPv4-mapped IPv6 addresses (`::ffff:192.0.2.10`) included: they reach the same host.
  Ipv4(Ipv4Addr),
  Ipv6(Ipv6Addr),
}

impl<'a> LinkHost<'a> {
  fn of(host: Host<&'a str>) -> Self {
    match host {
      Host::Domain(name) => Self::Domain(name.trim_end_matches('.')),
      Host::Ipv4(address) => Self::Ipv4(address),
      Host::Ipv6(address) => address.to_ipv4_mapped().map_or(Self::Ipv6(address), Self::Ipv4),
    }
  }

  fn is_public(&self) -> bool {
    match self {
      Self::Domain(name) => *name != "localhost" && !name.ends_with(".localhost") && !name.is_empty(),
      Self::Ipv4(address) => is_public_address(IpAddr::V4(*address)),
      Self::Ipv6(address) => is_public_address(IpAddr::V6(*address)),
    }
  }
}

/// Whether `address` may be contacted for a preview: not loopback, unspecified, private,
/// link-local, shared (carrier-grade NAT), multicast, broadcast or reserved, also not when IPv4 is
/// embedded in IPv6 (mapped, compatible, NAT64, 6to4). Used for address literals in links and, on
/// native clients, for what a host name resolves to.
pub fn is_public_address(address: IpAddr) -> bool {
  match address {
    IpAddr::V4(address) => is_public_ipv4(address),
    IpAddr::V6(address) => is_public_ipv6(address),
  }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
  let [first, second, ..] = address.octets();
  !(first == 0
    || address.is_loopback()
    || address.is_private()
    || address.is_link_local()
    || address.is_multicast()
    || address.is_broadcast()
    // 100.64.0.0/10, shared address space (RFC 6598).
    || (first == 100 && (second & 0b1100_0000) == 64)
    // 240.0.0.0/4, reserved.
    || first >= 240)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
  let segments = address.segments();
  if let Some(embedded) = embedded_ipv4(address) {
    return is_public_ipv4(embedded);
  }
  !(address.is_unspecified()
    || address.is_loopback()
    || address.is_multicast()
    // fc00::/7, unique local.
    || (segments[0] & 0xfe00) == 0xfc00
    // fe80::/10 link-local and fec0::/10 (deprecated) site-local.
    || (segments[0] & 0xffc0) == 0xfe80
    || (segments[0] & 0xffc0) == 0xfec0)
}

/// The IPv4 address an IPv6 address reaches: IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible
/// (`::a.b.c.d`, not `::` or `::1`), NAT64 (`64:ff9b::a.b.c.d`) and 6to4 (`2002:aabb:ccdd::`).
fn embedded_ipv4(address: Ipv6Addr) -> Option<Ipv4Addr> {
  let segments = address.segments();
  let low = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
  if let Some(mapped) = address.to_ipv4_mapped() {
    return Some(mapped);
  }
  if segments[..6] == [0; 6] && !address.is_unspecified() && !address.is_loopback() {
    return Some(low(segments[6], segments[7]));
  }
  if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
    return Some(low(segments[6], segments[7]));
  }
  if segments[0] == 0x2002 {
    return Some(low(segments[1], segments[2]));
  }
  None
}

/// What an admin typed as a list entry, in the stored form: trimmed, a leading `*.` and trailing
/// dots dropped, lower-case, international names in punycode, IPv4 addresses in dotted form.
/// `None` for anything that isn't a host name: URLs, ports, user info, paths, IPv6, empty input.
pub fn normalize_link_preview_domain(entry: &str) -> Option<String> {
  parse_entry(entry).map(|host| match host {
    ListedHost::Domain(name) => name,
    ListedHost::Ipv4(address) => address.to_string(),
  })
}

fn parse_entry(entry: &str) -> Option<ListedHost> {
  let entry = entry.trim();
  let entry = entry.strip_prefix("*.").unwrap_or(entry).trim_end_matches('.');
  // Characters that make it a URL, not a host: scheme, user info, port, path, query, IPv6.
  if entry.is_empty()
    || entry
      .chars()
      .any(|character| character.is_whitespace() || "/\\:@?#[]%*".contains(character))
  {
    return None;
  }
  match Host::parse(entry).ok()? {
    Host::Domain(name) => {
      let name = name.to_ascii_lowercase();
      is_host_name(&name).then_some(ListedHost::Domain(name))
    },
    Host::Ipv4(address) => Some(ListedHost::Ipv4(address)),
    Host::Ipv6(_) => None,
  }
}

/// RFC 1123 host name: labels of letters, digits and inner hyphens.
fn is_host_name(name: &str) -> bool {
  name.len() <= MAX_HOST_CHARS
    && name.split('.').all(|label| {
      (1..=MAX_LABEL_CHARS).contains(&label.len())
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
          .bytes()
          .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod normalize {
    use super::*;

    #[test]
    fn stores_host_names_in_ascii_lower_case() {
      for (typed, stored) in [
        ("example.com", "example.com"),
        ("  Docs.EXAMPLE.com  ", "docs.example.com"),
        ("*.example.com", "example.com"),
        ("example.com.", "example.com"),
        ("bücher.example", "xn--bcher-kva.example"),
        ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        // Full-width dots are dots (IDNA mapping).
        ("example\u{3002}com", "example.com"),
        ("intranet", "intranet"),
        ("192.0.2.10", "192.0.2.10"),
      ] {
        assert_eq!(
          normalize_link_preview_domain(typed).as_deref(),
          Some(stored),
          "{typed}"
        );
      }
    }

    #[test]
    fn refuses_what_is_not_a_host_name() {
      for typed in [
        "",
        "   ",
        "*.",
        ".",
        "https://example.com",
        "example.com/path",
        "example.com:443",
        "user@example.com",
        "user:pass@example.com",
        "[2001:db8::1]",
        "2001:db8::1",
        "exa mple.com",
        "example..com",
        "-example.com",
        "example-.com",
        "ex_ample.com",
        "*.*.example.com",
        "a*.example.com",
        "example.com?x",
        "example.com#x",
        "exam%70le.com",
        "example\\com",
        format!("{}.com", "a".repeat(64)).as_str(),
        format!("{}com", "a.".repeat(127)).as_str(),
      ] {
        assert_eq!(normalize_link_preview_domain(typed), None, "{typed:?}");
      }
    }
  }

  mod matching {
    use super::*;

    #[test]
    fn an_entry_covers_its_host_and_subdomains_on_label_boundaries() {
      let allow = policy(LinkPreviewMode::AllowList, &["example.com"]);
      for allowed in [
        "https://example.com/",
        "https://docs.example.com/x",
        "https://a.b.example.com/x",
        "http://EXAMPLE.com/",
        "https://example.com./",
        "https://docs.example.com../",
        "https://example.com:8443/x",
        "https://user:secret@example.com/",
      ] {
        assert!(allow.allows(&url(allowed)), "{allowed}");
      }
      for refused in [
        "https://evil-example.com/",
        "https://notexample.com/",
        "https://example.com.evil.example/",
        "https://example.co/",
        "https://com/",
        // User info is not the host.
        "https://example.com@evil.example/",
        "https://example.com:x@evil.example/",
        // A backslash ends the authority in http(s) URLs.
        "https://evil.example\\@example.com/",
        "ftp://example.com/",
        "mailto:someone@example.com",
      ] {
        assert!(!allow.allows(&url(refused)), "{refused}");
      }
    }

    #[test]
    fn international_names_match_in_either_form() {
      let allow = policy(LinkPreviewMode::AllowList, &["xn--bcher-kva.example"]);
      assert!(allow.allows(&url("https://bücher.example/")));
      assert!(allow.allows(&url("https://shop.BÜCHER.example/")));
      assert!(allow.allows(&url("https://xn--bcher-kva.example/")));
      // A look-alike in another script is another host.
      assert!(!allow.allows(&url("https://bucher.example/")));
    }

    #[test]
    fn addresses_match_only_themselves() {
      let allow = policy(LinkPreviewMode::AllowList, &["192.0.2.10"]);
      assert!(allow.allows(&url("http://192.0.2.10/")));
      assert!(allow.allows(&url("http://192.0.2.10:8080/")));
      // Other spellings of the same address are the same address.
      assert!(allow.allows(&url("http://3221225994/")));
      assert!(allow.allows(&url("http://0xc0.0.2.10/")));
      assert!(!allow.allows(&url("http://192.0.2.11/")));
      assert!(!allow.allows(&url("http://1.192.0.2.10.example/")));
      // A domain entry never covers an address that ends like it, or the other way round.
      let suffix = policy(LinkPreviewMode::AllowList, &["0.2.10"]);
      assert!(!suffix.allows(&url("http://192.0.2.10/")));
      let block = policy(LinkPreviewMode::BlockList, &["example.com"]);
      assert!(block.allows(&url("http://[2001:db8::1]/")));
      let allow = policy(LinkPreviewMode::AllowList, &["example.com"]);
      assert!(!allow.allows(&url("http://[2001:db8::1]/")));
    }

    #[test]
    fn ipv4_mapped_ipv6_is_the_ipv4_address() {
      let block = policy(LinkPreviewMode::BlockList, &["192.0.2.10"]);
      assert!(!block.allows(&url("http://[::ffff:192.0.2.10]/")));
      assert!(!block.allows(&url("http://[::ffff:c000:20a]/")));
      assert!(block.allows(&url("http://[::ffff:192.0.2.11]/")));
      let allow = policy(LinkPreviewMode::AllowList, &["192.0.2.10"]);
      assert!(allow.allows(&url("http://[::ffff:192.0.2.10]/")));
    }

    #[test]
    fn hosts_that_are_not_public_never_get_previews() {
      // Even when an admin lists them: the sender's intranet stays out of the conversation.
      let everything = policy(LinkPreviewMode::BlockList, &[]);
      let listed = policy(
        LinkPreviewMode::AllowList,
        &[
          "localhost",
          "127.0.0.1",
          "192.168.1.10",
          "10.0.0.1",
          "intranet.localhost",
        ],
      );
      for refused in [
        "http://localhost/",
        "http://LOCALHOST./x",
        "http://intranet.localhost/",
        "http://127.0.0.1/",
        "http://127.1/",
        "http://2130706433/",
        "http://0.0.0.0/",
        "http://0/",
        "http://10.0.0.1/",
        "http://172.16.0.1/",
        "http://172.31.255.255/",
        "http://192.168.1.10/x.jpg",
        "http://169.254.169.254/latest/meta-data/",
        "http://100.64.0.1/",
        "http://100.127.255.255/",
        "http://224.0.0.1/",
        "http://240.0.0.1/",
        "http://255.255.255.255/",
        "http://[::]/",
        "http://[::1]/",
        "http://[fc00::1]/",
        "http://[fd12:3456::1]/",
        "http://[fe80::1]/",
        "http://[febf::1]/",
        "http://[fec0::1]/",
        "http://[ff02::1]/",
        "http://[::ffff:127.0.0.1]/",
        "http://[::ffff:192.168.1.10]/",
        "http://[::ffff:10.0.0.1]/",
        "http://[::127.0.0.1]/",
        "http://[64:ff9b::a00:1]/",
        "http://[2002:c0a8:010a::1]/",
      ] {
        assert!(!everything.allows(&url(refused)), "{refused}");
        assert!(!listed.allows(&url(refused)), "{refused}");
      }
      for public in [
        "http://192.0.2.10/",
        "http://172.32.0.1/",
        "http://100.128.0.1/",
        "http://8.8.8.8/",
        "http://[2001:db8::1]/",
        "http://[2606:4700::1111]/",
        "http://[64:ff9b::808:808]/",
        "https://localhost.example.com/",
        "https://mylocalhost/",
      ] {
        assert!(everything.allows(&url(public)), "{public}");
      }
    }

    #[test]
    fn modes_decide_what_a_listed_host_means() {
      let block = policy(LinkPreviewMode::BlockList, &["tracker.example"]);
      assert!(!block.allows(&url("https://a.tracker.example/")));
      assert!(!block.allows(&url("https://tracker.example./")));
      assert!(block.allows(&url("https://example.org/")));
      assert!(block.allows(&url("https://evil-tracker.example/")));
      let everything = policy(LinkPreviewMode::BlockList, &[]);
      assert!(everything.allows(&url("https://example.org/")));
      for mode in [LinkPreviewMode::Off, LinkPreviewMode::Unknown("newer".into())] {
        let policy = policy(mode, &["example.org"]);
        assert!(!policy.allows(&url("https://example.org/")));
      }
    }

    #[test]
    fn entries_that_are_not_host_names_cover_nothing() {
      // A list from before the check: the bad entry is skipped, the good one still works.
      let allow = policy(
        LinkPreviewMode::AllowList,
        &["https://evil.example", "", "example.com"],
      );
      assert!(!allow.allows(&url("https://evil.example/")));
      assert!(allow.allows(&url("https://example.com/")));
    }
  }

  mod check {
    use super::*;

    #[test]
    fn accepts_stored_forms() {
      assert_eq!(policy(LinkPreviewMode::Off, &[]).check(), Ok(()));
      assert_eq!(policy(LinkPreviewMode::BlockList, &[]).check(), Ok(()));
      assert_eq!(
        policy(LinkPreviewMode::AllowList, &["example.com", "192.0.2.10"]).check(),
        Ok(())
      );
    }

    #[test]
    fn refuses_lists_that_are_not_in_stored_form() {
      for (index, bad) in [
        (1, "Example.com"),
        (1, "*.example.com"),
        (1, "bücher.example"),
        (1, ""),
      ] {
        assert_eq!(
          policy(LinkPreviewMode::BlockList, &["example.org", bad]).check(),
          Err(LinkPreviewPolicyError::InvalidDomain(index)),
          "{bad:?}"
        );
      }
    }

    #[test]
    fn refuses_long_lists_and_empty_allow_lists() {
      assert_eq!(
        policy(LinkPreviewMode::AllowList, &[]).check(),
        Err(LinkPreviewPolicyError::EmptyAllowList)
      );
      let many: Vec<String> = (0..=MAX_LINK_PREVIEW_DOMAINS)
        .map(|index| format!("host{index}.example"))
        .collect();
      let long = LinkPreviewPolicy {
        mode: LinkPreviewMode::BlockList,
        domains: many,
      };
      assert_eq!(long.check(), Err(LinkPreviewPolicyError::TooManyDomains));
    }
  }

  fn policy(mode: LinkPreviewMode, domains: &[&str]) -> LinkPreviewPolicy {
    LinkPreviewPolicy {
      mode,
      domains: domains.iter().map(|domain| (*domain).to_owned()).collect(),
    }
  }

  fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
  }
}
