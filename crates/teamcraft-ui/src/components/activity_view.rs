//! The activity feed: mentions, reactions to the user's messages and replies in their threads, in
//! one list with an unread state. An entry jumps to the message.

use leptos::prelude::*;
use teamcraft_client_api::activity::{ActivityItem, ActivityKind};

use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// What the user sees of the feed in one go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityFilter {
  All,
  Unread,
  Mentions,
}

impl ActivityFilter {
  fn includes(self, item: &ActivityItem) -> bool {
    match self {
      Self::All => true,
      Self::Unread => item.is_unread,
      Self::Mentions => item.kind == ActivityKind::Mention,
    }
  }
}

#[component]
pub fn ActivityView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(ActivityFilter::All);
  let feed = Memo::new(move |_| state.read(|backend| backend.activity()));
  let items = Memo::new(move |_| {
    let selected = filter.get();
    feed
      .get()
      .items
      .into_iter()
      .filter(|item| selected.includes(item))
      .collect::<Vec<_>>()
  });
  let filter_button = move |value: ActivityFilter, label: &'static str| {
    view! {
      <button aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value)>
        {move || i18n.t(label)}
      </button>
    }
  };

  view! {
    <section class="activity-view" aria-label=move || i18n.t("activity-title")>
      <header class="panel-header">
        <div class="panel-title">
          <h1>{move || i18n.t("activity-title")}</h1>
        </div>
        <div class="segmented" role="group" aria-label=move || i18n.t("activity-filter")>
          {filter_button(ActivityFilter::All, "activity-all")}
          {filter_button(ActivityFilter::Unread, "activity-unread")}
          {filter_button(ActivityFilter::Mentions, "activity-mentions")}
        </div>
        <button
          class="btn btn-small"
          prop:disabled=move || feed.get().unread_count == 0
          on:click=move |_| state.write(|backend| backend.mark_activity_read())
        >
          {move || i18n.t("activity-mark-read")}
        </button>
      </header>
      <ul class="activity-list panel-scroll">
        <For each=move || items.get() key=|item| (item.id.clone(), item.is_unread) let:item>
          <ActivityRow item=item />
        </For>
        <Show when=move || items.get().is_empty() fallback=|| ()>
          <li class="empty-state">{move || i18n.t("activity-empty")}</li>
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
