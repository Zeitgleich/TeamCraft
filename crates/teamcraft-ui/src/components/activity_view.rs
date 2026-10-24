//! The activity feed: mentions, reactions to the user's messages and replies in their threads, in
//! one list with an unread state, with tabs per kind. An entry jumps to the message.

use leptos::prelude::*;
use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// The tabs of the feed: everything, or one kind of activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityTab {
  All,
  /// Activity in DMs and group DMs.
  Direct,
  Mentions,
  /// Replies in threads the user takes part in.
  Threads,
}

impl ActivityTab {
  const ALL: [Self; 4] = [Self::All, Self::Direct, Self::Mentions, Self::Threads];

  fn label_key(self) -> &'static str {
    match self {
      Self::All => "activity-all",
      Self::Direct => "activity-direct",
      Self::Mentions => "activity-mentions",
      Self::Threads => "activity-threads",
    }
  }

  fn includes(self, item: &ActivityItem, is_direct: impl Fn() -> bool) -> bool {
    match self {
      Self::All => true,
      Self::Direct => is_direct(),
      Self::Mentions => item.kind == ActivityKind::Mention,
      Self::Threads => item.kind == ActivityKind::ThreadReply,
    }
  }
}

#[component]
pub fn ActivityView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let tab = RwSignal::new(ActivityTab::All);
  let is_unread_only = RwSignal::new(false);
  let feed = Memo::new(move |_| state.read(|backend| backend.activity()));
  let items = Memo::new(move |_| {
    let selected = tab.get();
    let unread_only = is_unread_only.get();
    feed
      .get()
      .items
      .into_iter()
      .filter(|item| !unread_only || item.is_unread)
      .filter(|item| {
        selected.includes(item, || {
          state
            .read(|backend| backend.conversation(item.conversation))
            .is_some_and(|conversation| conversation.kind.is_direct_like())
        })
      })
      .collect::<Vec<_>>()
  });

  view! {
    <section class="activity-view" aria-label=move || i18n.t("activity-title")>
      <header class="panel-header">
        <div class="panel-title">
          <h1>{move || i18n.t("activity-title")}</h1>
        </div>
        <button
          class="btn btn-small"
          aria-pressed=move || is_unread_only.get().to_string()
          on:click=move |_| is_unread_only.update(|value| *value = !*value)
        >
          {move || i18n.t("activity-unread")}
        </button>
        <button
          class="btn btn-small"
          prop:disabled=move || feed.get().unread_count == 0
          on:click=move |_| state.write(|backend| backend.mark_activity_read())
        >
          {move || i18n.t("activity-mark-read")}
        </button>
      </header>
      <div class="view-tabs" role="tablist" aria-label=move || i18n.t("activity-filter")>
        {ActivityTab::ALL
          .into_iter()
          .map(|value| {
            view! {
              <button
                role="tab"
                aria-selected=move || (tab.get() == value).to_string()
                on:click=move |_| tab.set(value)
              >
                {move || i18n.t(value.label_key())}
              </button>
            }
          })
          .collect_view()}
      </div>
      <ul class="activity-list panel-scroll" role="tabpanel">
        <For each=move || items.get() key=|item| (item.id.clone(), item.is_unread) let:item>
          <ActivityRow item=item />
        </For>
        <Show when=move || items.get().is_empty() fallback=|| ()>
          <EmptyState icon=IconName::Check title_key="activity-empty-title" body_key="activity-empty" as_item=true />
        </Show>
      </ul>
    </section>
  }
}

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
    .map(|conversation| conversation.name)
    .unwrap_or_default();
  let summary_key = match &item.kind {
    ActivityKind::Mention => "activity-mentioned",
    ActivityKind::Reaction { .. } => "activity-reacted",
    ActivityKind::ThreadReply => "activity-replied",
  };
  let emoji = match &item.kind {
    ActivityKind::Reaction { emoji } => emoji.clone(),
    _ => String::new(),
  };
  let summary = move || {
    i18n.t_with(
      summary_key,
      &[
        ("name", &actor_name),
        ("channel", &conversation_name),
        ("emoji", &emoji),
      ],
    )
  };
  let (conversation, message, maybe_thread) = (item.conversation, item.message, item.maybe_thread);
  let relative = format_relative(item.at, now_ms(), locale);
  let absolute = format_date_time(item.at, locale);
  let is_unread = item.is_unread;

  view! {
    <li>
      <button
        class="activity-item"
        class:unread=is_unread
        on:click=move |_| state.open_message(conversation, message, maybe_thread)
      >
        <span class="activity-avatar">
          {actor.map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
        </span>
        <span class="activity-body">
          <span class="activity-summary" translate="no">{summary}</span>
          <span class="activity-excerpt">{item.excerpt.clone()}</span>
        </span>
        <time class="activity-time" title=absolute>{relative}</time>
        {is_unread.then(|| view! {
          <span class="unread-dot" aria-hidden="true"></span>
          <span class="visually-hidden">{move || i18n.t("activity-unread-mark")}</span>
        })}
        <Icon name=IconName::Bell class="activity-kind" />
      </button>
    </li>
  }
}
