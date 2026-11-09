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

pub use teamcraft_proto::web_links::{extract_urls, is_openable_link};

/// Links the "Files & links" tab loads at first and with each "Show more".
pub const LINKS_PER_PAGE: usize = 30;
/// The most links one read returns, whatever it asks for: "Show more" stops growing the page there.
pub const MAX_LINKS_PER_READ: usize = 1_000;
/// Messages read from the backend per step while looking for links, newest first.
const SCAN_CHUNK: usize = 200;
/// The most links one URL list of a message contributes (a message pasting a list of links).
pub const MAX_LINKS_PER_MESSAGE: usize = 10;

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

/// The message a link was shared in.
#[derive(Clone, Copy, Debug)]
pub struct LinkSource {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub sender: UserId,
  pub sent_at: TimestampMs,
}

/// The newest `limit` links of a conversation. Reads the messages newest first in chunks and stops
/// once `limit` and one more link are found, so a long conversation is never read whole to show
/// its latest links; "Show more" asks again with a larger limit.
pub fn links_page(
  backend: &(impl ChatBackend + ?Sized),
  conversation: ConversationId,
  limit: usize,
) -> LinkPage {
  let limit = limit.min(MAX_LINKS_PER_READ);
  let wanted = limit + 1;
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
      let maybe_preview = extra.and_then(|extra| extra.maybe_link_preview.as_ref());
      let source = LinkSource {
        conversation,
        message: message.id,
        sender: message.author,
        sent_at: message.sent_at,
      };
      found.extend(source.links(urls, maybe_preview));
    }
  }
  LinkPage::of_found(found, limit)
}

impl LinkSource {
  /// The links of the message: its URLs ([`extract_urls`]), at most [`MAX_LINKS_PER_MESSAGE`],
  /// each with the sender's preview when it is of that URL.
  pub fn links<'a>(
    self,
    urls: Vec<String>,
    maybe_preview: Option<&'a LinkPreview>,
  ) -> impl Iterator<Item = LinkInfo> + 'a {
    urls
      .into_iter()
      .take(MAX_LINKS_PER_MESSAGE)
      .map(move |url| LinkInfo {
        // Only the preview of this URL is copied (its image included).
        maybe_preview: maybe_preview.filter(|preview| preview.url == url).cloned(),
        url,
        conversation: self.conversation,
        message: self.message,
        sender: self.sender,
        sent_at: self.sent_at,
      })
  }
}

impl LinkPage {
  /// The page of the first `limit` of `found` (newest first); one more found means more exist.
  pub fn of_found(mut found: Vec<LinkInfo>, limit: usize) -> Self {
    let has_more = found.len() > limit;
    found.truncate(limit);
    Self {
      links: found,
      has_more,
    }
  }
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
