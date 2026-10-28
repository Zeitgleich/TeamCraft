//! "Alice is typing…" under a conversation's messages, and when the composer tells the others that
//! the user types (#733), as the phone apps do. Also in the chat list and in a direct
//! conversation's header, and in a thread for the replies typed there (#882). Typing is metadata the
//! server sees (e2ee.md §4.4); the user can stop sharing their own in Settings > Privacy.
//!
//! Who types where is asked once per change for the whole window ([`provide_typing_now`]); every
//! row, header and line derives its own memo from that answer, so a `typing_start` only touches the
//! places it concerns.

use leptos::prelude::*;

use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, ThreadId, TypingNow, UserId};
use crate::state::{AppState, use_app_state};

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

/// Everyone typing anywhere, read once per backend change for every place that shows it.
#[derive(Clone, Copy)]
struct TypingNowHub(Memo<Vec<TypingNow>>);

/// Shares who types where with the whole shell: one question to the core per change, however many
/// rows show it (on the desktop each question is a call to the core).
pub fn provide_typing_now(state: AppState) {
  provide_context(TypingNowHub(Memo::new(move |_| {
    state.read(|backend| backend.typing_now())
  })));
}

fn use_typing_now() -> Memo<Vec<TypingNow>> {
  use_context::<TypingNowHub>().map_or_else(
    || {
      let state = use_app_state();
      Memo::new(move |_| state.read(|backend| backend.typing_now()))
    },
    |hub| hub.0,
  )
}

/// Who types in a conversation itself (`maybe_thread` `None`) or in one of its threads.
pub fn users_at(
  typing: &[TypingNow],
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
) -> Vec<UserId> {
  typing
    .iter()
    .find(|place| place.conversation == conversation && place.maybe_thread == maybe_thread)
    .map(|place| place.users.clone())
    .unwrap_or_default()
}

/// What a place shows while people type there, or `None`. `is_direct`: a direct conversation's row
/// and header say only "typing…", the name is right there.
pub fn typing_text(
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
  is_direct: bool,
) -> Memo<Option<String>> {
  let state = use_app_state();
  let i18n = use_i18n();
  let typing = use_typing_now();
  // Changes only when someone starts or stops typing here, not with every other place.
  let users = Memo::new(move |_| typing.with(|typing| users_at(typing, conversation, maybe_thread)));
  Memo::new(move |_| {
    let users = users.get();
    if users.is_empty() {
      return None;
    }
    if is_direct {
      return Some(i18n.t("typing-direct"));
    }
    let names: Vec<String> = state.read(|backend| {
      users
        .iter()
        .filter_map(|user| backend.user(*user))
        .map(|user| user.display_name)
        .collect()
    });
    TypingLine::of(&names).map(|line| line.text(i18n))
  })
}

/// The three dots that pulse one after the other while someone types (still with reduced motion).
#[component]
pub fn TypingDots() -> impl IntoView {
  view! {
    <span class="typing-dots" aria-hidden="true">
      <span></span>
      <span></span>
      <span></span>
    </span>
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

/// Who else is typing in the conversation, on the bottom edge of the messages above the composer;
/// with `maybe_thread`, who types a reply in that thread, at the bottom of the thread (replies
/// typed in a thread don't show under the conversation).
///
/// It takes no room of its own: it sits in the space the message list keeps under the newest
/// message, so nothing moves when someone starts or stops typing. Screen readers hear the line once
/// per change (a polite status), not on every refresh; the visible line is hidden from them.
#[component]
pub fn TypingIndicator(
  conversation: ConversationId,
  #[prop(optional)] maybe_thread: Option<ThreadId>,
) -> impl IntoView {
  // A `typing_start` changes it, and so does an indicator that ran out (the core wakes up for it).
  // The memo keeps an unchanged line from touching the DOM.
  let maybe_text = typing_text(conversation, maybe_thread, false);
  let text = Memo::new(move |_| maybe_text.get().unwrap_or_default());
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
        <TypingDots />
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

  mod places {
    use super::*;
    use crate::model::id_from_raw;

    fn place(conversation: u64, maybe_thread: Option<u64>, users: &[u64]) -> TypingNow {
      TypingNow {
        conversation: id_from_raw(conversation),
        maybe_thread: maybe_thread.map(id_from_raw),
        users: users.iter().copied().map(id_from_raw).collect(),
      }
    }

    #[test]
    fn each_place_reads_its_own_people() {
      let typing = [
        place(7, None, &[2, 3]),
        place(7, Some(70), &[4]),
        place(8, None, &[5]),
      ];
      let ids = |raw: &[u64]| -> Vec<UserId> { raw.iter().copied().map(id_from_raw).collect() };
      assert_eq!(users_at(&typing, id_from_raw(7), None), ids(&[2, 3]));
      assert_eq!(
        users_at(&typing, id_from_raw(7), Some(id_from_raw(70))),
        ids(&[4])
      );
      assert_eq!(users_at(&typing, id_from_raw(8), None), ids(&[5]));
      // Nobody types in another thread, or in a conversation not listed.
      assert!(users_at(&typing, id_from_raw(7), Some(id_from_raw(71))).is_empty());
      assert!(users_at(&typing, id_from_raw(9), None).is_empty());
    }

    #[test]
    fn rows_headers_and_threads_each_follow_their_own_place() {
      use std::rc::Rc;

      use crate::backend::Backend;
      use crate::mock::MockBackend;
      use crate::settings::Settings;

      let mock = Rc::new(MockBackend::new(0));
      let backend: Rc<dyn Backend> = mock.clone();
      let state = AppState::new(backend, Settings::for_tests(), None);
      let owner = Owner::new();
      owner.with(|| {
        provide_context(state);
        provide_context(I18n::new(Locale::En));
        provide_typing_now(state);
        let channel = typing_text(id_from_raw(7), None, false);
        let thread = typing_text(id_from_raw(7), Some(id_from_raw(70)), false);
        let direct = typing_text(id_from_raw(8), None, true);
        let quiet = typing_text(id_from_raw(9), None, false);
        assert_eq!(channel.get_untracked(), None, "nobody types yet");

        mock.set_typing(vec![
          place(7, None, &[2, 3]),
          place(7, Some(70), &[4]),
          place(8, None, &[2]),
        ]);
        state.backend_changed();
        assert_eq!(
          channel.get_untracked().as_deref(),
          Some("Sam Rivera and Jordan Lee are typing…")
        );
        assert_eq!(thread.get_untracked().as_deref(), Some("Robin Chen is typing…"));
        assert_eq!(direct.get_untracked().as_deref(), Some("typing…"));
        assert_eq!(quiet.get_untracked(), None);

        // The indicators ran out.
        mock.set_typing(Vec::new());
        state.backend_changed();
        assert_eq!(channel.get_untracked(), None);
        assert_eq!(thread.get_untracked(), None);
        assert_eq!(direct.get_untracked(), None);
      });
    }

    #[test]
    fn a_direct_conversation_says_only_typing() {
      for locale in [Locale::En, Locale::De] {
        let text = translate(locale, "typing-direct", None);
        assert_ne!(text, "typing-direct", "{locale:?}");
        assert!(text.ends_with('…'), "{locale:?}: {text}");
      }
      assert_eq!(translate(Locale::En, "typing-direct", None), "typing…");
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
