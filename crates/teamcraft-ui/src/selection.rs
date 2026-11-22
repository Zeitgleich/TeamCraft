//! Selecting several messages of one conversation (issue #2070): the selection's state and the
//! pure rules around it, so forward, copy and delete act on the same, ordered list of messages.

use std::collections::HashSet;

use crate::model::{ConversationId, Message, MessageId, MessageKind, TimestampMs, UserId};

/// The messages the user picked in selection mode. Selection mode is on while `maybe_conversation`
/// is set; it belongs to that one conversation and ends when the user leaves it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MessageSelection {
  maybe_conversation: Option<ConversationId>,
  ids: HashSet<MessageId>,
  /// The message a Shift+click range starts from: the last one toggled on.
  maybe_anchor: Option<MessageId>,
}

/// What the delete button of a selection does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteAccess {
  /// Every selected message may be deleted by this user.
  Allowed,
  /// At least one may not (the author is somebody else, or posting is off).
  Refused,
}

impl MessageSelection {
  /// Starts selection mode in `conversation` with `message` selected.
  pub fn start(conversation: ConversationId, message: MessageId) -> Self {
    Self {
      maybe_conversation: Some(conversation),
      ids: HashSet::from([message]),
      maybe_anchor: Some(message),
    }
  }

  pub fn is_active(&self) -> bool {
    self.maybe_conversation.is_some()
  }

  pub fn is_active_in(&self, conversation: ConversationId) -> bool {
    self.maybe_conversation == Some(conversation)
  }

  pub fn is_selected(&self, message: MessageId) -> bool {
    self.ids.contains(&message)
  }

  pub fn len(&self) -> usize {
    self.ids.len()
  }

  pub fn is_empty(&self) -> bool {
    self.ids.is_empty()
  }

  pub fn ids(&self) -> &HashSet<MessageId> {
    &self.ids
  }

  pub fn maybe_anchor(&self) -> Option<MessageId> {
    self.maybe_anchor
  }

  /// Selects the message, or deselects it when it is. The last message of the selection stays:
  /// leaving the mode is Cancel's job, not an emptied list's.
  pub fn toggle(&mut self, message: MessageId) {
    if self.ids.remove(&message) {
      if self.maybe_anchor == Some(message) {
        self.maybe_anchor = None;
      }
    } else {
      self.ids.insert(message);
      self.maybe_anchor = Some(message);
    }
  }

  /// Adds the messages between the anchor and `to` in `feed_order` (Shift+click). Without an anchor
  /// (or one that left the feed) `to` alone is added.
  pub fn extend_to(&mut self, to: MessageId, feed_order: &[MessageId]) {
    let range = self
      .maybe_anchor
      .map(|anchor| range_between(feed_order, anchor, to))
      .unwrap_or_default();
    if range.is_empty() {
      self.ids.insert(to);
    } else {
      self.ids.extend(range);
    }
    self.maybe_anchor = Some(to);
  }

  /// Drops the messages for which `is_gone` says so (deleted meanwhile).
  pub fn forget(&mut self, is_gone: impl Fn(MessageId) -> bool) {
    self.ids.retain(|id| !is_gone(*id));
    if self
      .maybe_anchor
      .is_some_and(|anchor| !self.ids.contains(&anchor))
    {
      self.maybe_anchor = None;
    }
  }

  /// Leaves selection mode.
  pub fn clear(&mut self) {
    *self = Self::default();
  }

  /// The selected messages of `feed`, oldest first: the order they are forwarded and copied in.
  pub fn in_order<'a>(&self, feed: &'a [Message]) -> Vec<&'a Message> {
    let mut picked: Vec<&Message> = feed
      .iter()
      .filter(|message| self.ids.contains(&message.id) && message.kind == MessageKind::User)
      .collect();
    picked.sort_by_key(|message| (message.sent_at, message.id));
    picked
  }
}

/// Whether `me` may delete every one of `messages`: only their own, and only where posting is on
/// (the rule of a single message's Delete in `message_actions`).
pub fn delete_access(messages: &[&Message], me: UserId, can_post: bool) -> DeleteAccess {
  if can_post
    && !messages.is_empty()
    && messages
      .iter()
      .all(|message| message.author == me && message.maybe_imported.is_none())
  {
    DeleteAccess::Allowed
  } else {
    DeleteAccess::Refused
  }
}

/// The text the Copy button puts on the clipboard: per message the sender and time on a line, then
/// the text, with a blank line between messages.
pub fn copy_text(
  messages: &[&Message],
  name_of: impl Fn(UserId) -> String,
  time_of: impl Fn(TimestampMs) -> String,
) -> String {
  messages
    .iter()
    .map(|message| {
      format!(
        "{}, {}\n{}",
        name_of(message.author),
        time_of(message.sent_at),
        message.body
      )
    })
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// The messages from `from` to `to` in `order`, both included, whichever comes first. Empty when
/// either is not in `order`.
pub fn range_between(order: &[MessageId], from: MessageId, to: MessageId) -> Vec<MessageId> {
  let (Some(a), Some(b)) = (
    order.iter().position(|id| *id == from),
    order.iter().position(|id| *id == to),
  ) else {
    return Vec::new();
  };
  order[a.min(b)..=a.max(b)].to_vec()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  mod toggling {
    use super::*;

    #[test]
    fn starting_selects_the_message() {
      let selection = MessageSelection::start(id_from_raw(1), id_from_raw(10));
      assert!(selection.is_active_in(id_from_raw(1)));
      assert!(!selection.is_active_in(id_from_raw(2)));
      assert!(selection.is_selected(id_from_raw(10)));
      assert_eq!(selection.len(), 1);
    }

    #[test]
    fn toggling_twice_deselects_but_keeps_the_mode() {
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(10));
      selection.toggle(id_from_raw(11));
      selection.toggle(id_from_raw(11));
      assert!(!selection.is_selected(id_from_raw(11)));
      selection.toggle(id_from_raw(10));
      assert!(selection.is_empty());
      assert!(selection.is_active());
    }

    #[test]
    fn clearing_leaves_the_mode() {
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(10));
      selection.clear();
      assert!(!selection.is_active());
      assert!(selection.is_empty());
    }
  }

  mod ranges {
    use super::*;

    fn feed() -> Vec<MessageId> {
      (1..=6).map(id_from_raw).collect()
    }

    #[test]
    fn a_range_includes_both_ends_in_either_direction() {
      assert_eq!(
        range_between(&feed(), id_from_raw(2), id_from_raw(4)),
        [2, 3, 4].map(id_from_raw)
      );
      assert_eq!(
        range_between(&feed(), id_from_raw(4), id_from_raw(2)),
        [2, 3, 4].map(id_from_raw)
      );
    }

    #[test]
    fn a_range_with_an_unknown_end_is_empty() {
      assert!(range_between(&feed(), id_from_raw(2), id_from_raw(99)).is_empty());
    }

    #[test]
    fn extending_adds_everything_from_the_anchor() {
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(2));
      selection.extend_to(id_from_raw(5), &feed());
      assert_eq!(selection.len(), 4);
      assert_eq!(selection.maybe_anchor(), Some(id_from_raw(5)));
    }

    #[test]
    fn extending_without_an_anchor_adds_just_the_target() {
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(2));
      selection.toggle(id_from_raw(2));
      selection.extend_to(id_from_raw(5), &feed());
      assert_eq!(selection.len(), 1);
      assert!(selection.is_selected(id_from_raw(5)));
    }
  }

  mod consistency {
    use super::*;

    #[test]
    fn deleted_messages_drop_out_and_take_the_anchor() {
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(2));
      selection.toggle(id_from_raw(3));
      selection.forget(|id| id == id_from_raw(3));
      assert!(!selection.is_selected(id_from_raw(3)));
      assert!(selection.is_selected(id_from_raw(2)));
      assert_eq!(selection.maybe_anchor(), None);
    }
  }

  mod ordering_and_delete {
    use super::*;

    #[test]
    fn selected_messages_come_oldest_first_and_only_user_messages() {
      let feed = vec![message(3, 1, 300), message(1, 1, 100), message(2, 2, 200)];
      let mut selection = MessageSelection::start(id_from_raw(1), id_from_raw(3));
      selection.toggle(id_from_raw(1));
      selection.toggle(id_from_raw(9));
      let ids: Vec<_> = selection
        .in_order(&feed)
        .iter()
        .map(|message| message.id)
        .collect();
      assert_eq!(ids, [1, 3].map(id_from_raw));
    }

    #[test]
    fn copied_text_names_sender_and_time_per_message() {
      let (first, second) = (message(1, 7, 100), message(2, 8, 200));
      let text = copy_text(
        &[&first, &second],
        |user| {
          if user == id_from_raw(7) {
            "Ada".to_owned()
          } else {
            "Bo".to_owned()
          }
        },
        |time| format!("t{time}"),
      );
      assert_eq!(text, "Ada, t100\nmessage 1\n\nBo, t200\nmessage 2");
    }

    #[test]
    fn delete_needs_every_message_to_be_the_users_own() {
      let own = message(1, 7, 100);
      let other = message(2, 8, 200);
      let me = id_from_raw(7);
      assert_eq!(delete_access(&[&own], me, true), DeleteAccess::Allowed);
      assert_eq!(delete_access(&[&own, &other], me, true), DeleteAccess::Refused);
      assert_eq!(delete_access(&[&own], me, false), DeleteAccess::Refused);
      assert_eq!(delete_access(&[], me, true), DeleteAccess::Refused);
    }
  }

  fn message(id: u64, author: u64, sent_at: i64) -> Message {
    Message {
      maybe_sticker: None,
      id: id_from_raw(id),
      conversation: id_from_raw(1),
      author: id_from_raw(author),
      kind: MessageKind::User,
      sent_at,
      body: format!("message {id}"),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
      maybe_imported: None,
      maybe_source: None,
      maybe_placeholder: None,
      group_mentions: Vec::new(),
      custom_emoji: Vec::new(),
      mentions_me: false,
      maybe_poll: None,
    }
  }
}
