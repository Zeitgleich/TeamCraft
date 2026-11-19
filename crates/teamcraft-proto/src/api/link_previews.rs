//! The domain list of the link preview policy (e2ee.md §12): what an entry may be, how the admin
//! console stores it, and which links it covers. One implementation for the console, the server's
//! check of a published policy, and every client (senders and receivers both enforce it).
//!
//! An entry is a host name in its ASCII form (`example.com`, `xn--bcher-kva.example`) and covers
//! that host and every subdomain of it, on label boundaries: `example.com` covers
//! `docs.example.com` but not `evil-example.com` or `example.com.evil.example`. An IPv4 address
//! covers only itself. IPv6 literals can't be listed: they never match a list entry.

use std::net::Ipv4Addr;

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

  /// Whether links to `url` may get a preview. Only http(s) links; `Off` and modes this client
  /// doesn't know allow none.
  pub fn allows(&self, url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
      return false;
    }
    let Some(host) = url.host() else {
      return false;
    };
    let is_listed = self.lists(&host);
    match self.mode {
      LinkPreviewMode::AllowList => is_listed,
      LinkPreviewMode::BlockList => !is_listed,
      LinkPreviewMode::Off | LinkPreviewMode::Unknown(_) => false,
    }
  }

  /// Whether an entry covers `host`. Entries that aren't host names (a policy from an older
  /// console) cover nothing.
  fn lists(&self, host: &Host<&str>) -> bool {
    self
      .domains
      .iter()
      .filter_map(|entry| parse_entry(entry))
      .any(|entry| match (host, entry) {
        (Host::Domain(name), ListedHost::Domain(listed)) => {
          // The URL parser gives special-scheme hosts lower-case and in punycode; the trailing dots
          // of a fully qualified name don't make it another host.
          let name = name.trim_end_matches('.');
          name == listed
            || name
              .strip_suffix(listed.as_str())
              .is_some_and(|prefix| prefix.ends_with('.'))
        },
        (Host::Ipv4(address), ListedHost::Ipv4(listed)) => *address == listed,
        _ => false,
      })
  }
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
