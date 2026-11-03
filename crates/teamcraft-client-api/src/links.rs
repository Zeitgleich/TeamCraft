//! Which links a client opens from content other members control (bookmarks): only web pages.
//! Other schemes (`tel:`, `content:`, `file:`, app deep links) would let a member trigger actions
//! in other apps with one tap.
//!
//! [`is_external_link`] is the wider list the desktop shell hands to the system: web pages and mail
//! addresses. Both the UI (which routes clicks) and the shell (which opens) use it, so the two
//! can't drift apart.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MessageId, UserId};

use crate::actions::LinkPreview;
use crate::backend::ChatBackend;
use crate::model::{MessageKind, TimestampMs};

/// Links the "Files & links" tab loads at first and with each "Show more".
pub const LINKS_PER_PAGE: usize = 30;
/// Messages read from the backend per step while looking for links, newest first.
const SCAN_CHUNK: usize = 200;
/// The most links one URL list of a message contributes (a message pasting a list of links).
const MAX_LINKS_PER_MESSAGE: usize = 10;

/// One link shared in a conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkInfo {
  pub url: String,
  pub conversation: ConversationId,
  /// The message that contains it.
  pub message: MessageId,
  pub sender: UserId,
  pub sent_at: TimestampMs,
  /// The sender's preview of the link, when the message carries one for this URL.
  #[serde(default)]
  pub maybe_preview: Option<LinkPreview>,
}

/// The newest links of a conversation, newest first.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LinkPage {
  pub links: Vec<LinkInfo>,
  /// Whether older links exist beyond these.
  pub has_more: bool,
}

/// The web links in a message body, in order, without duplicates: `http(s)://` up to whitespace,
/// without the punctuation or closing bracket that ends a sentence around the link.
pub fn extract_urls(body: &str) -> Vec<String> {
  let mut urls: Vec<String> = Vec::new();
  for word in body.split(|character: char| character.is_whitespace() || matches!(character, '<' | '>' | '"'))
  {
    let Some(start) = ["https://", "http://"]
      .iter()
      .filter_map(|scheme| find_scheme(word, scheme))
      .min()
    else {
      continue;
    };
    let url = trim_url_end(&word[start..]);
    if url.len() > "https://".len() && is_openable_link(url) && !urls.iter().any(|known| known == url) {
      urls.push(url.to_owned());
    }
  }
  urls
}

fn find_scheme(word: &str, scheme: &str) -> Option<usize> {
  word.to_ascii_lowercase().find(scheme)
}

/// Drops sentence punctuation and an unbalanced closing bracket from the end of a URL.
fn trim_url_end(url: &str) -> &str {
  let mut end = url.len();
  while let Some(last) = url[..end].chars().next_back() {
    let is_trailing = match last {
      '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' | '_' => true,
      ')' => url[..end].matches('(').count() < url[..end].matches(')').count(),
      ']' => url[..end].matches('[').count() < url[..end].matches(']').count(),
      _ => false,
    };
    if !is_trailing {
      break;
    }
    end -= last.len_utf8();
  }
  &url[..end]
}

/// The newest `limit` links of a conversation. Reads the messages newest first in chunks and stops
/// once `limit` and one more link are found, so a long conversation is never read whole to show
/// its latest links; "Show more" asks again with a larger limit.
pub fn links_page(
  backend: &(impl ChatBackend + ?Sized),
  conversation: ConversationId,
  limit: usize,
) -> LinkPage {
  let wanted = limit.saturating_add(1);
  let mut found: Vec<LinkInfo> = Vec::new();
  let mut end = backend.message_count(conversation);
  while end > 0 && found.len() < wanted {
    let start = end.saturating_sub(SCAN_CHUNK);
    let mut chunk = backend.messages(conversation, start, end - start);
    end = start;
    chunk.reverse();
    let with_links: Vec<_> = chunk
      .into_iter()
      .filter(|message| matches!(message.kind, MessageKind::User))
      .map(|message| {
        let urls = extract_urls(&message.body);
        (message, urls)
      })
      .filter(|(_, urls)| !urls.is_empty())
      .collect();
    if with_links.is_empty() {
      continue;
    }
    let ids: Vec<MessageId> = with_links.iter().map(|(message, _)| message.id).collect();
    let extras = backend.message_extras(conversation, &ids);
    for (message, urls) in with_links {
      let extra = extras.iter().find(|extra| extra.message == message.id);
      // A deleted message keeps no links.
      if extra.is_some_and(|extra| extra.is_deleted) {
        continue;
      }
      for url in urls.into_iter().take(MAX_LINKS_PER_MESSAGE) {
        let maybe_preview = extra
          .and_then(|extra| extra.maybe_link_preview.clone())
          .filter(|preview| preview.url == url);
        found.push(LinkInfo {
          url,
          conversation,
          message: message.id,
          sender: message.author,
          sent_at: message.sent_at,
          maybe_preview,
        });
      }
    }
  }
  let has_more = found.len() > limit;
  found.truncate(limit);
  LinkPage {
    links: found,
    has_more,
  }
}

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
