use std::time::Duration;

use leptos::prelude::*;
use teamcraft_client_api::actions::{FileTransfer, MessagePriority, WorkspaceEvent};
use teamcraft_client_api::log::{LogEntry, log_entries};
use teamcraft_proto::HistoryPolicy;
use wasm_bindgen::JsCast;

use crate::channel_access::can_react_in;
use crate::clock::{format_relative_short, format_time, format_time_seconds, local_day, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::date_jump::DatePill;
use crate::components::file_views::{LinkPreviewCard, MessageFiles};
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::{MessageAction, run_message_action};
use crate::components::message_link_card::MessageLinkPreviews;
use crate::components::message_toolbar::{ActiveMessage, LONG_PRESS, ToolbarHost};
use crate::components::profile_views::UserHoverCard;
use crate::components::rich_text::RichText;
use crate::delivery::{Delivery, ReadIndicator, is_read_by_all};
use crate::feed_motion::{Entrance, RowMotion};
use crate::i18n::use_i18n;
use crate::markdown;
use crate::message_links::{as_permalink, own_hosts};
use crate::model::{LogLevel, Message, MessageKind, SystemEvent, User, UserId, is_pending, thread_id_of};
use crate::reactive::{SetIfAlive, use_interval};
use crate::state::{Overlay, use_app_state};
use crate::viewport::is_touch_primary;

/// Messages by the same author closer together than this share one header.
const GROUP_WINDOW_MS: i64 = 5 * 60_000;
/// How long the pointer rests on a name before its hover card opens.
const HOVER_CARD_DELAY: Duration = Duration::from_millis(450);
/// Room the hover card needs; it opens above the name when there is less below.
const HOVER_CARD_HEIGHT_PX: f64 = 130.0;
/// How long the card stays after the pointer left, so it can move into the card.
const HOVER_CARD_GRACE: Duration = Duration::from_millis(250);
/// Avatars shown in the thread summary under a message.
const MAX_THREAD_AVATARS: usize = 3;
/// How often a message in the outbox re-checks whether it counts as delayed.
const DELIVERY_TICK_MS: u64 = 2_000;
/// A finger that moves further than this is scrolling, not pressing the message.
const LONG_PRESS_SLOP_PX: i32 = 10;
/// The short tick when the action sheet opens, where the device can vibrate (not iOS Safari).
const LONG_PRESS_HAPTIC_MS: u32 = 10;

/// A line's i18n key and its arguments.
type LineText = (&'static str, Vec<(&'static str, String)>);

/// One message in the channel feed or in a thread. The caller recreates it when the message changes.
#[component]
pub fn MessageRow(
  message: Message,
  /// The message right before this one in the same list, used for grouping and date dividers.
  maybe_prev: Option<Message>,
  #[prop(default = false)] in_thread: bool,
  /// How the row enters and which reactions pop (the feed decides, `feed_motion.rs`).
  #[prop(optional)]
  motion: RowMotion,
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
        <DatePill label=label />
      </div>
    }
  });

  let body = match &message.kind {
    MessageKind::Log(level) => log_line(&message, *level, locale).into_any(),
    MessageKind::System(event) => {
      let author = state.read(|backend| backend.user(message.author));
      system_line(&message, event, author, locale).into_any()
    },
    // Decided by the kind, not the extras: a backend that loads them later (the desktop app's)
    // shows the line as soon as they arrive.
    MessageKind::Change => workspace_text_line(workspace_text(&message)).into_any(),
    MessageKind::User => {
      let is_grouped = is_grouped(&message, maybe_prev.as_ref());
      user_message(message, is_grouped, in_thread, motion).into_any()
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
  // Membership changes by somebody else come from the core as workspace events too.
  let workspace_text = workspace_text(message);
  view! {
    {workspace_text_line(workspace_text)}
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

/// The text of `message`'s workspace event (i18n key and arguments), with the names the backend
/// knows; `None` without one.
fn workspace_text(message: &Message) -> Memo<Option<LineText>> {
  let state = use_app_state();
  let (conversation, message_id) = (message.conversation, message.id);
  Memo::new(move |_| {
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
  })
}

/// The line showing a workspace event's `text`, while there is one.
fn workspace_text_line(text: Memo<Option<LineText>>) -> impl IntoView + use<> {
  let i18n = use_i18n();
  move || {
    text.get().map(|(key, args)| {
      let args: Vec<(&str, &str)> = args.iter().map(|(name, value)| (*name, value.as_str())).collect();
      view! {
        <div class="msg msg-system" tabindex="-1" data-nav="row">
          <span class="system-text" translate="no">{i18n.t_with(key, &args)}</span>
        </div>
      }
    })
  }
}

fn user_message(message: Message, is_grouped: bool, in_thread: bool, motion: RowMotion) -> impl IntoView {
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
    let badge_guest = state.shows_guest_markers() && author.as_ref().is_some_and(|user| user.is_guest);
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
        {extras
          .maybe_link_preview
          .clone()
          // A link to a message of this server gets the message's own preview instead.
          .filter(|preview| as_permalink(&preview.url, &own_hosts()).is_none())
          .map(|preview| view! { <LinkPreviewCard preview=preview /> })}
      }
    })
  };
  // Links to other messages: each viewer's client resolves its own preview (#319).
  let link_previews = {
    let body = message.body.clone();
    move || {
      (!extras.get().is_some_and(|extras| extras.is_deleted))
        .then(|| view! { <MessageLinkPreviews body=body.clone() /> })
    }
  };
  let acknowledge = move || {
    extras
      .get()
      .filter(|extras| extras.wants_acknowledgement && !is_mine)
      .map(|extras| {
        let has_acknowledged = extras.acknowledged_by.contains(&me.id);
        view! {
          <button
            class="btn btn-small msg-ack"
            prop:disabled=has_acknowledged
            on:click=move |_| state.write(|backend| backend.acknowledge(conversation, message_id))
          >
            {move || if has_acknowledged { i18n.t("ack-done") } else { i18n.t("ack-confirm") }}
          </button>
        }
      })
  };

  let reaction_chips = (!reactions.is_empty()).then(|| {
    // The counts stay readable for everybody; adding to them takes `add_reaction` (issue #691).
    let can_react = can_react_in(state, conversation);
    let chips: Vec<_> = reactions
      .into_iter()
      .map(|reaction| {
        let emoji = reaction.emoji.clone();
        let label_emoji = reaction.emoji.clone();
        let count = reaction.count;
        // A count that changed again mid-pop continues the pop on this rebuilt chip (#371).
        let maybe_pop = motion.pop_of(&reaction.emoji);
        let is_popping = maybe_pop.is_some();
        let pop_delay = maybe_pop.map(|pop| format!("-{}ms", pop.elapsed.as_millis())).unwrap_or_default();
        view! {
          <button
            class="reaction"
            class:is-popping=is_popping
            style:animation-delay=pop_delay
            class:reaction-mine=reaction.reacted_by_me
            aria-pressed=reaction.reacted_by_me.to_string()
            prop:disabled=!can_react
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

  // Thread summary under the root: who replied, how many replies, when the last one came (#300).
  let replies = (!in_thread && reply_count > 0).then(|| {
    let summary = Memo::new(move |_| {
      state.read(|backend| {
        let replies: Vec<Message> = backend
          .thread(conversation, thread_root)
          .into_iter()
          .filter(|reply| reply.maybe_thread.is_some())
          .collect();
        let maybe_last = replies.iter().map(|reply| reply.sent_at).max();
        let mut participants: Vec<UserId> = Vec::new();
        for reply in replies.iter().rev() {
          if !participants.contains(&reply.author) {
            participants.push(reply.author);
          }
        }
        participants.truncate(MAX_THREAD_AVATARS);
        let users: Vec<User> = participants.into_iter().filter_map(|id| backend.user(id)).collect();
        (users, maybe_last)
      })
    });
    view! {
      <button class="reply-count" on:click=move |_| state.open_thread(conversation, thread_root)>
        <span class="reply-avatars">
          {move || summary.get().0.into_iter().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> }).collect_view()}
        </span>
        <span>{move || i18n.t_count("thread-replies", reply_count as usize)}</span>
        {move || summary.get().1.map(|last| view! {
          <span class="reply-last">{i18n.t_with("thread-last-reply", &[("time", &format_relative_short(last, now_ms(), locale))])}</span>
        })}
      </button>
    }
  });

  let maybe_host = ToolbarHost::use_nearest();
  let active = StoredValue::new(ActiveMessage {
    message: message.clone(),
    in_thread,
    is_mine,
  });
  // A recreated row (new reaction, edit) hands the toolbar its fresh message.
  if let Some(host) = maybe_host {
    host.refresh(active.get_value());
  }
  let show_toolbar = move || {
    if let Some(host) = maybe_host {
      host.show(active.get_value());
    }
  };
  let hide_toolbar = move || {
    if let Some(host) = maybe_host {
      host.hide_soon();
    }
  };
  let is_toolbar_on = move || maybe_host.is_some_and(|host| host.is_active(message_id));
  let long_press = LongPress::new(move || {
    // A deleted message has nothing to act on, as on hover.
    let is_deleted = extras
      .try_with_untracked(|extras| extras.as_ref().is_some_and(|extras| extras.is_deleted))
      .unwrap_or(true);
    if is_deleted {
      return;
    }
    if let Some(host) = maybe_host {
      host.open_sheet(active.get_value());
    }
  });

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
  // A message waits for its files: just the clock while they go up, however long that takes. A
  // file that stopped says so (with "Try again") on its own tile, not "retrying automatically".
  let is_uploading = move || {
    extras.with(|maybe_extras| {
      maybe_extras.as_ref().is_some_and(|extras| {
        extras.files.iter().any(|file| {
          matches!(
            file.transfer,
            FileTransfer::Uploading { .. } | FileTransfer::UploadStalled { .. }
          )
        })
      })
    })
  };
  // Refused by the server for good (no permission, too large): only then is it an error.
  let is_send_failed =
    move || extras.with(|maybe_extras| maybe_extras.as_ref().is_some_and(|extras| extras.is_send_failed));
  // Double ticks in DMs only; group DMs say who read it in "Seen by" (#635). Receipts are empty when
  // either side turned them off, so it then stays at one tick.
  let sent_time = time.clone();
  let is_read = move || {
    state.read(|backend| {
      backend
        .conversation(conversation)
        .filter(|conv| ReadIndicator::of(conv.kind) == ReadIndicator::Ticks)
        .is_some_and(|conv| {
          is_read_by_all(
            &backend.seen_by(conversation, message_id),
            &conv.member_ids,
            me.id,
          )
        })
    })
  };
  let delivery_view = move || {
    if is_send_failed() {
      return Some(
        view! {
          <span class="msg-delivery msg-failed" role="alert">
            <Icon name=IconName::Alert />
            {move || i18n.t("message-send-failed")}
            <button
              class="link-btn"
              on:click=move |_| state.write(|backend| backend.retry_send(conversation, message_id))
            >
              {move || i18n.t("message-send-retry")}
            </button>
            <button
              class="link-btn"
              on:click=move |_| state.write(|backend| backend.discard_unsent(conversation, message_id))
            >
              {move || i18n.t("message-send-discard")}
            </button>
          </span>
        }
        .into_any(),
      );
    }
    match delivery.get() {
      // Own messages on the server: one tick, two blue ones once the DM partner read it (#568, #635).
      Delivery::Sent if is_mine => {
        let is_read = is_read();
        let sent_time = sent_time.clone();
        let key = if is_read {
          "message-status-read"
        } else {
          "message-status-sent"
        };
        let icon = if is_read {
          IconName::CheckDouble
        } else {
          IconName::Check
        };
        Some(
          view! {
            <span
              class="msg-delivery is-sent"
              class:is-read=is_read
              role="img"
              aria-label=move || i18n.t(key)
              data-tip=move || format!("{}, {}", i18n.t(key), sent_time)
            >
              <Icon name=icon />
            </span>
          }
          .into_any(),
        )
      },
      Delivery::Sent => None,
      // Unconfirmed: a small clock, retried in the background (offline, reconnect, reload).
      Delivery::Sending => Some(
        view! {
          <span
            class="msg-delivery is-pending"
            role="status"
            aria-label=move || i18n.t("message-sending")
            data-tip=move || i18n.t("message-sending")
          >
            <Icon name=IconName::Clock />
          </span>
        }
        .into_any(),
      ),
      Delivery::Delayed if is_uploading() => Some(
        view! {
          <span class="msg-delivery is-pending" role="status" aria-label=move || i18n.t("message-sending")>
            <Icon name=IconName::Clock />
          </span>
        }
        .into_any(),
      ),
      Delivery::Delayed => Some(
        view! {
          <span class="msg-delivery is-pending msg-delayed" role="status">
            <Icon name=IconName::Clock />
            {move || i18n.t("message-not-sent")}
          </span>
        }
        .into_any(),
      ),
    }
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
                "Escape" => {
                  // Only the edit is cancelled; the feed doesn't take it as "mark read".
                  ev.prevent_default();
                  is_editing.set(false);
                },
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

  // Shortcuts of a focused message (shown in the toolbar's tooltips).
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.target() != ev.current_target() || ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
      return;
    }
    let key = ev.key();
    let maybe_action = match key.as_str() {
      "Enter" | "t" | "T" if !in_thread => Some(MessageAction::Thread),
      "r" | "R" => Some(MessageAction::Quote),
      _ => None,
    };
    if let Some(action) = maybe_action {
      ev.prevent_default();
      active
        .with_value(|active| run_message_action(action, &active.message, extras.get_untracked().as_ref()));
      return;
    }
    let Some(host) = maybe_host else { return };
    match key.as_str() {
      "e" | "E" => {
        ev.prevent_default();
        host.open_picker(active.get_value());
      },
      "ContextMenu" => {
        ev.prevent_default();
        host.open_more(active.get_value());
      },
      "F10" if ev.shift_key() => {
        ev.prevent_default();
        host.open_more(active.get_value());
      },
      _ => {},
    }
  };

  view! {
    <div
      class="msg msg-user"
      class:msg-grouped=is_grouped
      class:msg-mine=is_mine
      class:msg-pending=is_pending(message_id)
      class:msg-arriving=motion.entrance == Entrance::Arriving
      class:msg-settling=motion.entrance == Entrance::Settling
      class:msg-active=is_toolbar_on
      class:is-pressing=move || long_press.is_pressing.get()
      class:msg-highlight=move || state.highlighted.get() == Some(message_id)
      hidden=move || state.hidden_messages.with(|hidden| hidden.contains(&message_id))
      tabindex="-1"
      data-nav="row"
      data-msg-id=message_id.to_string()
      aria-label=aria_label
      on:keydown=on_keydown
      on:pointerenter=move |ev| {
        // A finger has no hover (#364): tap does nothing, a long press opens the sheet.
        let is_deleted = extras.with_untracked(|extras| extras.as_ref().is_some_and(|extras| extras.is_deleted));
        if ev.pointer_type() != "touch" && !is_deleted {
          show_toolbar();
        }
      }
      on:mouseleave=move |_| hide_toolbar()
      on:focusin=move |ev| {
        // A tap focuses the row too; only the keyboard brings the toolbar then.
        let is_keyboard = event_target::<web_sys::Element>(&ev)
          .matches(":focus-visible")
          .unwrap_or(true);
        if let Some(host) = maybe_host
          && (!long_press.was_touch() || is_keyboard)
        {
          host.focus(active.get_value());
        }
      }
      on:focusout=move |_| hide_toolbar()
      on:pointerdown=move |ev| long_press.start(&ev)
      on:pointermove=move |ev| long_press.track(&ev)
      on:pointerup=move |_| long_press.cancel()
      on:pointercancel=move |_| long_press.cancel()
      on:contextmenu=move |ev: web_sys::MouseEvent| {
        // The browser's own long-press menu would cover the action sheet.
        if long_press.has_fired() || is_touch_primary() {
          ev.prevent_default();
          return;
        }
        // A right click in the desktop app shows the message's actions (#719); text keeps the text
        // menu, links the link menu, files their own (they stop the event before it gets here).
        let is_on_link = event_target::<web_sys::Element>(&ev).closest("a[href]").ok().flatten().is_some();
        if !crate::auth::tauri::is_available()
          || is_on_link
          || crate::external_links::is_on_text(&ev)
          || crate::external_links::wants_native_menu()
        {
          return;
        }
        let is_deleted = extras
          .try_with_untracked(|extras| extras.as_ref().is_some_and(|extras| extras.is_deleted))
          .unwrap_or(true);
        if let Some(host) = maybe_host
          && !is_deleted
        {
          ev.prevent_default();
          host.open_more(active.get_value());
        }
      }
    >
      <div class="msg-gutter">{gutter}</div>
      <div class="msg-main">
        <div class="msg-bubble">
          {header} {forwarded} {quote} <div class="msg-body">
            {rendered_body}
            {edited}
            {badges}
            // Time first, the ticks at the far end, as in messaging apps (#735).
            <span class="bubble-meta">
              <time class="bubble-time" datetime=message.sent_at.to_string()>
                {time.clone()}
              </time>
              {delivery_view}
            </span>
          </div> {attachments} {link_previews} {acknowledge}
        </div>
        {reaction_chips} {replies}
      </div>
    </div>
  }
}

/// A finger resting on a message opens its action sheet; moving it (scrolling) or lifting it
/// early does not. The row tints from the first touch, so the press is felt before the sheet
/// comes (#364).
#[derive(Clone, Copy)]
struct LongPress {
  /// Bumped by every new press and cancel, so a timer of an earlier press does nothing.
  generation: StoredValue<u32>,
  maybe_origin: StoredValue<Option<(i32, i32)>>,
  has_fired: StoredValue<bool>,
  /// The last press on the row was a finger (not a mouse or pen).
  was_touch: StoredValue<bool>,
  is_pressing: RwSignal<bool>,
  on_fire: StoredValue<Box<dyn Fn() + Send + Sync>>,
}

impl LongPress {
  fn new(on_fire: impl Fn() + Send + Sync + 'static) -> Self {
    Self {
      generation: StoredValue::new(0),
      maybe_origin: StoredValue::new(None),
      has_fired: StoredValue::new(false),
      was_touch: StoredValue::new(false),
      is_pressing: RwSignal::new(false),
      on_fire: StoredValue::new(Box::new(on_fire)),
    }
  }

  fn start(&self, ev: &web_sys::PointerEvent) {
    self.has_fired.set_value(false);
    self.was_touch.set_value(ev.pointer_type() == "touch");
    // Pictures have their own long-press actions (the media toolbar).
    let is_on_picture = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|element| element.closest(".media-tile").ok().flatten())
      .is_some();
    if ev.pointer_type() != "touch" || is_on_picture {
      return;
    }
    let generation = self.bump();
    self.maybe_origin.set_value(Some((ev.client_x(), ev.client_y())));
    self.is_pressing.set(true);
    let press = *self;
    set_timeout(
      move || {
        if press.generation.try_get_value() == Some(generation) {
          press.has_fired.try_set_value(true);
          press.maybe_origin.try_set_value(None);
          press.is_pressing.set_if_alive(false);
          press.on_fire.try_with_value(|fire| fire());
          tick_haptic();
        }
      },
      LONG_PRESS,
    );
  }

  fn track(&self, ev: &web_sys::PointerEvent) {
    let Some((x, y)) = self.maybe_origin.get_value() else {
      return;
    };
    if (ev.client_x() - x).abs() > LONG_PRESS_SLOP_PX || (ev.client_y() - y).abs() > LONG_PRESS_SLOP_PX {
      self.cancel();
    }
  }

  fn cancel(&self) {
    self.bump();
    self.maybe_origin.set_value(None);
    self.is_pressing.set(false);
  }

  fn has_fired(&self) -> bool {
    self.has_fired.try_get_value().unwrap_or(false)
  }

  fn was_touch(&self) -> bool {
    self.was_touch.try_get_value().unwrap_or(false)
  }

  fn bump(&self) -> u32 {
    let mut next = 0;
    self.generation.update_value(|generation| {
      *generation = generation.wrapping_add(1);
      next = *generation;
    });
    next
  }
}

/// A short vibration where the device offers one; without it the sheet itself is the feedback.
fn tick_haptic() {
  let Some(window) = web_sys::window() else {
    return;
  };
  let navigator = window.navigator();
  // WebKit (iOS Safari, WKWebView, WebKitGTK) has no `vibrate`; calling it would throw.
  if !js_sys::Reflect::has(&navigator, &"vibrate".into()).unwrap_or(false) {
    return;
  }
  // Refused (no permission yet, no motor): nothing to tell the user.
  let _ = navigator.vibrate_with_duration(LONG_PRESS_HAPTIC_MS);
}

/// The i18n key and arguments that describe a workspace event; `None` for events not worth a line.
fn workspace_event_text(
  event: &WorkspaceEvent,
  name_of: impl Fn(crate::model::UserId) -> String,
) -> Option<LineText> {
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
    // Changes the server reports (issue #818): who made them only when the line names somebody.
    WorkspaceEvent::Archived { maybe_by } => {
      by_or_server(*maybe_by, ("ws-archived", "ws-archived-unattributed"), &name_of)
    },
    WorkspaceEvent::Unarchived { maybe_by } => by_or_server(
      *maybe_by,
      ("ws-unarchived", "ws-unarchived-unattributed"),
      &name_of,
    ),
    WorkspaceEvent::MadePrivate { maybe_by } => by_or_server(
      *maybe_by,
      ("ws-made-private", "ws-made-private-unattributed"),
      &name_of,
    ),
    WorkspaceEvent::MadePublic { maybe_by } => by_or_server(
      *maybe_by,
      ("ws-made-public", "ws-made-public-unattributed"),
      &name_of,
    ),
    WorkspaceEvent::Pinned { by, .. } => ("ws-pinned", vec![("by", name_of(*by))]),
    WorkspaceEvent::AdminJoined {
      user,
      maybe_approved_by: Some(by),
      ..
    } => (
      "ws-admin-joined-approved",
      vec![("name", name_of(*user)), ("by", name_of(*by))],
    ),
    WorkspaceEvent::AdminJoined {
      user,
      maybe_added_by: Some(by),
      ..
    } => (
      "ws-admin-joined-added",
      vec![("name", name_of(*user)), ("by", name_of(*by))],
    ),
    WorkspaceEvent::AdminJoined { user, .. } => ("ws-admin-joined", vec![("name", name_of(*user))]),
    WorkspaceEvent::Moved { maybe_by, space } if space.is_empty() => by_or_server(
      *maybe_by,
      ("ws-moved-unnamed", "ws-moved-unnamed-unattributed"),
      &name_of,
    ),
    WorkspaceEvent::Moved { maybe_by, space } => {
      let (key, mut args) = by_or_server(*maybe_by, ("ws-moved", "ws-moved-unattributed"), &name_of);
      args.push(("space", space.clone()));
      (key, args)
    },
    WorkspaceEvent::Unknown => return None,
  })
}

/// A change's line: the first of `keys` with who made it when somebody is named (`maybe_by`), else
/// the second, its wording as the server's statement ("This channel was archived", issue #818).
fn by_or_server(
  maybe_by: Option<crate::model::UserId>,
  (named, unattributed): (&'static str, &'static str),
  name_of: &impl Fn(crate::model::UserId) -> String,
) -> LineText {
  match maybe_by {
    Some(by) => (named, vec![("by", name_of(by))]),
    None => (unattributed, Vec::new()),
  }
}

/// Whether `message` goes under the header of the one before: only a message people wrote has
/// one (not a line of a change, [`MessageKind::Change`]).
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

  /// The line of a change (archived, an admin who joined) has no header to go under; a message
  /// without text (a deleted one) still has one.
  #[test]
  fn nothing_is_grouped_under_the_line_of_a_change() {
    let line = Message {
      kind: MessageKind::Change,
      body: String::new(),
      ..message(1, 1, 0)
    };
    assert!(!is_grouped(&message(2, 1, 60_000), Some(&line)));
    let without_text = Message {
      body: String::new(),
      ..message(3, 1, 0)
    };
    assert!(is_grouped(&message(4, 1, 60_000), Some(&without_text)));
  }

  /// Review of #815 (M1): the desktop app's backend answers extras later than the message. The row
  /// of a change is a line by its kind, and reads its text once the extras arrive.
  #[test]
  fn a_change_reads_its_line_once_the_extras_arrive() {
    use std::rc::Rc;

    use crate::backend::Backend;
    use crate::mock::MockBackend;
    use crate::settings::Settings;
    use crate::state::AppState;

    let mock = Rc::new(MockBackend::new(0));
    let backend: Rc<dyn Backend> = mock.clone();
    let state = AppState::new(backend, Settings::for_tests(), None);
    let owner = Owner::new();
    owner.with(|| {
      provide_context(state);
      let change = Message {
        kind: MessageKind::Change,
        body: String::new(),
        ..message(77_001, 1, 0)
      };
      let text = workspace_text(&change);
      assert_eq!(text.get_untracked(), None, "no extras yet");

      mock.set_workspace_event(
        change.id,
        WorkspaceEvent::AdminJoined {
          user: id_from_raw::<UserId>(2),
          maybe_approved_by: None,
          maybe_added_by: Some(id_from_raw::<UserId>(1)),
        },
      );
      state.backend_changed();
      assert_eq!(
        text.get_untracked().map(|(key, _)| key),
        Some("ws-admin-joined-added")
      );
    });
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
    let approved = WorkspaceEvent::AdminJoined {
      user,
      maybe_approved_by: Some(by),
      maybe_added_by: None,
    };
    assert_eq!(
      workspace_event_text(&approved, name_of),
      Some((
        "ws-admin-joined-approved",
        vec![("name", "Sam".to_owned()), ("by", "Ada".to_owned())]
      ))
    );
    // Review of #815 (L2): no "approved" where nobody asked.
    let added = WorkspaceEvent::AdminJoined {
      user,
      maybe_approved_by: None,
      maybe_added_by: Some(by),
    };
    assert_eq!(
      workspace_event_text(&added, name_of),
      Some((
        "ws-admin-joined-added",
        vec![("name", "Sam".to_owned()), ("by", "Ada".to_owned())]
      ))
    );
    let own = WorkspaceEvent::AdminJoined {
      user,
      maybe_approved_by: None,
      maybe_added_by: None,
    };
    assert_eq!(
      workspace_event_text(&own, name_of),
      Some(("ws-admin-joined", vec![("name", "Sam".to_owned())]))
    );
  }

  /// Issue #818: a change the server reports names who made it only when the line does; else it
  /// is worded as the server's statement, with no name at all.
  #[test]
  fn server_changes_name_nobody_unless_the_line_does() {
    let by = id_from_raw::<UserId>(1);
    let name_of = |_: UserId| "Ada".to_owned();
    let cases = [
      (
        WorkspaceEvent::Archived { maybe_by: None },
        "ws-archived-unattributed",
      ),
      (
        WorkspaceEvent::Unarchived { maybe_by: None },
        "ws-unarchived-unattributed",
      ),
      (
        WorkspaceEvent::MadePrivate { maybe_by: None },
        "ws-made-private-unattributed",
      ),
      (
        WorkspaceEvent::MadePublic { maybe_by: None },
        "ws-made-public-unattributed",
      ),
      (
        WorkspaceEvent::Moved {
          maybe_by: None,
          space: String::new(),
        },
        "ws-moved-unnamed-unattributed",
      ),
    ];
    for (event, key) in cases {
      assert_eq!(
        workspace_event_text(&event, name_of),
        Some((key, Vec::new())),
        "{event:?}"
      );
    }
    assert_eq!(
      workspace_event_text(
        &WorkspaceEvent::Moved {
          maybe_by: None,
          space: "Project B".into()
        },
        name_of
      ),
      Some(("ws-moved-unattributed", vec![("space", "Project B".to_owned())]))
    );
    assert_eq!(
      workspace_event_text(&WorkspaceEvent::Archived { maybe_by: Some(by) }, name_of),
      Some(("ws-archived", vec![("by", "Ada".to_owned())]))
    );
    assert_eq!(
      workspace_event_text(
        &WorkspaceEvent::Moved {
          maybe_by: Some(by),
          space: "Project B".into()
        },
        name_of
      ),
      Some((
        "ws-moved",
        vec![("by", "Ada".to_owned()), ("space", "Project B".to_owned())]
      ))
    );
  }

  fn message(id: u64, author: u64, sent_at: i64) -> Message {
    Message {
      id: id_from_raw::<MessageId>(id),
      conversation: id_from_raw::<ConversationId>(1),
      author: id_from_raw::<UserId>(author),
      kind: MessageKind::User,
      sent_at,
      body: format!("message {id}"),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    }
  }
}
