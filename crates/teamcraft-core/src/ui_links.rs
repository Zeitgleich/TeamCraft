//! The Files & links tab's links from the store's index of messages with links (issue #609).
//!
//! [`LocalStore::attached_messages`] lists only messages the channel shows whose text has a web
//! link, newest first, so a page reads the messages it shows, never the history in between: a long
//! conversation with few links costs as little as a short one. Links are content:
//! nothing here logs them.

use teamcraft_client_api::links::{LinkPage, LinkSource, MAX_LINKS_PER_READ, extract_urls};
use teamcraft_client_api::model::{ConversationId, MessageId};

use crate::imported::Authorship;
use crate::store::{Attached, AttachedRange, LocalStore, MessageContent};
use crate::ui_actions::shown_link_preview;
use crate::ui_backend::{CoreChatBackend, ui_message_id};

impl<S: LocalStore> CoreChatBackend<S> {
  /// The newest `limit` links of a conversation (at most [`MAX_LINKS_PER_READ`]), newest first: one
  /// more tells whether there are more. Every message the index lists has a link, so one read is
  /// enough; it reads again from where it stopped only when a message turns out to have none.
  pub(crate) fn ui_conversation_links(&self, conversation: ConversationId, limit: usize) -> LinkPage {
    let limit = limit.min(MAX_LINKS_PER_READ);
    let wanted = limit + 1;
    let policy = self.verified_policy().link_previews;
    let maybe_importer = self.importer();
    let mut found = Vec::new();
    let mut maybe_from: Option<MessageId> = None;
    while found.len() < wanted {
      let asked = wanted - found.len();
      let messages = self.attached_messages(
        conversation,
        Attached::Link,
        AttachedRange::Older(maybe_from),
        asked,
      );
      let is_last = messages.len() < asked;
      maybe_from = messages.last().and_then(|message| message.maybe_server_id);
      for stored in &messages {
        let MessageContent::Text { body, meta, .. } = &stored.content else {
          continue;
        };
        let authorship = Authorship::of(stored, maybe_importer);
        let source = LinkSource {
          conversation,
          message: ui_message_id(stored),
          sender: authorship.author,
          sent_at: authorship.sent_at_ms,
        };
        let maybe_preview = shown_link_preview(meta, &policy);
        found.extend(source.links(extract_urls(body), maybe_preview.as_ref()));
      }
      if is_last || maybe_from.is_none() {
        break;
      }
    }
    LinkPage::of_found(found, limit)
  }
}
