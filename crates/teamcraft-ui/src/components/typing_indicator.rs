//! "Alice is typing…" under a conversation's messages, and when the composer tells the others that
//! the user types (#733), as the phone apps do. Typing is metadata the server sees (e2ee.md §4.4);
//! the user can stop sharing their own in Settings > Privacy.

use leptos::prelude::*;

use crate::i18n::{I18n, use_i18n};
use crate::model::ConversationId;
use crate::state::use_app_state;

/// How often the composer tells the core that the user types; the core and the server throttle as
/// well (the server forwards one per 3 s and conversation). The same as on the phones.
pub const TYPING_NOTIFY_INTERVAL_MS: i64 = 3_000;

/// What the typing line says, by how many people type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypingLine {
  One(String),
  Two(String, String),
  Several,
}

impl TypingLine {
  /// The line for these names, or none when nobody types.
  pub fn of(names: &[String]) -> Option<Self> {
    match names {
      [] => None,
      [name] => Some(Self::One(name.clone())),
      [first, second] => Some(Self::Two(first.clone(), second.clone())),
      _ => Some(Self::Several),
    }
  }

  pub fn text(&self, i18n: I18n) -> String {
    let (key, args) = self.message();
    i18n.t_with(key, &args)
  }

  /// The i18n key and its arguments.
  fn message(&self) -> (&'static str, Vec<(&'static str, &str)>) {
    match self {
      Self::One(name) => ("typing-one", vec![("name", name.as_str())]),
      Self::Two(first, second) => (
        "typing-two",
        vec![("first", first.as_str()), ("second", second.as_str())],
      ),
      Self::Several => ("typing-several", Vec::new()),
    }
  }
}

/// Whether an edit in the composer announces typing now: only with text in it (clearing the
/// composer is no typing), and at most every [`TYPING_NOTIFY_INTERVAL_MS`]. After the clock went
/// back it announces again rather than waiting out the jump.
pub fn should_notify_typing(is_blank: bool, now_ms: i64, maybe_last_ms: Option<i64>) -> bool {
  !is_blank
    && maybe_last_ms
      .is_none_or(|last_ms| !(0..TYPING_NOTIFY_INTERVAL_MS).contains(&now_ms.saturating_sub(last_ms)))
}

/// Who else is typing in the conversation, on the bottom edge of the messages above the composer.
///
/// It takes no room of its own: it sits in the space the message list keeps under the newest
/// message, so nothing moves when someone starts or stops typing. Screen readers hear the line once
/// per change (a polite status), not on every refresh; the visible line is hidden from them.
#[component]
pub fn TypingIndicator(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // Re-read on every change of the backend: a `typing_start` is one, and so is an indicator that
  // ran out (the core wakes up for it). The memo keeps an unchanged line from touching the DOM.
  let text = Memo::new(move |_| {
    let names: Vec<String> = state.read(|backend| {
      backend
        .typing_users(conversation)
        .into_iter()
        .filter_map(|user| backend.user(user))
        .map(|user| user.display_name)
        .collect()
    });
    TypingLine::of(&names).map_or_else(String::new, |line| line.text(i18n))
  });
  let is_visible = Memo::new(move |_| !text.with(String::is_empty));
  // The last line stays while it fades out, instead of going blank first.
  let shown = Memo::new(move |previous: Option<&String>| {
    let current = text.get();
    match previous {
      Some(previous) if current.is_empty() => previous.clone(),
      _ => current,
    }
  });

  view! {
    <div class="typing-anchor">
      <div class="typing-line" class:is-visible=is_visible aria-hidden="true" translate="no">
        <span class="typing-dots">
          <span></span>
          <span></span>
          <span></span>
        </span>
        <span class="typing-text">{move || shown.get()}</span>
      </div>
      <p class="visually-hidden" role="status" translate="no">
        {move || text.get()}
      </p>
    </div>
  }
}

#[cfg(test)]
mod tests {
  use fluent_bundle::FluentArgs;

  use super::*;
  use crate::i18n::{Locale, translate};

  mod line {
    use super::*;

    #[test]
    fn says_who_types_by_how_many_there_are() {
      assert_eq!(TypingLine::of(&[]), None);
      assert_eq!(
        TypingLine::of(&names(&["Alice"])),
        Some(TypingLine::One("Alice".to_owned()))
      );
      assert_eq!(
        TypingLine::of(&names(&["Alice", "Bob"])),
        Some(TypingLine::Two("Alice".to_owned(), "Bob".to_owned()))
      );
      assert_eq!(
        TypingLine::of(&names(&["Alice", "Bob", "Carol"])),
        Some(TypingLine::Several)
      );
    }

    #[test]
    fn every_line_is_translated_with_its_names() {
      let lines = [
        TypingLine::One("Alice".to_owned()),
        TypingLine::Two("Alice".to_owned(), "Bob".to_owned()),
        TypingLine::Several,
      ];
      for locale in [Locale::En, Locale::De] {
        for line in &lines {
          let (key, args) = line.message();
          let text = rendered(locale, line);
          assert_ne!(text, key, "{locale:?} lacks {key}");
          for (_, value) in &args {
            assert!(text.contains(value), "{locale:?} {key}: {text}");
          }
        }
      }
      let two = TypingLine::Two("Alice".to_owned(), "Bob".to_owned());
      assert_eq!(rendered(Locale::En, &two), "Alice and Bob are typing…");
      assert_eq!(rendered(Locale::De, &two), "Alice und Bob schreiben …");
    }
  }

  mod notify {
    use super::*;

    #[test]
    fn typing_is_announced_at_most_every_few_seconds() {
      assert!(should_notify_typing(false, 1_000, None));
      assert!(!should_notify_typing(
        false,
        1_000 + TYPING_NOTIFY_INTERVAL_MS - 1,
        Some(1_000)
      ));
      assert!(should_notify_typing(
        false,
        1_000 + TYPING_NOTIFY_INTERVAL_MS,
        Some(1_000)
      ));
    }

    #[test]
    fn an_empty_composer_announces_nothing() {
      assert!(!should_notify_typing(true, 10_000, None));
    }

    #[test]
    fn typing_is_announced_after_the_clock_went_back() {
      assert!(should_notify_typing(false, 1_000, Some(60_000)));
    }
  }

  fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
  }

  fn rendered(locale: Locale, line: &TypingLine) -> String {
    let (key, args) = line.message();
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
      fluent_args.set(name, value);
    }
    translate(locale, key, Some(&fluent_args))
  }
}
