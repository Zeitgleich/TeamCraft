//! How a row of the message feed enters (issue #273): a message that arrives while the conversation
//! is open rises in, the user's own message settles when the server confirmed it, and a reaction
//! pops when its count changed. Rows of the virtualized feed are rebuilt when they scroll back into
//! view or their message changes, so this decides from what a row showed before and what already
//! moved, never from the clock.

use std::collections::HashSet;

use crate::model::{Message, MessageId, is_pending};

/// Messages remembered as having played their entrance. Old ones are forgotten past this; they
/// would only rise in again after the reader scrolled far away and back.
const MAX_REMEMBERED: usize = 512;

/// What a row showed the last time it rendered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
  pub id: MessageId,
  pub reactions: Vec<(String, u32)>,
}

impl Shown {
  pub fn of(message: &Message) -> Self {
    Self {
      id: message.id,
      reactions: message
        .reactions
        .iter()
        .map(|reaction| (reaction.emoji.clone(), reaction.count))
        .collect(),
    }
  }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Entrance {
  #[default]
  None,
  /// New since the conversation was opened: rises in.
  Arriving,
  /// The user's own message, now confirmed by the server: its dimmed state fades to normal.
  Settling,
}

/// How one render of a row moves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMotion {
  pub entrance: Entrance,
  /// Emoji whose reaction is new or changed its count since the row last rendered.
  pub popping: Vec<String>,
}

/// What the feed of one open conversation has seen: the newest message when it opened (older ones
/// never count as arriving) and the messages that already played their entrance.
#[derive(Debug, Default)]
pub struct FeedMotion {
  maybe_baseline: Option<MessageId>,
  moved: HashSet<MessageId>,
}

impl FeedMotion {
  /// `maybe_newest` is the newest confirmed message when the feed opened; without one, nothing
  /// arrives until [`FeedMotion::set_baseline_if_unset`] got one.
  pub fn new(maybe_newest: Option<MessageId>) -> Self {
    Self {
      maybe_baseline: maybe_newest,
      moved: HashSet::new(),
    }
  }

  /// The first messages of a conversation that opened empty (still loading) are history, not news.
  pub fn set_baseline_if_unset(&mut self, newest: MessageId) {
    self.maybe_baseline.get_or_insert(newest);
  }

  /// Decides how a row showing `message` moves, given what it showed before, and remembers it.
  pub fn row_motion(&mut self, maybe_previous: Option<&Shown>, message: &Message) -> RowMotion {
    let entrance = match maybe_previous {
      Some(previous) if previous.id == message.id => Entrance::None,
      Some(previous) if is_pending(previous.id) && !is_pending(message.id) => Entrance::Settling,
      _ if self.is_news(message.id) => Entrance::Arriving,
      _ => Entrance::None,
    };
    if entrance != Entrance::None {
      self.remember(message.id);
    }
    let popping = match maybe_previous {
      Some(previous) if previous.id == message.id => changed_reactions(previous, message),
      _ => Vec::new(),
    };
    RowMotion { entrance, popping }
  }

  fn is_news(&self, id: MessageId) -> bool {
    !self.moved.contains(&id) && self.maybe_baseline.is_some_and(|baseline| id > baseline)
  }

  fn remember(&mut self, id: MessageId) {
    if self.moved.len() >= MAX_REMEMBERED {
      self.moved.clear();
    }
    self.moved.insert(id);
  }
}

/// The newest confirmed message among `newest_first` (pending ones are the user's own, still in
/// the outbox).
pub fn newest_confirmed<'a>(newest_first: impl IntoIterator<Item = &'a Message>) -> Option<MessageId> {
  newest_first
    .into_iter()
    .map(|message| message.id)
    .find(|id| !is_pending(*id))
}

fn changed_reactions(previous: &Shown, message: &Message) -> Vec<String> {
  message
    .reactions
    .iter()
    .filter(|reaction| {
      !previous
        .reactions
        .iter()
        .any(|(emoji, count)| *emoji == reaction.emoji && *count == reaction.count)
    })
    .map(|reaction| reaction.emoji.clone())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{MessageKind, PENDING_ID_BIT, Reaction, id_from_raw};

  #[test]
  fn messages_from_before_the_feed_opened_do_not_arrive() {
    let mut motion = FeedMotion::new(Some(id_from_raw(10)));
    assert_eq!(motion.row_motion(None, &message(9)).entrance, Entrance::None);
    assert_eq!(motion.row_motion(None, &message(10)).entrance, Entrance::None);
  }

  #[test]
  fn a_new_message_arrives_once() {
    let mut motion = FeedMotion::new(Some(id_from_raw(10)));
    assert_eq!(motion.row_motion(None, &message(11)).entrance, Entrance::Arriving);
    // Scrolled away and back: the row is rebuilt, the message stays put.
    assert_eq!(motion.row_motion(None, &message(11)).entrance, Entrance::None);
  }

  #[test]
  fn a_feed_that_opened_empty_takes_its_first_messages_as_history() {
    let mut motion = FeedMotion::new(None);
    assert_eq!(motion.row_motion(None, &message(5)).entrance, Entrance::None);
    motion.set_baseline_if_unset(id_from_raw(5));
    motion.set_baseline_if_unset(id_from_raw(99));
    assert_eq!(motion.row_motion(None, &message(6)).entrance, Entrance::Arriving);
  }

  #[test]
  fn the_own_message_arrives_while_pending_and_settles_when_confirmed() {
    let mut motion = FeedMotion::new(Some(id_from_raw(10)));
    let pending = pending(3);
    assert_eq!(motion.row_motion(None, &pending).entrance, Entrance::Arriving);
    let confirmed = message(12);
    assert_eq!(
      motion.row_motion(Some(&Shown::of(&pending)), &confirmed).entrance,
      Entrance::Settling
    );
    // Rebuilt later (scrolled back): no second entrance.
    assert_eq!(motion.row_motion(None, &confirmed).entrance, Entrance::None);
  }

  #[test]
  fn only_changed_reactions_pop() {
    let mut motion = FeedMotion::new(Some(id_from_raw(10)));
    let before = with_reactions(4, &[("👍", 1), ("🎉", 2)]);
    let after = with_reactions(4, &[("👍", 2), ("🎉", 2), ("👀", 1)]);
    let shown = Shown::of(&before);
    assert_eq!(motion.row_motion(Some(&shown), &after).popping, ["👍", "👀"]);
    // A first render pops nothing.
    assert!(motion.row_motion(None, &after).popping.is_empty());
  }

  #[test]
  fn the_newest_confirmed_message_skips_the_outbox() {
    let list = [pending(1), message(8), message(7)];
    assert_eq!(newest_confirmed(&list), Some(id_from_raw(8)));
    assert_eq!(newest_confirmed(&[pending(1)]), None);
  }

  #[test]
  fn the_memory_stays_bounded() {
    let mut motion = FeedMotion::new(Some(id_from_raw(0)));
    for raw in 1..=(MAX_REMEMBERED as u64 * 2) {
      motion.row_motion(None, &message(raw));
    }
    assert!(motion.moved.len() <= MAX_REMEMBERED);
  }

  fn pending(raw: u64) -> Message {
    message(u64::try_from(PENDING_ID_BIT).unwrap_or(0) | raw)
  }

  fn message(raw: u64) -> Message {
    with_reactions(raw, &[])
  }

  fn with_reactions(raw: u64, reactions: &[(&str, u32)]) -> Message {
    Message {
      id: id_from_raw(raw),
      reactions: reactions
        .iter()
        .map(|(emoji, count)| Reaction {
          emoji: (*emoji).to_owned(),
          count: *count,
          reacted_by_me: false,
        })
        .collect(),
      conversation: id_from_raw(1),
      author: id_from_raw(2),
      kind: MessageKind::User,
      sent_at: 0,
      body: String::new(),
      is_edited: false,
      reply_count: 0,
      maybe_thread: None,
    }
  }
}
