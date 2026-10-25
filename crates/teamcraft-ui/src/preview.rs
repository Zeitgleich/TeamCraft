//! What the in-app preview of a file (#557) is made of, apart from the DOM so it can be tested:
//! which files get a preview, how big they may be, and the document an HTML file is shown in.
//!
//! The file is untrusted. An HTML file is shown in an `<iframe>` that is sandboxed with no
//! permission at all (no scripts, no `allow-same-origin`: an opaque origin that reaches nothing of
//! the app, its storage or its tokens, no forms, no popups, no top navigation) and carries its own
//! Content-Security-Policy ([`HTML_FRAME_CSP`]) on top of the page's: nothing is loaded from the
//! network, so opening a file never tells anyone that it was opened. Markdown goes through the
//! app's own renderer (`markdown.rs`, a tree of elements, no HTML string), PDF through the vendored
//! PDF.js with scripting off (`pdf-viewer.mjs`). docs/e2ee.md, "Previews of files".

use std::collections::HashSet;

use crate::markdown;

/// Files up to this size get a preview; larger ones are downloaded as before. A preview holds the
/// decrypted file and, for HTML and Markdown, its text in memory (docs/performance.md).
pub const PREVIEW_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// The policy of the document an HTML file is shown in: nothing at all is allowed except what the
/// file carries itself (`data:` and `blob:` images, inline styles). Script, fonts, frames, media,
/// forms, base URLs and connections are all blocked.
pub const HTML_FRAME_CSP: &str = "default-src 'none'; img-src data: blob:; style-src 'unsafe-inline'; \
  base-uri 'none'; form-action 'none'";

/// Links shown next to an HTML preview at most (a page can hold thousands).
pub const MAX_LISTED_LINKS: usize = 100;

/// The kinds of file with an in-app preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
  Markdown,
  Html,
  Pdf,
}

impl PreviewKind {
  /// What a file previews as, by its media type, else by its extension (senders' systems often
  /// hand over `application/octet-stream` or `text/plain` for `.md`). `None`: download only.
  pub fn of(media_type: &str, file_name: &str) -> Option<Self> {
    let essence = media_type
      .split(';')
      .next()
      .unwrap_or_default()
      .trim()
      .to_ascii_lowercase();
    match essence.as_str() {
      "application/pdf" => return Some(Self::Pdf),
      "text/html" | "application/xhtml+xml" => return Some(Self::Html),
      "text/markdown" | "text/x-markdown" => return Some(Self::Markdown),
      _ => {},
    }
    let is_unspecific = matches!(essence.as_str(), "" | "application/octet-stream" | "text/plain");
    let extension = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    match extension.as_str() {
      "pdf" if is_unspecific => Some(Self::Pdf),
      "html" | "htm" if is_unspecific => Some(Self::Html),
      "md" | "markdown" if is_unspecific => Some(Self::Markdown),
      _ => None,
    }
  }

  /// Whether the preview works from the file's text.
  pub fn is_text(self) -> bool {
    matches!(self, Self::Markdown | Self::Html)
  }
}

/// Whether a file of `size_bytes` is small enough to preview.
pub fn is_within_limit(size_bytes: u64) -> bool {
  size_bytes <= PREVIEW_MAX_BYTES
}

/// The file's text for previews: UTF-8, invalid bytes replaced, a byte order mark dropped.
pub fn text_of(bytes: &[u8]) -> String {
  let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
  String::from_utf8_lossy(bytes).into_owned()
}

/// The document an HTML file is shown in (`srcdoc` of the sandboxed frame): our policy comes first,
/// so it applies to everything the file adds after it. The file can't undo it — a second policy
/// only ever narrows — and has no script to rewrite the head.
pub fn html_frame_document(html: &str) -> String {
  format!(
    "<!doctype html><html><head><meta http-equiv=\"Content-Security-Policy\" content=\"{HTML_FRAME_CSP}\">\
     <meta charset=\"utf-8\"></head><body>{html}</body></html>"
  )
}

/// The `http(s)` links among `hrefs`, without repeats and in order of appearance, at most
/// [`MAX_LISTED_LINKS`]. They are opened by the system browser, never inside the frame.
pub fn listed_links<'a>(hrefs: impl IntoIterator<Item = &'a str>) -> Vec<String> {
  let mut seen = HashSet::new();
  hrefs
    .into_iter()
    .map(str::trim)
    .filter(|href| markdown::is_allowed_link(href) && !href.starts_with(markdown::PERMALINK_PREFIX))
    .filter(|href| seen.insert(*href))
    .take(MAX_LISTED_LINKS)
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  mod kinds {
    use super::*;

    #[test]
    fn media_type_decides() {
      assert_eq!(PreviewKind::of("application/pdf", "x"), Some(PreviewKind::Pdf));
      assert_eq!(
        PreviewKind::of("text/html; charset=utf-8", "x"),
        Some(PreviewKind::Html)
      );
      assert_eq!(PreviewKind::of("text/markdown", "x"), Some(PreviewKind::Markdown));
    }

    #[test]
    fn extension_decides_when_the_type_is_unspecific() {
      assert_eq!(
        PreviewKind::of("application/octet-stream", "Notes.MD"),
        Some(PreviewKind::Markdown)
      );
      assert_eq!(PreviewKind::of("", "page.htm"), Some(PreviewKind::Html));
      assert_eq!(PreviewKind::of("text/plain", "a.pdf"), Some(PreviewKind::Pdf));
    }

    #[test]
    fn a_specific_other_type_is_not_overruled_by_the_extension() {
      assert_eq!(PreviewKind::of("image/png", "evil.html"), None);
      assert_eq!(PreviewKind::of("application/zip", "a.md"), None);
      assert_eq!(PreviewKind::of("text/plain", "a.txt"), None);
      assert_eq!(PreviewKind::of("application/octet-stream", "noextension"), None);
    }
  }

  mod limit {
    use super::*;

    #[test]
    fn the_limit_itself_is_allowed() {
      assert!(is_within_limit(PREVIEW_MAX_BYTES));
      assert!(!is_within_limit(PREVIEW_MAX_BYTES + 1));
    }
  }

  mod text {
    use super::*;

    #[test]
    fn invalid_bytes_and_a_byte_order_mark_are_handled() {
      assert_eq!(text_of(b"\xEF\xBB\xBFhi"), "hi");
      assert_eq!(text_of(b"a\xFFb"), "a\u{FFFD}b");
    }
  }

  mod frame {
    use super::*;

    #[test]
    fn the_policy_comes_before_the_file() {
      let document = html_frame_document("<script>x()</script><p>hi</p>");
      let policy = document.find("Content-Security-Policy").expect("has a policy");
      let content = document.find("<script>").expect("has the file");
      assert!(policy < content);
    }

    #[test]
    fn the_policy_allows_no_script_and_no_network() {
      assert!(HTML_FRAME_CSP.starts_with("default-src 'none'"));
      for forbidden in [
        "script-src",
        "connect-src",
        "frame-src",
        "https:",
        "http:",
        "'self'",
      ] {
        assert!(!HTML_FRAME_CSP.contains(forbidden), "{forbidden}");
      }
      assert!(!HTML_FRAME_CSP.contains('"'), "must fit the content attribute");
    }
  }

  mod links {
    use super::*;

    #[test]
    fn only_http_links_once_each_in_order() {
      let links = listed_links([
        "https://example.com/a",
        "javascript:alert(1)",
        " http://example.com/b ",
        "https://example.com/a",
        "mailto:x@example.com",
        "/relative",
        "data:text/html,x",
      ]);
      assert_eq!(links, ["https://example.com/a", "http://example.com/b"]);
    }

    #[test]
    fn the_list_is_bounded() {
      let hrefs: Vec<String> = (0..MAX_LISTED_LINKS + 20)
        .map(|n| format!("https://example.com/{n}"))
        .collect();
      assert_eq!(
        listed_links(hrefs.iter().map(String::as_str)).len(),
        MAX_LISTED_LINKS
      );
    }
  }
}
