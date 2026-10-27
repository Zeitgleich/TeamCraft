//! Activity, a place of its own (#752, activity_place.rs): its list takes the sidebar's place and
//! says for each entry what happened and where; the entry opens beside it. Filters: everything,
//! mentions, the threads the user follows, reactions.

use leptos::prelude::*;
use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

use crate::activity_place::{ActivityFilter, summary_key, thread_summary_key};
use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::join_requests::ActivityJoinRequests;
use crate::components::rich_text::RichText;
use crate::i18n::use_i18n;
use crate::model::{Conversation, ThreadSummary, thread_id_of};
use crate::state::{MainView, SidePanel, use_app_state};

/// The activity list in the sidebar's place: filters, then the entries.
#[component]
pub fn ActivitySidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = state.activity_filter;
  let is_unread_only = RwSignal::new(false);
  let feed = Memo::new(move |_| state.read(|backend| backend.activity()));
  let items = Memo::new(move |_| {
    let selected = filter.get();
    let unread_only = is_unread_only.get();
    feed
      .get()
      .items
      .into_iter()
      .filter(|item| !unread_only || item.is_unread)
      .filter(|item| selected.includes(item))
      .collect::<Vec<_>>()
  });
  let threads = Memo::new(move |_| {
    let mut all = state.read(|backend| backend.followed_threads());
    if is_unread_only.get() {
      all.retain(|thread| thread.unread_replies > 0);
    }
    all
  });
  // Join requests wait above the entries of "All" (they are no mention, reaction or reply).
  let shows_requests = move || filter.get() == ActivityFilter::All;
  let is_empty = move || {
    if filter.get().lists_threads() {
      threads.with(Vec::is_empty)
    } else {
      items.with(Vec::is_empty) && !(shows_requests() && feed.with(|feed| !feed.join_requests.is_empty()))
    }
  };

  view! {
    <nav class="sidebar activity-sidebar" aria-label=move || i18n.t("activity-title")>
      <div class="sidebar-head">
        <h2 class="space-name">{move || i18n.t("activity-title")}</h2>
        <button
          class="icon-btn"
          aria-label=move || i18n.t("activity-mark-read")
          title=move || i18n.t("activity-mark-read")
          prop:disabled=move || feed.get().unread_count == 0
          on:click=move |_| state.write(|backend| backend.mark_activity_read())
        >
          <Icon name=IconName::CheckDouble />
        </button>
      </div>
      <div class="home-chips" role="group" aria-label=move || i18n.t("activity-filter")>
        {ActivityFilter::ALL
          .into_iter()
          .map(|value| {
            view! {
              <button
                type="button"
                class="home-chip"
                aria-pressed=move || (filter.get() == value).to_string()
                on:click=move |_| filter.set(value)
              >
                {move || i18n.t(value.label_key())}
              </button>
            }
          })
          .collect_view()}
        <span class="home-chips-divider" aria-hidden="true"></span>
        <button
          type="button"
          class="home-chip"
          aria-pressed=move || is_unread_only.get().to_string()
          on:click=move |_| is_unread_only.update(|value| *value = !*value)
        >
          {move || i18n.t("activity-unread")}
        </button>
      </div>
      <div class="sidebar-scroll">
        <Show when=shows_requests fallback=|| ()>
          <ActivityJoinRequests />
        </Show>
        <ul class="activity-list">
          {move || {
            if filter.get().lists_threads() {
              view! {
                <For each=move || threads.get() key=|thread| (thread.root.id, thread.reply_count, thread.unread_replies) let:thread>
                  <ThreadRow thread=thread />
                </For>
              }
              .into_any()
            } else {
              view! {
                <For each=move || items.get() key=|item| (item.id.clone(), item.is_unread) let:item>
                  <ActivityRow item=item />
                </For>
              }
              .into_any()
            }
          }}
        </ul>
        {move || {
          is_empty().then(|| {
            let (title_key, body_key) = filter.get().empty_keys();
            let icon = if filter.get().lists_threads() { IconName::Threads } else { IconName::Bell };
            view! { <div class="activity-empty"><EmptyState icon=icon title_key=title_key body_key=body_key /></div> }
          })
        }}
      </div>
    </nav>
  }
}

/// The main column while no entry is open: what Activity is for.
#[component]
pub fn ActivityHome() -> impl IntoView {
  view! {
    <section class="activity-home">
      <EmptyState icon=IconName::Bell title_key="activity-place-title" body_key="activity-place-body" />
    </section>
  }
}

/// A mention, reaction or reply: who did what where, the start of the message, when.
#[component]
fn ActivityRow(item: ActivityItem) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let actor = state.read(|backend| backend.user(item.actor));
  let actor_name = actor
    .as_ref()
    .map(|user| user.display_name.clone())
    .unwrap_or_default();
  let conversation_name = state
    .read(|backend| backend.conversation(item.conversation))
    .map(|conversation| place_name(&conversation))
    .unwrap_or_default();
  let key = summary_key(&item.kind);
  let emoji = match &item.kind {
    ActivityKind::Reaction { emoji } => emoji.clone(),
    _ => String::new(),
  };
  let summary = move || {
    i18n.t_with(
      key,
      &[
        ("name", &actor_name),
        ("channel", &conversation_name),
        ("emoji", &emoji),
      ],
    )
  };
  let (conversation, message, maybe_thread) = (item.conversation, item.message, item.maybe_thread);
  let maybe_read_through = item.maybe_read_through;
  let relative = format_relative(item.at, now_ms(), locale);
  let absolute = format_date_time(item.at, locale);
  let is_unread = item.is_unread;
  let is_current = move || {
    state.view.get() == MainView::Conversation(conversation)
      && match maybe_thread {
        Some(thread) => state.panel.get() == SidePanel::Thread { conversation, thread },
        None => state.panel.get() == SidePanel::Closed,
      }
  };

  view! {
    <li>
      <button
        class="activity-item"
        class:unread=is_unread
        class:current=is_current
        aria-current=move || is_current().then_some("true")
        on:click=move |_| {
          state.open_from_activity(conversation, message, maybe_thread);
          // Opening is reading: the entry's target moves its read position up to the entry (a
          // thread's panel does it for thread entries), and the unread state follows from that.
          if maybe_thread.is_none()
            && let Some(read_through) = maybe_read_through
          {
            state.write(|backend| backend.mark_read_through(conversation, read_through));
          }
        }
      >
        <span class="activity-avatar">
          {actor.map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
        </span>
        <span class="activity-body">
          <span class="activity-summary" translate="no">{summary}</span>
          <span class="activity-excerpt">{item.excerpt.clone()}</span>
          <time class="activity-time" title=absolute>{relative}</time>
        </span>
        {is_unread.then(|| view! {
          <span class="unread-dot" aria-hidden="true"></span>
          <span class="visually-hidden">{move || i18n.t("activity-unread-mark")}</span>
        })}
      </button>
    </li>
  }
}

/// A thread the user follows: its new replies and where, the start of its first message.
#[component]
fn ThreadRow(thread: ThreadSummary) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let author = state.read(|backend| backend.user(thread.root.author));
  let conversation_name = state
    .read(|backend| backend.conversation(thread.conversation))
    .map(|conversation| place_name(&conversation))
    .unwrap_or_default();
  let (conversation, root) = (thread.conversation, thread.root.id);
  let thread_id = thread_id_of(root);
  let unread = thread.unread_replies;
  let key = thread_summary_key(unread);
  let summary = move || i18n.t_count_with(key, unread as usize, &[("channel", &conversation_name)]);
  let relative = format_relative(thread.last_reply_at, now_ms(), locale);
  let absolute = format_date_time(thread.last_reply_at, locale);
  let is_current = move || {
    state.panel.get()
      == SidePanel::Thread {
        conversation,
        thread: thread_id,
      }
  };

  view! {
    <li>
      <button
        class="activity-item"
        class:unread={unread > 0}
        class:current=is_current
        aria-current=move || is_current().then_some("true")
        on:click=move |_| state.open_from_activity(conversation, root, Some(thread_id))
      >
        <span class="activity-avatar">
          {author.map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
        </span>
        <span class="activity-body">
          <span class="activity-summary" translate="no">{summary}</span>
          <span class="activity-excerpt"><RichText body=thread.root.body.clone() /></span>
          <time class="activity-time" title=absolute>{relative}</time>
        </span>
        {(unread > 0).then(|| view! {
          <span class="unread-dot" class:mention=thread.has_mention aria-hidden="true"></span>
          <span class="visually-hidden">{move || i18n.t("activity-unread-mark")}</span>
        })}
      </button>
    </li>
  }
}

/// Where something happened, as a sentence names it: "#general", or the chat's name.
fn place_name(conversation: &Conversation) -> String {
  if conversation.kind.is_direct_like() {
    conversation.name.clone()
  } else {
    format!("#{}", conversation.name)
  }
}
