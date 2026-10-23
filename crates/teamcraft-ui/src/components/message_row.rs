use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::actions::{MessagePriority, WorkspaceEvent};
use teamcraft_client_api::log::{LogEntry, log_entries};
use teamcraft_proto::HistoryPolicy;

use crate::clock::{format_time, format_time_seconds, local_day, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::emoji_picker::EmojiPicker;
use crate::components::file_views::{LinkPreviewCard, MessageFiles};
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::MessageMenu;
use crate::components::profile_views::UserHoverCard;
use crate::components::rich_text::RichText;
use crate::delivery::Delivery;
use crate::i18n::use_i18n;
use crate::markdown;
use crate::model::{LogLevel, Message, MessageKind, SystemEvent, User, thread_id_of};
use crate::reactive::use_interval;
use crate::state::{Overlay, use_app_state};

/// Messages by the same author closer together than this share one header.
const GROUP_WINDOW_MS: i64 = 5 * 60_000;
/// How long the pointer rests on a name before its hover card opens.
const HOVER_CARD_DELAY: Duration = Duration::from_millis(450);
/// Room the hover card needs; it opens above the name when there is less below.
const HOVER_CARD_HEIGHT_PX: f64 = 130.0;
/// How long the card stays after the pointer left, so it can move into the card.
const HOVER_CARD_GRACE: Duration = Duration::from_millis(250);
/// How often a message in the outbox re-checks whether it counts as delayed.
const DELIVERY_TICK_MS: u64 = 2_000;
/// Quick reactions on hover until the user has used emoji of their own.
const DEFAULT_QUICK_REACTIONS: [&str; 3] = ["👍", "❤️", "😂"];
/// How many quick reactions the hover toolbar shows.
const QUICK_REACTION_COUNT: usize = 3;

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
  let entries = log_entries(&message.body, level);
  let author = log_author(message.author);
  if let [single] = entries.as_slice() {
    let class = format!("msg msg-log level-{}", single.level.slug());
    return log_entry_line(&time, &author, single, &class, message.sent_at).into_any();
  }
  // A bridge batch: one row per entry under the batch's time.
  let lines: Vec<_> = entries
    .iter()
    .map(|entry| {
      let class = format!("msg-log log-entry level-{}", entry.level.slug());
      log_entry_line(&time, &author, entry, &class, message.sent_at)
    })
    .collect();
  view! {
    <div class="msg msg-log-batch" tabindex="-1" data-nav="row">
      {lines}
    </div>
  }
  .into_any()
}

/// Who wrote a log row: the bridge's bot or, for a pasted message, the person.
pub fn log_author(author: crate::model::UserId) -> String {
  let state = use_app_state();
  state
    .read(|backend| backend.user(author))
    .map(|user| user.display_name)
    .unwrap_or_default()
}

/// One entry of the log view: time, author, level and text in a monospace line.
pub fn log_entry_line(
  time: &str,
  author: &str,
  entry: &LogEntry,
  class: &str,
  sent_at: i64,
) -> impl IntoView + use<> {
  let i18n = use_i18n();
  let level = entry.level;
  let text = entry.text.clone();
  let class = class.to_owned();
  let time = time.to_owned();
  let author = author.to_owned();
  let title = author.clone();
  let tab_index = if class.starts_with("msg ") { "-1" } else { "" };
  let data_nav = if class.starts_with("msg ") { "row" } else { "" };
  view! {
    <div class=class tabindex=tab_index data-nav=data_nav>
      <time class="log-time" datetime=sent_at.to_string()>
        {time}
      </time>
      <span class="log-author" translate="no" title=title>
        {author}
      </span>
      <span class="log-level">{move || i18n.t(&format!("log-level-{}", level.slug()))}</span>
      <span class="log-text" translate="no">
        {text}
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
  let author_id = message.author;
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
  // ↑ in the empty composer asks this row to open its editor.
  Effect::new(move |_| {
    if state.edit_request.get() == Some(message_id) && is_mine {
      state.edit_request.set(None);
      is_editing.set(true);
    }
  });
  let edit_input = NodeRef::<leptos::html::Textarea>::new();
  Effect::new(move |_| {
    if let Some(element) = edit_input.get() {
      drop(element.focus());
      let end = element.value().encode_utf16().count() as u32;
      drop(element.set_selection_range(end, end));
    }
  });

  let gutter = match (is_grouped, author.clone()) {
    (false, Some(user)) => view! { <Avatar user=user size=AvatarSize::Large /> }.into_any(),
    _ => {
      let time = time.clone();
      view! { <time class="hover-time">{time}</time> }.into_any()
    },
  };

  // Hover card: appears when the pointer rests on the name, stays while it moves into the card.
  let hover_card = RwSignal::new(None::<(f64, f64)>);
  let card_timer = StoredValue::new(0_u32);
  let cancel_card_timer = move || {
    card_timer.try_update_value(|generation| *generation += 1);
  };
  let after_delay = move |delay: Duration, action: Box<dyn FnOnce()>| {
    cancel_card_timer();
    let generation = card_timer.try_get_value();
    set_timeout(
      move || {
        if generation.is_some() && card_timer.try_get_value() == generation {
          action();
        }
      },
      delay,
    );
  };
  let show_card_soon = move |anchor: web_sys::Element| {
    after_delay(
      HOVER_CARD_DELAY,
      Box::new(move || {
        let rect = anchor.get_bounding_client_rect();
        let window_height = web_sys::window()
          .and_then(|window| window.inner_height().ok())
          .and_then(|height| height.as_f64())
          .unwrap_or(f64::MAX);
        // Below the name, or above it when the card would leave the window.
        let top = if rect.bottom() + HOVER_CARD_HEIGHT_PX + 8.0 > window_height {
          rect.top() - HOVER_CARD_HEIGHT_PX - 4.0
        } else {
          rect.bottom() + 4.0
        };
        hover_card.try_set(Some((top.max(4.0), rect.left())));
      }),
    );
  };
  let hide_card_soon = move || {
    after_delay(
      HOVER_CARD_GRACE,
      Box::new(move || {
        hover_card.try_set(None);
      }),
    );
  };
  let hide_card_now = move || {
    cancel_card_timer();
    hover_card.set(None);
  };

  let header = (!is_grouped).then(|| {
    let status = author.as_ref().and_then(|user| user.maybe_custom_status.clone());
    let badge_bot = author.as_ref().is_some_and(|user| user.is_bot);
    let badge_guest = author.as_ref().is_some_and(|user| user.is_guest);
    view! {
      <div class="msg-header">
        <button
          type="button"
          class="msg-author"
          translate="no"
          aria-haspopup="dialog"
          on:click=move |_| {
            hide_card_now();
            state.overlay.set(Overlay::UserProfile(author_id));
          }
          on:mouseenter=move |ev| show_card_soon(event_target::<web_sys::Element>(&ev))
          on:mouseleave=move |_| hide_card_soon()
        >
          {author_name.clone()}
        </button>
        {move || hover_card.get().map(|(top, left)| view! {
          <UserHoverCard
            user=author_id
            top=top
            left=left
            on_enter=Callback::new(move |()| cancel_card_timer())
            on_leave=Callback::new(move |()| hide_card_soon())
          />
        })}
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
  let maybe_picker_anchor = RwSignal::new_local(None::<web_sys::Element>);
  let react = move |emoji: &str| {
    state.settings.remember_emoji(emoji);
    state.write(|backend| backend.toggle_reaction(conversation, message_id, emoji));
  };
  let quick_reactions = move || {
    let mut list = state.settings.recent_emoji.get();
    for emoji in DEFAULT_QUICK_REACTIONS {
      if !list.iter().any(|recent| recent == emoji) {
        list.push(emoji.to_owned());
      }
    }
    list.truncate(QUICK_REACTION_COUNT);
    list
  };
  let actions = view! {
    <div class="msg-actions" role="toolbar" aria-label=move || i18n.t("message-actions")>
      <For each=quick_reactions key=|emoji| emoji.clone() let:emoji>
        {
          let emoji_for_label = emoji.clone();
          let label = move || i18n.t_with("action-react-with", &[("emoji", &emoji_for_label)]);
          let chosen = emoji.clone();
          view! {
            <button
              class="icon-btn quick-reaction"
              aria-label=label.clone()
              title=label
              on:click=move |_| react(&chosen)
            >
              {emoji}
            </button>
          }
        }
      </For>
      <button
        class="icon-btn"
        aria-label=move || i18n.t("action-react")
        title=move || i18n.t("action-react")
        aria-haspopup="dialog"
        aria-expanded=move || maybe_picker_anchor.with(Option::is_some).to_string()
        on:click=move |ev| {
          let maybe_button = event_target::<web_sys::Element>(&ev);
          maybe_picker_anchor.set(Some(maybe_button));
        }
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
  // Messages still in the outbox say so; the core retries by itself.
  let sent_at = message.sent_at;
  let delivery = RwSignal::new(Delivery::of(message_id, sent_at, now_ms()));
  if delivery.get_untracked() != Delivery::Sent {
    use_interval(DELIVERY_TICK_MS, move || {
      delivery.set(Delivery::of(message_id, sent_at, now_ms()))
    });
  }
  let delivery_view = move || match delivery.get() {
    Delivery::Sent => None,
    Delivery::Sending => Some(
      view! {
        <span class="msg-delivery" role="status">
          <Icon name=IconName::Clock />
          {move || i18n.t("message-sending")}
        </span>
      }
      .into_any(),
    ),
    Delivery::Delayed => Some(
      view! {
        <span class="msg-delivery msg-delayed" role="status">
          <Icon name=IconName::Alert />
          {move || i18n.t("message-not-sent")}
        </span>
      }
      .into_any(),
    ),
  };
  let is_long_body = markdown::is_long(&body_text);
  let is_expanded = RwSignal::new(false);
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
              node_ref=edit_input
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
      if !is_long_body {
        return view! { <RichText body=body_text.clone() me=me.username.clone() /> }.into_any();
      }
      view! {
        <div class="msg-long" class:collapsed=move || !is_expanded.get()>
          <RichText body=body_text.clone() me=me.username.clone() />
        </div>
        <button
          class="link-btn msg-expand"
          aria-expanded=move || is_expanded.get().to_string()
          on:click=move |_| is_expanded.update(|value| *value = !*value)
        >
          {move || i18n.t(if is_expanded.get() { "message-show-less" } else { "message-show-more" })}
        </button>
      }
      .into_any()
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
      class:msg-highlight=move || state.highlighted.get() == Some(message_id)
      hidden=move || state.hidden_messages.with(|hidden| hidden.contains(&message_id))
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
          {delivery_view}
        </div> {attachments} {acknowledge} {reaction_chips} {replies}
      </div>
      {actions}
      // Outside the toolbar, which is only shown while the message is hovered or focused.
      {move || {
        maybe_picker_anchor
          .get()
          .map(|anchor| {
            view! {
              <EmojiPicker
                anchor=anchor
                on_pick=Callback::new(move |emoji: String| {
                  maybe_picker_anchor.set(None);
                  state.write(|backend| backend.toggle_reaction(conversation, message_id, &emoji));
                })
                on_close=Callback::new(move |()| maybe_picker_anchor.set(None))
              />
            }
          })
      }}
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
    WorkspaceEvent::Moved { by, space } if space.is_empty() => {
      ("ws-moved-unnamed", vec![("by", name_of(*by))])
    },
    WorkspaceEvent::Moved { by, space } => ("ws-moved", vec![("by", name_of(*by)), ("space", space.clone())]),
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
