//! The web links in a message body, by one rule for every client and store: the Files & links tab
//! lists them, a local store flags the messages that have them (issue #609), and a client opens only
//! these (bookmarks and links in content other members control).

use std::collections::HashSet;

/// The schemes of a web link.
const OPENABLE_SCHEMES: [&str; 2] = ["https://", "http://"];

/// Whether `url` is a web link a client may open.
pub fn is_openable_link(url: &str) -> bool {
  OPENABLE_SCHEMES.iter().any(|scheme| {
    url
      .get(..scheme.len())
      .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
  })
}

/// The web links in a message body, in order, without duplicates: `http(s)://` up to whitespace,
/// without the punctuation or closing bracket that ends a sentence around the link.
pub fn extract_urls(body: &str) -> Vec<String> {
  let mut seen: HashSet<&str> = HashSet::new();
  web_links(body)
    .filter(|url| seen.insert(url))
    .map(str::to_owned)
    .collect()
}

/// Whether a message body has a web link ([`extract_urls`] finds one). Asked for every message a
/// store writes, so it stops at the first link and allocates only for words with a scheme.
pub fn has_web_link(body: &str) -> bool {
  web_links(body).next().is_some()
}

/// The web links in a body, in order, with duplicates.
fn web_links(body: &str) -> impl Iterator<Item = &str> {
  body
    .split(|character: char| character.is_whitespace() || matches!(character, '<' | '>' | '"'))
    // Every scheme has it: a word without it is no link, and isn't lowercased to look.
    .filter(|word| word.contains("://"))
    .filter_map(|word| {
      let start = OPENABLE_SCHEMES
        .iter()
        .filter_map(|scheme| find_scheme(word, scheme))
        .min()?;
      let url = trim_url_end(&word[start..]);
      (url.len() > "https://".len() && is_openable_link(url)).then_some(url)
    })
}

fn find_scheme(word: &str, scheme: &str) -> Option<usize> {
  word.to_ascii_lowercase().find(scheme)
}

/// Drops sentence punctuation and an unbalanced closing bracket from the end of a URL. The brackets
/// are counted once, so a hostile run of them costs one pass, not one per trimmed character.
fn trim_url_end(url: &str) -> &str {
  let count = |character: char| url.chars().filter(|known| *known == character).count();
  let (open_round, mut close_round) = (count('('), count(')'));
  let (open_square, mut close_square) = (count('['), count(']'));
  let mut end = url.len();
  while let Some(last) = url[..end].chars().next_back() {
    let is_trailing = match last {
      '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' => true,
      ')' if open_round < close_round => {
        close_round -= 1;
        true
      },
      ']' if open_square < close_square => {
        close_square -= 1;
        true
      },
      _ => false,
    };
    if !is_trailing {
      break;
    }
    end -= last.len_utf8();
  }
  &url[..end]
}

#[cfg(test)]
mod tests {
  use super::*;

  mod extraction {
    use super::*;

    #[test]
    fn links_are_found_without_sentence_punctuation() {
      assert_eq!(
        extract_urls("See https://example.com/a, and (http://example.org/b). Done!"),
        ["https://example.com/a", "http://example.org/b"]
      );
    }

    #[test]
    fn a_bracket_that_belongs_to_the_url_stays() {
      assert_eq!(
        extract_urls("https://example.com/wiki/Rust_(language)"),
        ["https://example.com/wiki/Rust_(language)"]
      );
    }

    #[test]
    fn duplicates_and_other_schemes_are_dropped() {
      assert_eq!(
        extract_urls("https://example.com/a https://example.com/a ftp://example.com/x javascript:alert(1)"),
        ["https://example.com/a"]
      );
    }

    #[test]
    fn markdown_links_and_angle_brackets_give_the_url() {
      assert_eq!(
        extract_urls("[docs](https://example.com/docs) and <https://example.com/b>"),
        ["https://example.com/docs", "https://example.com/b"]
      );
    }

    #[test]
    fn a_bare_scheme_is_no_link() {
      assert!(extract_urls("https:// and http://").is_empty());
      assert!(!has_web_link("https:// and http://"));
    }

    /// A hostile message (a run of brackets as long as a message may be) costs one pass; counting
    /// them per trimmed character took hours.
    #[test]
    fn a_long_run_of_brackets_is_trimmed_in_one_pass() {
      let body = format!("https://a({}", ")".repeat(1 << 20));
      assert_eq!(extract_urls(&body), ["https://a()"]);
    }

    #[test]
    fn many_links_are_deduplicated_in_order() {
      let body: String = (0..20_000)
        .map(|n| format!("https://example.com/{} ", n % 10_000))
        .collect();
      let urls = extract_urls(&body);
      assert_eq!(urls.len(), 10_000);
      assert_eq!(urls[1], "https://example.com/1");
    }

    #[test]
    fn has_web_link_agrees_with_extraction() {
      for body in [
        "",
        "no link here",
        "See https://example.com/a.",
        "HTTPS://EXAMPLE.COM/upper",
        "ftp://example.com/x javascript:alert(1)",
        "mailto:a@example.com",
        "[docs](https://example.com/docs)",
        "text://example.com https://",
      ] {
        assert_eq!(has_web_link(body), !extract_urls(body).is_empty(), "{body}");
      }
    }
  }

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
