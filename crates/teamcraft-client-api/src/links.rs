//! Which links a client opens from content other members control (bookmarks): only web pages.
//! Other schemes (`tel:`, `content:`, `file:`, app deep links) would let a member trigger actions
//! in other apps with one tap.

const OPENABLE_SCHEMES: [&str; 2] = ["https://", "http://"];

/// Whether `url` is a web link a client may open.
pub fn is_openable_link(url: &str) -> bool {
  OPENABLE_SCHEMES.iter().any(|scheme| {
    url
      .get(..scheme.len())
      .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_web_links_open() {
    assert!(is_openable_link("https://example.com/plan"));
    assert!(is_openable_link("HTTP://example.com"));
    for url in [
      "file:///data/data/app/secret",
      "content://provider/item",
      "tel:+491234",
      "intent://scan#Intent;end",
      "javascript:alert(1)",
      "httpx://example.com",
      " https://example.com",
      "",
    ] {
      assert!(!is_openable_link(url), "{url}");
    }
  }
}
