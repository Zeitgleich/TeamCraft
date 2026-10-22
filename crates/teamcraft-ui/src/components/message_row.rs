use leptos::prelude::*;
use teamcraft_client_api::actions::{MessagePriority, WorkspaceEvent};
use teamcraft_proto::HistoryPolicy;

use crate::clock::{format_time, format_time_seconds, local_day};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::file_views::{LinkPreviewCard, MessageFiles};
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::MessageMenu;
use crate::components::rich_text::RichText;
use crate::i18n::use_i18n;
use crate::model::{LogLevel, Message, MessageKind, SystemEvent, User, thread_id_of};
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
  let (conversation, message_id) = (message.conversation, message.id);
  // Archive, privacy and membership changes come from the core as workspace events.
  let workspace_text = Memo::new(move |_| {
    state.read(|backend| {
      let extras = backend
        .message_extras(conversation, &[message_id])
        .into_iter()
        .find(|extras| extras.message == message_id)?;
      workspace_event_text(&extras.maybe_workspace_event?, |user| {
        backend
          .user(user)
          .map(|user| user.display_name)
          .unwrap_or_default()
      })
    })
  });
  view! {
    {move || {
      workspace_text.get().map(|(key, args)| {
        let args: Vec<(&str, &str)> = args.iter().map(|(name, value)| (*name, value.as_str())).collect();
        view! {
          <div class="msg msg-system" tabindex="-1" data-nav="row">
            <span class="system-text" translate="no">{i18n.t_with(key, &args)}</span>
          </div>
        }
      })
    }}
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
  let thread_root = message.maybe_thread.unwrap_or_else(|| thread_id_of(message.id));
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
  // Replies, pins, history and the rest come from the backend per visible message.
  let extras = Memo::new(move |_| {
    state
      .read(|backend| backend.message_extras(conversation, &[message_id]))
      .into_iter()
      .find(|extras| extras.message == message_id)
  });
  let is_editing = RwSignal::new(false);

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
        {status.filter(|_| state.settings.prefs.get_untracked().display.show_status_in_messages).map(|status| view! { <span class="msg-status" title=status.text>{status.emoji}</span> })}
        <time class="msg-time" datetime=message.sent_at.to_string()>
          {time.clone()}
        </time>
      </div>
    }
  });

  let edited = message.is_edited.then(|| {
    view! {
      <button
        class="edited link-btn"
        aria-haspopup="dialog"
        on:click=move |_| state.overlay.set(crate::state::Overlay::EditHistory(conversation, message_id))
      >
        {move || i18n.t("message-edited")}
      </button>
    }
  });
  let quote = move || {
    extras.get().and_then(|extras| extras.maybe_reply).map(|reply| {
      let name = state
        .read(|backend| backend.user(reply.author))
        .map(|user| user.display_name)
        .unwrap_or_default();
      view! {
        <blockquote class="msg-quote">
          <span class="quote-author" translate="no">{name}</span>
          <span class="quote-text" translate="no">
            {if reply.excerpt.is_empty() { i18n.t("quote-unavailable") } else { reply.excerpt }}
          </span>
        </blockquote>
      }
    })
  };
  let forwarded = move || {
    extras
      .get()
      .and_then(|extras| extras.maybe_forwarded)
      .map(|from| {
        let source = from.maybe_conversation_name.unwrap_or_default();
        view! {
          <div class="msg-forwarded">
            <Icon name=IconName::Send />
            <span translate="no">
              {i18n.t_with("forwarded-from", &[("name", &from.author_name), ("conversation", &source)])}
            </span>
          </div>
        }
      })
  };
  let badges = move || {
    extras.get().map(|extras| {
      view! {
        {extras.is_pinned.then(|| view! { <span class="badge badge-pin">{move || i18n.t("badge-pinned")}</span> })}
        {extras.is_saved.then(|| view! { <span class="badge badge-saved">{move || i18n.t("badge-saved")}</span> })}
        {match extras.priority {
          MessagePriority::Normal => ().into_any(),
          MessagePriority::Important => view! { <span class="badge badge-important">{move || i18n.t("priority-important")}</span> }.into_any(),
          MessagePriority::Urgent => view! { <span class="badge badge-urgent">{move || i18n.t("priority-urgent")}</span> }.into_any(),
        }}
      }
    })
  };
  let attachments = move || {
    extras.get().map(|extras| {
      view! {
        {(!extras.files.is_empty()).then(|| view! { <MessageFiles files=extras.files.clone() /> })}
        {extras.maybe_link_preview.clone().map(|preview| view! { <LinkPreviewCard preview=preview /> })}
      }
    })
  };
  let acknowledge = move || {
    extras
      .get()
      .filter(|extras| extras.wants_acknowledgement && !is_mine)
      .map(|extras| {
        let has_acknowledged = extras.acknowledged_by.contains(&me.id);
        view! {
          <button
            class="btn btn-small"
            prop:disabled=has_acknowledged
            on:click=move |_| state.write(|backend| backend.acknowledge(conversation, message_id))
          >
            {move || if has_acknowledged { i18n.t("ack-done") } else { i18n.t("ack-confirm") }}
          </button>
        }
      })
  };

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

  let message_for_menu = message.clone();
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
      <MessageMenu
        message=message_for_menu
        extras=Signal::derive(move || extras.get())
        is_mine=is_mine
        in_thread=in_thread
        on_edit=Callback::new(move |()| is_editing.set(true))
      />
    </div>
  };

  let body_text = message.body.clone();
  let edit_draft = RwSignal::new(String::new());
  let start_edit_text = body_text.clone();
  Effect::new(move |_| {
    if is_editing.get() {
      edit_draft.set(start_edit_text.clone());
    }
  });
  let save_edit = move || {
    let text = edit_draft.get_untracked();
    if !text.trim().is_empty() {
      state.write(|backend| backend.edit_message(conversation, message_id, &text));
    }
    is_editing.set(false);
  };
  let rendered_body = {
    let body_text = body_text.clone();
    move || {
      if extras.get().is_some_and(|extras| extras.is_deleted) {
        return view! { <em class="msg-deleted">{i18n.t("message-deleted")}</em> }.into_any();
      }
      if is_editing.get() {
        return view! {
          <div class="msg-edit">
            <textarea
              class="input"
              rows="3"
              aria-label=i18n.t("action-edit")
              prop:value=move || edit_draft.get()
              on:input=move |ev| edit_draft.set(event_target_value(&ev))
              on:keydown=move |ev| match ev.key().as_str() {
                "Enter" if !ev.shift_key() => { ev.prevent_default(); save_edit(); },
                "Escape" => is_editing.set(false),
                _ => {},
              }
            ></textarea>
            <div class="msg-edit-actions">
              <button class="btn btn-small" on:click=move |_| is_editing.set(false)>{i18n.t("cancel")}</button>
              <button class="btn btn-small btn-primary" on:click=move |_| save_edit()>{i18n.t("save")}</button>
            </div>
          </div>
        }
        .into_any();
      }
      view! { <RichText body=body_text.clone() me=me.username.clone() /> }.into_any()
    }
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
        {header} {forwarded} {quote} <div class="msg-body">
          {rendered_body}
          {edited}
          {badges}
        </div> {attachments} {acknowledge} {reaction_chips} {replies}
      </div>
      {actions}
    </div>
  }
}

/// The i18n key and arguments that describe a workspace event; `None` for events not worth a line.
fn workspace_event_text(
  event: &WorkspaceEvent,
  name_of: impl Fn(crate::model::UserId) -> String,
) -> Option<(&'static str, Vec<(&'static str, String)>)> {
  Some(match event {
    WorkspaceEvent::MemberAdded { user, by, history } => (
      match history {
        HistoryPolicy::Full => "ws-member-added-history",
        _ => "ws-member-added",
      },
      vec![("name", name_of(*user)), ("by", name_of(*by))],
    ),
    WorkspaceEvent::MemberRemoved { user, by } => (
      "ws-member-removed",
      vec![("name", name_of(*user)), ("by", name_of(*by))],
    ),
    WorkspaceEvent::Archived { by } => ("ws-archived", vec![("by", name_of(*by))]),
    WorkspaceEvent::Unarchived { by } => ("ws-unarchived", vec![("by", name_of(*by))]),
    WorkspaceEvent::MadePrivate { by } => ("ws-made-private", vec![("by", name_of(*by))]),
    WorkspaceEvent::MadePublic { by } => ("ws-made-public", vec![("by", name_of(*by))]),
    WorkspaceEvent::Pinned { by, .. } => ("ws-pinned", vec![("by", name_of(*by))]),
    WorkspaceEvent::Unknown => return None,
  })
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
  use crate::model::{ConversationId, MessageId, UserId, id_from_raw};

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

  #[test]
  fn workspace_events_name_who_did_what() {
    let by = id_from_raw::<UserId>(1);
    let user = id_from_raw::<UserId>(2);
    let name_of = |id: UserId| {
      if id == by {
        "Ada".to_owned()
      } else {
        "Sam".to_owned()
      }
    };
    let added = WorkspaceEvent::MemberAdded {
      user,
      by,
      history: HistoryPolicy::Full,
    };
    assert_eq!(
      workspace_event_text(&added, name_of),
      Some((
        "ws-member-added-history",
        vec![("name", "Sam".to_owned()), ("by", "Ada".to_owned())]
      ))
    );
    assert_eq!(workspace_event_text(&WorkspaceEvent::Unknown, name_of), None);
  }

  fn message(id: u64, author: u64, sent_at: i64) -> Message {
    Message {
      id: id_from_raw::<MessageId>(id),
      conversation: id_from_raw::<ConversationId>(1),
      author: id_from_raw::<UserId>(author),
      kind: MessageKind::User,
      sent_at,
      body: String::new(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }
}
