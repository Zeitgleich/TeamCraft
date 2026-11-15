//! The card beside a sidebar row after the pointer rests on it or it has the keyboard focus
//! (#2078): the last few messages of the conversation, so people can glance without opening it,
//! which would mark it read. It shows what the store already holds and loads nothing.

use std::time::Duration;

use leptos::prelude::*;

use crate::clock::format_time;
use crate::components::message_actions::excerpt;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, Message, MessageKind};
use crate::state::use_app_state;

/// How long the pointer or focus rests on a row before the card appears.
pub const PREVIEW_DELAY: Duration = Duration::from_millis(600);
/// How long the card stays after the pointer left, so it can move onto the card.
pub const PREVIEW_GRACE: Duration = Duration::from_millis(250);
/// The card's estimated height in pixels, to keep it inside the window.
pub const PREVIEW_HEIGHT_PX: f64 = 210.0;

/// Messages shown at most.
const SHOWN: usize = 4;
/// Rows asked of the store: some may be system lines or have no text to show.
const LOOKED_AT: usize = 8;

/// Where the card goes and when: a fixed position in pixels beside the row.
#[component]
pub fn ConversationPreview(
  conversation: ConversationId,
  /// The conversation's name, for the card's label.
  #[prop(into)]
  name: String,
  top: f64,
  left: f64,
  /// The id the row points at with `aria-describedby`.
  id: String,
  on_enter: Callback<()>,
  on_leave: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let lines: Vec<PreviewLine> = state.read(|backend| {
    backend
      .held_recent_messages(conversation, LOOKED_AT)
      .iter()
      .filter_map(|message| {
        let sender = backend
          .user(message.author)
          .map(|user| user.display_name)
          .unwrap_or_default();
        preview_line(message, sender, |ts| format_time(ts, locale))
      })
      .collect()
  });
  let first_shown = lines.len().saturating_sub(SHOWN);
  let label = i18n.t_with("conversation-preview-label", &[("name", &name)]);
  let is_empty = lines.is_empty();
  view! {
    <div
      id=id
      class="hover-card conversation-preview"
      role="tooltip"
      aria-label=label
      style:top=format!("{top}px")
      style:left=format!("{left}px")
      on:pointerenter=move |_| on_enter.run(())
      on:pointerleave=move |_| on_leave.run(())
    >
      {is_empty.then(|| view! { <p class="small muted-text">{move || i18n.t("conversation-preview-empty")}</p> })}
      {lines
        .into_iter()
        .skip(first_shown)
        .map(|line| {
          view! {
            <div class="conversation-preview-line">
              <p class="conversation-preview-meta small muted-text">
                <strong translate="no">{line.sender}</strong>
                <time>{line.time}</time>
              </p>
              <p class="conversation-preview-body" translate="no">{line.text}</p>
            </div>
          }
        })
        .collect_view()}
    </div>
  }
}

/// One message as the card shows it.
#[derive(Debug, PartialEq, Eq)]
struct PreviewLine {
  sender: String,
  time: String,
  /// One line, bounded.
  text: String,
}

/// The line for `message`, or `None` for one with nothing to read (a system line, a message this
/// device can't decrypt yet, or one that is only an attachment).
fn preview_line(message: &Message, sender: String, time: impl Fn(i64) -> String) -> Option<PreviewLine> {
  let is_readable =
    matches!(message.kind, MessageKind::User | MessageKind::Log(_)) && message.maybe_placeholder.is_none();
  let text = excerpt(&message.body);
  (is_readable && !text.is_empty()).then(|| PreviewLine {
    sender,
    time: time(message.sent_at),
    text,
  })
}

#[cfg(test)]
mod tests {
  use crate::model::{SystemEvent, id_from_raw};

  use super::*;

  #[test]
  fn a_text_message_is_one_bounded_line() {
    let line = preview_line(&message("Hello\n  there"), "Sam".into(), |_| "09:00".into()).unwrap();
    assert_eq!(line.text, "Hello there");
    assert_eq!((line.sender.as_str(), line.time.as_str()), ("Sam", "09:00"));
    let long = "x".repeat(500);
    let line = preview_line(&message(&long), String::new(), |_| String::new()).unwrap();
    assert!(line.text.chars().count() < 200);
  }

  #[test]
  fn lines_without_text_are_skipped() {
    assert_eq!(
      preview_line(&message("  "), String::new(), |_| String::new()),
      None
    );
    let mut system = message("joined");
    system.kind = MessageKind::System(SystemEvent::Joined);
    assert_eq!(preview_line(&system, String::new(), |_| String::new()), None);
  }

  fn message(body: &str) -> Message {
    Message {
      id: id_from_raw(1),
      conversation: id_from_raw(2),
      author: id_from_raw(3),
      kind: MessageKind::User,
      sent_at: 0,
      body: body.into(),
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
