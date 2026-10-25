//! Which links a client opens from content other members control (bookmarks): only web pages.
//! Other schemes (`tel:`, `content:`, `file:`, app deep links) would let a member trigger actions
//! in other apps with one tap.
//!
//! [`is_external_link`] is the wider list the desktop shell hands to the system: web pages and mail
//! addresses. Both the UI (which routes clicks) and the shell (which opens) use it, so the two
//! can't drift apart.

const OPENABLE_SCHEMES: [&str; 2] = ["https://", "http://"];

/// Whether `url` is a web link a client may open.
pub fn is_openable_link(url: &str) -> bool {
  OPENABLE_SCHEMES.iter().any(|scheme| {
    url
      .get(..scheme.len())
      .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
  })
}

const EXTERNAL_SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

/// Whether `url` may leave the app for the system's browser or mail client.
pub fn is_external_link(url: &str) -> bool {
  EXTERNAL_SCHEMES.iter().any(|scheme| {
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

  #[test]
  fn web_and_mail_links_leave_the_app() {
    for url in [
      "https://example.com/a",
      "HTTP://example.com",
      "mailto:a@example.com",
      "MAILTO:a@example.com",
    ] {
      assert!(is_external_link(url), "{url}");
    }
    for url in [
      "file:///etc/passwd",
      "tauri://localhost/",
      "about:blank",
      "javascript:alert(1)",
      "tel:+491234",
      "mailtox:a",
      " https://example.com",
      "",
    ] {
      assert!(!is_external_link(url), "{url}");
    }
  }
}
