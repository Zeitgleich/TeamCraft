//! The round "jump to reaction" button above "jump to latest" (issue #1009, notifications.md
//! "Reactions to my messages"): shown while others' reactions to the user's own messages wait
//! unseen. A click goes to the oldest such message and lights it up; reactions count as seen once
//! their message is in view of the watched feed, or when the user jumps to it.

use std::collections::HashSet;
use std::ops::Range;

use leptos::prelude::*;

use crate::components::channel_view::{DateSearch, first_index_at_or_after};
use crate::components::count_badge::{BadgeTone, CountBadge};
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, Id, MessageId, UnseenReactions};
use crate::state::use_app_state;

/// The largest count the badge shows; more reads "99+", so the round button keeps its size.
const MAX_BADGE: u32 = 99;

#[component]
pub fn ReactionJump(
  conversation: ConversationId,
  /// The rows in view.
  visible: Memo<Range<usize>>,
  /// The focused window shows the feed (no call screen over it).
  is_watched: Signal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let unseen = Memo::new(move |_| state.read(|backend| backend.unseen_reactions(conversation)));
  // Marked already from here while they wait: a backend that answers later still lists them
  // meanwhile, and they must not be sent again on every change.
  let marked = StoredValue::new(HashSet::<MessageId>::new());
  let mark = move |messages: Vec<MessageId>| {
    let fresh: Vec<MessageId> = marked
      .try_update_value(|marked| messages.into_iter().filter(|id| marked.insert(*id)).collect())
      .unwrap_or_default();
    if !fresh.is_empty() {
      state.write(|backend| backend.mark_reactions_seen(conversation, fresh));
    }
  };

  // Reactions whose message is in view of the watched feed are seen.
  Effect::new(move |_| {
    let waiting = unseen.with(|unseen| unseen.messages.clone());
    // A message that waits no more and gets a new reaction later waits again.
    marked.update_value(|marked| marked.retain(|id| waiting.contains(id)));
    if waiting.is_empty() || !is_watched.get() {
      return;
    }
    let rows = visible.get();
    let shown = state.read(|backend| backend.messages(conversation, rows.start, rows.len()));
    mark(in_view(shown.iter().map(|message| message.id), &waiting));
  });

  // The message to jump to, until its row is found among the loaded ones.
  let target = RwSignal::new(None::<MessageId>);
  Effect::new(move |_| {
    let Some(message) = target.get() else {
      return;
    };
    let search = state.read(|backend| {
      first_index_at_or_after(
        backend.message_count(conversation),
        |index| {
          backend
            .messages(conversation, index, 1)
            .first()
            .map(|row| row.id.snowflake().get())
        },
        message.snowflake().get(),
      )
    });
    match search {
      DateSearch::Found(index) => {
        target.set(None);
        state.highlight(message);
        state.jump_to_index.set(Some((conversation, index)));
      },
      DateSearch::Empty => target.set(None),
      DateSearch::Pending => {},
    }
  });
  let jump = move |_| {
    if let Some(oldest) = unseen.with_untracked(jump_target) {
      mark(vec![oldest]);
      target.set(Some(oldest));
    }
  };

  view! {
    <Show when=move || unseen.with(is_shown) fallback=|| ()>
      <button
        class="jump-latest jump-reaction"
        aria-label=move || label(i18n, &unseen.get())
        title=move || label(i18n, &unseen.get())
        on:click=jump
      >
        <Icon name=IconName::Heart />
        <CountBadge
          count=Signal::derive(move || unseen.with(|unseen| u32::try_from(unseen.count).unwrap_or(u32::MAX)))
          tone=BadgeTone::Unread
          is_decorative=true
          maybe_cap=MAX_BADGE
        />
      </button>
    </Show>
  }
}

/// The name of the round button, which has no text of its own.
fn label(i18n: I18n, unseen: &UnseenReactions) -> String {
  i18n.t_count("jump-to-reaction", unseen.count)
}

/// Shown while anything waits unseen; gone at zero.
fn is_shown(unseen: &UnseenReactions) -> bool {
  unseen.count > 0 && !unseen.messages.is_empty()
}

/// The oldest message with an unseen reaction.
fn jump_target(unseen: &UnseenReactions) -> Option<MessageId> {
  unseen.messages.first().copied()
}

/// The shown messages whose reactions wait unseen.
fn in_view(shown: impl Iterator<Item = MessageId>, waiting: &[MessageId]) -> Vec<MessageId> {
  shown.filter(|id| waiting.contains(id)).collect()
}

#[cfg(test)]
mod tests {
  use crate::model::Snowflake;

  use super::*;

  #[test]
  fn the_button_shows_while_reactions_wait_and_jumps_to_the_oldest() {
    let waiting = UnseenReactions {
      count: 3,
      messages: vec![id(10), id(30)],
    };
    assert!(is_shown(&waiting));
    assert_eq!(jump_target(&waiting), Some(id(10)));
    let none = UnseenReactions::default();
    assert!(!is_shown(&none));
    assert_eq!(jump_target(&none), None);
  }

  #[test]
  fn only_shown_messages_with_waiting_reactions_are_seen() {
    let shown = [id(5), id(10), id(20)];
    assert_eq!(in_view(shown.into_iter(), &[id(10), id(30)]), vec![id(10)]);
    assert!(in_view(shown.into_iter(), &[]).is_empty());
  }

  fn id(raw: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
