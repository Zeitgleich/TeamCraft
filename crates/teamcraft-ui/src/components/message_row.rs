use leptos::prelude::*;

use crate::clock::{format_time, format_time_seconds, local_day};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::rich_text::RichText;
use crate::i18n::use_i18n;
use crate::model::{LogLevel, Message, MessageKind, SystemEvent, User};
use crate::state::use_app_state;

/// Messages by the same author closer together than this share one header.
const GROUP_WINDOW_MS: i64 = 5 * 60_000;
const QUICK_REACTION: &str = "👍";

/// One message in the channel feed or in a thread. The caller recreates it when the message changes.
#[component]
pub fn MessageRow(
  message: Message,
  /// The message right before this one in the same list, used for grouping and date dividers.
  maybe_prev: Option<Message>,
  #[prop(default = false)] in_thread: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let locale = i18n.locale().tag();
  let divider = date_divider(&message, maybe_prev.as_ref())
    .filter(|_| !in_thread)
    .map(|ts| crate::clock::format_date(ts, locale));
  let divider_view = divider.map(|label| {
    view! {
      <div class="date-divider" role="separator">
        <span>{label}</span>
      </div>
    }
  });

  let body = match &message.kind {
    MessageKind::Log(level) => log_line(&message, *level, locale).into_any(),
    MessageKind::System(event) => {
      let author = state.read(|backend| backend.user(message.author));
      system_line(&message, event, author, locale).into_any()
    },
    MessageKind::User => {
      let is_grouped = is_grouped(&message, maybe_prev.as_ref());
      user_message(message, is_grouped, in_thread).into_any()
    },
  };
  view! {
    {divider_view}
    {body}
  }
}

fn log_line(message: &Message, level: LogLevel, locale: &'static str) -> impl IntoView + use<> {
  let time = format_time_seconds(message.sent_at, locale);
  let body = message.body.clone();
  let i18n = use_i18n();
  view! {
    <div class=format!("msg msg-log level-{}", level.slug()) tabindex="-1" data-nav="row">
      <time class="log-time" datetime=message.sent_at.to_string()>
        {time}
      </time>
      <span class="log-level">{move || i18n.t(&format!("log-level-{}", level.slug()))}</span>
      <span class="log-text" translate="no">
        {body}
      </span>
    </div>
  }
}

fn system_line(
  message: &Message,
  event: &SystemEvent,
  maybe_author: Option<User>,
  locale: &'static str,
) -> impl IntoView + use<> {
  let i18n = use_i18n();
  let state = use_app_state();
  let name = maybe_author.map(|user| user.display_name).unwrap_or_default();
  let event = event.clone();
  let time = format_time(message.sent_at, locale);
  view! {
    {move || {
      state
        .settings
        .show_join_leave
        .get()
        .then(|| {
          let event = event.clone();
          let name = name.clone();
          let time = time.clone();
          view! {
            <div class="msg msg-system" tabindex="-1" data-nav="row">
              <span class="system-text">
                {move || match &event {
                  SystemEvent::Joined => i18n.t_with("system-joined", &[("name", &name)]),
                  SystemEvent::Left => i18n.t_with("system-left", &[("name", &name)]),
                  SystemEvent::TopicChanged { topic } => {
                    i18n.t_with("system-topic-changed", &[("name", &name), ("topic", topic)])
                  },
                  SystemEvent::Renamed { name: new_name } => {
                    i18n.t_with("system-renamed", &[("name", &name), ("channel", new_name)])
                  },
                }}
              </span>
              <time class="system-time">{time}</time>
            </div>
          }
        })
    }}
  }
}

fn user_message(message: Message, is_grouped: bool, in_thread: bool) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let locale = i18n.locale().tag();
  let author = state.read(|backend| backend.user(message.author));
  let me = state.me();
  let time = format_time(message.sent_at, locale);
  let thread_root = message.maybe_thread_root.unwrap_or(message.id);
  let conversation = message.conversation;
  let message_id = message.id;
  let reply_count = message.reply_count;
  let reactions = message.reactions.clone();
  let is_mine = message.author == me.id;
  let author_name = author
    .as_ref()
    .map(|user| user.display_name.clone())
    .unwrap_or_default();
  let aria_label = format!("{author_name}, {time}");

  let gutter = match (is_grouped, author.clone()) {
    (false, Some(user)) => view! { <Avatar user=user size=AvatarSize::Large /> }.into_any(),
    _ => {
      let time = time.clone();
      view! { <time class="hover-time">{time}</time> }.into_any()
    },
  };

  let header = (!is_grouped).then(|| {
    let status = author.as_ref().and_then(|user| user.maybe_custom_status.clone());
    let badge_bot = author.as_ref().is_some_and(|user| user.is_bot);
    let badge_guest = author.as_ref().is_some_and(|user| user.is_guest);
    view! {
      <div class="msg-header">
        <span class="msg-author" translate="no">
          {author_name.clone()}
        </span>
        {badge_bot.then(|| view! { <span class="badge">{move || i18n.t("badge-bot")}</span> })}
        {badge_guest.then(|| view! { <span class="badge badge-guest">{move || i18n.t("badge-guest")}</span> })}
        {status.map(|status| view! { <span class="msg-status" title=status.text>{status.emoji}</span> })}
        <time class="msg-time" datetime=message.sent_at.to_string()>
          {time.clone()}
        </time>
      </div>
    }
  });

  let edited = message.is_edited.then(|| {
    view! { <span class="edited">{move || i18n.t("message-edited")}</span> }
  });

  let reaction_chips = (!reactions.is_empty()).then(|| {
    let chips: Vec<_> = reactions
      .into_iter()
      .map(|reaction| {
        let emoji = reaction.emoji.clone();
        let label_emoji = reaction.emoji.clone();
        let count = reaction.count;
        view! {
          <button
            class="reaction"
            class:reaction-mine=reaction.reacted_by_me
            aria-pressed=reaction.reacted_by_me.to_string()
            aria-label=move || i18n.t_with("reaction-label", &[("emoji", &label_emoji), ("count", &count.to_string())])
            on:click=move |_| state.write(|backend| backend.toggle_reaction(conversation, message_id, &emoji))
          >
            <span aria-hidden="true">{reaction.emoji}</span>
            <span class="reaction-count">{count}</span>
          </button>
        }
      })
      .collect();
    view! { <div class="reactions">{chips}</div> }
  });

  let replies = (!in_thread && reply_count > 0).then(|| {
    view! {
      <button class="reply-count" on:click=move |_| state.open_thread(conversation, thread_root)>
        <Icon name=IconName::Threads />
        <span>{move || i18n.t_count("thread-replies", reply_count as usize)}</span>
      </button>
    }
  });

  let actions = view! {
    <div class="msg-actions" role="toolbar" aria-label=move || i18n.t("message-actions")>
      <button
        class="icon-btn"
        aria-label=move || i18n.t("action-react")
        title=move || i18n.t("action-react")
        on:click=move |_| state.write(|backend| backend.toggle_reaction(conversation, message_id, QUICK_REACTION))
      >
        <Icon name=IconName::Smile />
      </button>
      {(!in_thread).then(|| {
        view! {
          <button
            class="icon-btn"
            aria-label=move || i18n.t("action-reply")
            title=move || i18n.t("action-reply")
            on:click=move |_| state.open_thread(conversation, thread_root)
          >
            <Icon name=IconName::Reply />
          </button>
        }
      })}
    </div>
  };

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.target() == ev.current_target() && ev.key() == "Enter" && !in_thread {
      state.open_thread(conversation, thread_root);
    }
  };

  view! {
    <div
      class="msg msg-user"
      class:msg-grouped=is_grouped
      class:msg-mine=is_mine
      tabindex="-1"
      data-nav="row"
      aria-label=aria_label
      on:keydown=on_keydown
    >
      <div class="msg-gutter">{gutter}</div>
      <div class="msg-main">
        {header} <div class="msg-body">
          <RichText body=message.body.clone() me=me.username />
          {edited}
        </div> {reaction_chips} {replies}
      </div>
      {actions}
    </div>
  }
}

fn is_grouped(message: &Message, maybe_prev: Option<&Message>) -> bool {
  let Some(prev) = maybe_prev else { return false };
  matches!(prev.kind, MessageKind::User)
    && prev.author == message.author
    && message.sent_at - prev.sent_at < GROUP_WINDOW_MS
    && local_day(prev.sent_at) == local_day(message.sent_at)
}

/// A date divider is drawn before the first message and whenever the local day changes.
fn date_divider(message: &Message, maybe_prev: Option<&Message>) -> Option<i64> {
  match maybe_prev {
    Some(prev) if local_day(prev.sent_at) == local_day(message.sent_at) => None,
    _ => Some(message.sent_at),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationId, MessageId, UserId};

  #[test]
  fn grouping_needs_same_author_and_short_gap() {
    let first = message(1, 1, 0);
    assert!(is_grouped(&message(2, 1, 60_000), Some(&first)));
    assert!(!is_grouped(&message(3, 2, 60_000), Some(&first)));
    assert!(!is_grouped(&message(4, 1, GROUP_WINDOW_MS), Some(&first)));
    assert!(!is_grouped(&first, None));
  }

  #[test]
  fn divider_on_first_message_and_day_change() {
    let first = message(1, 1, 0);
    assert!(date_divider(&first, None).is_some());
    assert!(date_divider(&message(2, 1, 1000), Some(&first)).is_none());
    assert!(date_divider(&message(3, 1, 86_400_000 + 1), Some(&first)).is_some());
  }

  fn message(id: u64, author: u64, sent_at: i64) -> Message {
    Message {
      id: MessageId(id),
      conversation: ConversationId(1),
      author: UserId(author),
      kind: MessageKind::User,
      sent_at,
      body: String::new(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread_root: None,
    }
  }
}
