//! The meeting link a member hands out (docs/meetings.md, "Links"): the server's address with the
//! organizer's key fingerprint in the fragment as the copying device has it pinned (`v=`), never
//! the server's (e2ee.md §4.6). Desktop, web and the phone apps make it alike.

use teamcraft_crypto::guest_link::code_of_fingerprint;

/// The organizer's identity key fingerprint in a meeting link's fragment.
pub const VERIFIER_FRAGMENT_KEY: &str = "v=";
/// Its name in links of older apps.
pub const LEGACY_ORGANIZER_FRAGMENT_KEY: &str = "o=";

/// The organizer's code from a link fragment (`v=<fingerprint>`): what the guest's page shows,
/// for the guest to compare with the code the organizer reads out.
pub fn organizer_code(fragment: &str) -> String {
  code_of_fingerprint(fragment.strip_prefix(VERIFIER_FRAGMENT_KEY).unwrap_or(fragment))
}

/// `link` with `fragment` (`v=<fingerprint>`) as its verifier, replacing any `v` the server put
/// there: the copying device's pinned key wins over the server's word.
pub fn with_verifier(link: &str, fragment: &str) -> String {
  let (path, maybe_rest) = match link.split_once('#') {
    Some((path, rest)) => (path, Some(rest)),
    None => (link, None),
  };
  let mut parts: Vec<&str> = maybe_rest
    .into_iter()
    .flat_map(|rest| rest.split('&'))
    .filter(|part| {
      !part.is_empty()
        && !part.starts_with(VERIFIER_FRAGMENT_KEY)
        && !part.starts_with(LEGACY_ORGANIZER_FRAGMENT_KEY)
    })
    .collect();
  parts.push(fragment);
  format!("{path}#{}", parts.join("&"))
}

#[cfg(test)]
mod tests {
  use super::*;

  const BASE: &str = "https://chat.example.com/meeting/AbCdEfGhIjKlMnOpQrStUvWxYz012345";

  #[test]
  fn the_copying_devices_fingerprint_replaces_the_servers() {
    assert_eq!(with_verifier(BASE, "v=11"), format!("{BASE}#v=11"));
    assert_eq!(
      with_verifier(&format!("{BASE}#v=22"), "v=11"),
      format!("{BASE}#v=11")
    );
    assert_eq!(
      with_verifier(&format!("{BASE}#o=22"), "v=11"),
      format!("{BASE}#v=11")
    );
    assert_eq!(
      with_verifier(&format!("{BASE}#invite=abc&v=22"), "v=11"),
      format!("{BASE}#invite=abc&v=11")
    );
  }

  #[test]
  fn the_organizer_code_is_read_from_the_verifier() {
    assert_eq!(
      organizer_code("v=ABCDEF0123456789ffff"),
      organizer_code("abcdef0123456789FFFF")
    );
    assert!(!organizer_code("v=abcdef0123456789").contains('v'));
    assert_eq!(organizer_code("v=zz"), "");
  }
}
