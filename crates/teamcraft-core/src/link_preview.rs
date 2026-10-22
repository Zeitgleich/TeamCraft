//! Link previews (e2ee.md §12): the sender's client fetches the page and puts title, description
//! and a small image into the encrypted message. The domain list of the signed workspace policy is
//! enforced by senders (no preview for disallowed domains) and receivers (disallowed previews are
//! dropped), because the server can't see the URL.
//!
//! This module holds the pure parts: finding the URL, the policy check and reading the page's
//! metadata. Fetching is in [`crate::client::previews`].

use std::future::Future;
use std::pin::Pin;

use teamcraft_proto::{LinkPreviewMode, LinkPreviewPolicy};
use url::Url;

/// Longest title and description kept (characters).
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_DESCRIPTION_CHARS: usize = 400;

/// What a page says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMeta {
  pub maybe_title: Option<String>,
  pub maybe_description: Option<String>,
  pub maybe_site_name: Option<String>,
  pub maybe_image_url: Option<Url>,
}

/// A fetched resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
  /// After redirects.
  pub final_url: Url,
  pub content_type: String,
  pub body: Vec<u8>,
}

/// Fetches pages and images for previews; an implementation enforces timeouts and `max_bytes`.
pub trait PreviewFetcher {
  fn fetch(&self, url: Url, max_bytes: usize) -> Pin<Box<dyn Future<Output = Option<Fetched>> + '_>>;
}

/// The first http(s) link in a message text.
pub fn first_url(text: &str) -> Option<Url> {
  text
    .split(|character: char| character.is_whitespace() || matches!(character, '<' | '>' | '"'))
    .filter_map(|word| {
      word
        .find("https://")
        .or_else(|| word.find("http://"))
        .map(|start| &word[start..])
    })
    .map(|word| word.trim_end_matches(['.', ',', ')', ']', '!', '?', ';', ':', '\'']))
    .find_map(|word| Url::parse(word).ok())
    .filter(|url| url.host_str().is_some())
}

/// Whether the policy allows previews of links to `url` (sender and receiver both check).
pub fn is_allowed(policy: &LinkPreviewPolicy, url: &Url) -> bool {
  let Some(host) = url.host_str() else {
    return false;
  };
  let host = host.trim_end_matches('.').to_ascii_lowercase();
  let is_listed = policy.domains.iter().any(|domain| {
    let domain = domain
      .trim()
      .trim_start_matches("*.")
      .trim_end_matches('.')
      .to_ascii_lowercase();
    !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
  });
  match policy.mode {
    LinkPreviewMode::AllowList => is_listed,
    LinkPreviewMode::BlockList => !is_listed,
    // Off, and modes from newer policies this client doesn't know: no previews.
    _ => false,
  }
}

/// Reads OpenGraph tags (falling back to `<title>` and `<meta name="description">`) from HTML.
/// A small scanner instead of an HTML parser: only `<meta>` and `<title>` matter, and only the
/// document's head is read (callers cap the size).
pub fn parse_page(html: &str, base: &Url) -> PageMeta {
  let mut meta = PageMeta::default();
  let mut maybe_plain_description = None;
  let mut rest = html;
  while let Some(start) = rest.find('<') {
    rest = &rest[start + 1..];
    let Some(end) = rest.find('>') else {
      break;
    };
    let tag = &rest[..end];
    rest = &rest[end + 1..];
    let name: String = tag
      .chars()
      .take_while(|character| character.is_ascii_alphanumeric())
      .collect::<String>()
      .to_ascii_lowercase();
    match name.as_str() {
      "meta" => {
        let attributes = attributes(tag);
        let key = attribute(&attributes, "property").or_else(|| attribute(&attributes, "name"));
        let Some(content) = attribute(&attributes, "content") else {
          continue;
        };
        match key.map(str::to_ascii_lowercase).as_deref() {
          Some("og:title") => meta.maybe_title = Some(clean(content, MAX_TITLE_CHARS)),
          Some("og:description") => meta.maybe_description = Some(clean(content, MAX_DESCRIPTION_CHARS)),
          Some("og:site_name") => meta.maybe_site_name = Some(clean(content, MAX_TITLE_CHARS)),
          Some("og:image") | Some("og:image:url") => {
            meta.maybe_image_url = base.join(&decode_entities(content)).ok().filter(is_web_url);
          },
          Some("description") => maybe_plain_description = Some(clean(content, MAX_DESCRIPTION_CHARS)),
          _ => {},
        }
      },
      "title" if meta.maybe_title.is_none() => {
        if let Some(close) = rest.to_ascii_lowercase().find("</title") {
          meta.maybe_title = Some(clean(&rest[..close], MAX_TITLE_CHARS));
        }
      },
      "body" => break,
      _ => {},
    }
  }
  if meta.maybe_description.is_none() {
    meta.maybe_description = maybe_plain_description;
  }
  meta.maybe_title = meta.maybe_title.filter(|title| !title.is_empty());
  meta.maybe_description = meta
    .maybe_description
    .filter(|description| !description.is_empty());
  meta
}

fn is_web_url(url: &Url) -> bool {
  matches!(url.scheme(), "http" | "https")
}

/// `name="value"`, `name='value'` and `name=value` pairs of a tag.
fn attributes(tag: &str) -> Vec<(String, String)> {
  let mut pairs = Vec::new();
  let mut chars = tag.char_indices().peekable();
  // Skip the tag name.
  while chars
    .peek()
    .is_some_and(|(_, character)| !character.is_whitespace())
  {
    chars.next();
  }
  loop {
    while chars
      .peek()
      .is_some_and(|(_, character)| character.is_whitespace() || *character == '/')
    {
      chars.next();
    }
    let Some(&(name_start, _)) = chars.peek() else {
      break;
    };
    let mut name_end = name_start;
    while let Some(&(index, character)) = chars.peek() {
      if character == '=' || character.is_whitespace() {
        break;
      }
      name_end = index + character.len_utf8();
      chars.next();
    }
    let name = tag[name_start..name_end].to_ascii_lowercase();
    if chars.peek().map(|(_, character)| *character) != Some('=') {
      if name.is_empty() {
        chars.next();
      }
      continue;
    }
    chars.next();
    let value = match chars.peek().map(|(_, character)| *character) {
      Some(quote @ ('"' | '\'')) => {
        chars.next();
        let start = chars.peek().map_or(tag.len(), |(index, _)| *index);
        let mut end = tag.len();
        for (index, character) in chars.by_ref() {
          if character == quote {
            end = index;
            break;
          }
        }
        &tag[start.min(end)..end]
      },
      _ => {
        let start = chars.peek().map_or(tag.len(), |(index, _)| *index);
        let mut end = tag.len();
        while let Some(&(index, character)) = chars.peek() {
          if character.is_whitespace() {
            end = index;
            break;
          }
          chars.next();
        }
        &tag[start.min(end)..end]
      },
    };
    pairs.push((name, value.to_string()));
  }
  pairs
}

fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
  attributes
    .iter()
    .find(|(key, _)| key == name)
    .map(|(_, value)| value.as_str())
}

/// Entities decoded, whitespace collapsed, cut to `max_chars`.
fn clean(text: &str, max_chars: usize) -> String {
  let decoded = decode_entities(text);
  let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
  match collapsed.char_indices().nth(max_chars) {
    Some((cut, _)) => format!("{}…", &collapsed[..cut]),
    None => collapsed,
  }
}

fn decode_entities(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(start) = rest.find('&') {
    out.push_str(&rest[..start]);
    rest = &rest[start..];
    let Some(end) = rest[..rest.len().min(12)].find(';') else {
      out.push('&');
      rest = &rest[1..];
      continue;
    };
    let entity = &rest[1..end];
    let maybe_decoded = match entity {
      "amp" => Some('&'),
      "lt" => Some('<'),
      "gt" => Some('>'),
      "quot" => Some('"'),
      "apos" | "#39" => Some('\''),
      "nbsp" => Some(' '),
      _ => entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .or_else(|| entity.strip_prefix('#').and_then(|decimal| decimal.parse().ok()))
        .and_then(char::from_u32),
    };
    match maybe_decoded {
      Some(character) => {
        out.push(character);
        rest = &rest[end + 1..];
      },
      None => {
        out.push('&');
        rest = &rest[1..];
      },
    }
  }
  out.push_str(rest);
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn finds_the_first_web_link() {
    assert_eq!(
      first_url("see (https://example.com/page?a=1), thanks").map(String::from),
      Some("https://example.com/page?a=1".to_string())
    );
    assert_eq!(first_url("ftp://example.com and mailto:x"), None);
    assert_eq!(first_url("nothing here"), None);
  }

  #[test]
  fn policy_lists_match_domains_and_subdomains() {
    let allow = LinkPreviewPolicy {
      mode: LinkPreviewMode::AllowList,
      domains: vec!["example.com".into()],
    };
    let url = |text: &str| Url::parse(text).unwrap();
    assert!(is_allowed(&allow, &url("https://docs.example.com/x")));
    assert!(is_allowed(&allow, &url("https://EXAMPLE.com.")));
    assert!(!is_allowed(&allow, &url("https://notexample.com")));
    let block = LinkPreviewPolicy {
      mode: LinkPreviewMode::BlockList,
      domains: vec!["*.tracker.example".into()],
    };
    assert!(!is_allowed(&block, &url("https://a.tracker.example")));
    assert!(is_allowed(&block, &url("https://example.org")));
    let off = LinkPreviewPolicy {
      mode: LinkPreviewMode::Off,
      domains: Vec::new(),
    };
    assert!(!is_allowed(&off, &url("https://example.org")));
  }

  #[test]
  fn reads_open_graph_with_fallbacks_and_entities() {
    let base = Url::parse("https://example.com/articles/1").unwrap();
    let html = r#"<!doctype html><html><head>
      <title>Plain   title</title>
      <meta name="description" content="plain description">
      <meta property='og:title' content="Tom &amp; Jerry &#8211; &quot;The&quot; Story">
      <meta property=og:site_name content=Example>
      <meta content="/img/cover.png" property="og:image" />
      </head><body><meta property="og:description" content="ignored after body"></body></html>"#;
    let meta = parse_page(html, &base);
    assert_eq!(meta.maybe_title.as_deref(), Some("Tom & Jerry – \"The\" Story"));
    assert_eq!(meta.maybe_description.as_deref(), Some("plain description"));
    assert_eq!(meta.maybe_site_name.as_deref(), Some("Example"));
    assert_eq!(
      meta.maybe_image_url.map(String::from).as_deref(),
      Some("https://example.com/img/cover.png")
    );

    let fallback = parse_page("<title>Only a title</title>", &base);
    assert_eq!(fallback.maybe_title.as_deref(), Some("Only a title"));
    // Hostile or broken markup doesn't panic.
    for broken in [
      "<meta content=",
      "<meta property=\"og:title\" content=\"x",
      "<<<>>>&#xFFFFFFFF;",
      "&&&;",
      "<title>",
    ] {
      let _ = parse_page(broken, &base);
    }
    assert_eq!(
      parse_page(
        r#"<meta property="og:image" content="javascript:alert(1)">"#,
        &base
      )
      .maybe_image_url,
      None
    );
  }
}
