//! Log view of integration channels: one row per bridge batch entry in monospace, coloured by level,
//! with a filter by text and minimum level, follow mode and a per-channel switch to the plain chat
//! feed. Unfiltered, it reuses the virtualized [`MessageList`], which scales to huge channels;
//! with a filter it scans the newest part of the history (bounded) for matching entries.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::log::{LogEntry, LogFilter, log_entries};

use crate::backend::Backend;
use crate::clock::format_time_seconds;
use crate::components::message_list::MessageList;
use crate::components::message_row::log_entry_line;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ConversationKind, LogLevel, MessageKind, TimestampMs};
use crate::state::use_app_state;

/// Messages read per backend call while scanning for matches.
const SCAN_CHUNK: usize = 500;
/// How many of the newest messages a filtered view looks at.
const SCAN_LIMIT: usize = 20_000;
/// How many matching entries a filtered view renders.
const MAX_SHOWN_ENTRIES: usize = 2_000;
const MODE_STORAGE_PREFIX: &str = "teamcraft.log-view.";

/// How an integration channel is shown. Chosen per channel and remembered in the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedMode {
  Log,
  Chat,
}

impl FeedMode {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Log => "log",
      Self::Chat => "chat",
    }
  }

  pub fn from_slug(slug: &str) -> Self {
    match slug {
      "chat" => Self::Chat,
      _ => Self::Log,
    }
  }
}

/// The matching entries of a channel, oldest first, and whether more matched than were kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
  pub entries: Vec<(TimestampMs, LogEntry)>,
  pub is_truncated: bool,
}

/// The message feed of a conversation: plain for most channels; integration channels get the log
/// toolbar and, when filtering, the filtered log.
#[component]
pub fn ChannelFeed(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let is_integration = state
    .read(|backend| backend.conversation(conversation))
    .is_some_and(|conv| conv.kind == ConversationKind::Integration);
  if !is_integration {
    return view! { <MessageList conversation=conversation /> }.into_any();
  }
  let mode = RwSignal::new(load_mode(conversation));
  let filter = RwSignal::new(LogFilter::default());
  let follow = RwSignal::new(true);
  Effect::new(move |_| store_mode(conversation, mode.get()));
  let feed = move || match (mode.get(), filter.with(LogFilter::is_active)) {
    (FeedMode::Chat, _) => view! { <MessageList conversation=conversation is_chat=true /> }.into_any(),
    (FeedMode::Log, false) => view! { <MessageList conversation=conversation /> }.into_any(),
    (FeedMode::Log, true) => {
      view! { <FilteredLog conversation=conversation filter=filter follow=follow /> }.into_any()
    },
  };
  view! {
    <LogToolbar mode=mode filter=filter follow=follow />
    {feed}
  }
  .into_any()
}

#[component]
fn LogToolbar(
  mode: RwSignal<FeedMode>,
  filter: RwSignal<LogFilter>,
  follow: RwSignal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let mode_button = move |target: FeedMode, key: &'static str| {
    view! {
      <button
        class="btn btn-small"
        aria-pressed=move || (mode.get() == target).to_string()
        on:click=move |_| mode.set(target)
      >
        {move || i18n.t(key)}
      </button>
    }
  };
  let level_option = move |level: LogLevel| {
    view! {
      <option value=level.slug() selected=move || filter.with(|filter| filter.min_level == level)>
        {move || i18n.t(&format!("log-level-{}", level.slug()))}
      </option>
    }
  };
  let filters = move || {
    (mode.get() == FeedMode::Log).then(|| {
      view! {
        <select
          class="input log-filter-level"
          aria-label=move || i18n.t("log-filter-level")
          on:change=move |ev| {
            let level = LogLevel::from_slug(&event_target_value(&ev)).unwrap_or(LogLevel::Debug);
            filter.update(|filter| filter.min_level = level);
          }
        >
          {level_option(LogLevel::Debug)}
          {level_option(LogLevel::Info)}
          {level_option(LogLevel::Warn)}
          {level_option(LogLevel::Error)}
        </select>
        <input
          class="input log-filter-text"
          type="search"
          placeholder=move || i18n.t("log-filter-text")
          aria-label=move || i18n.t("log-filter-text")
          prop:value=move || filter.with(|filter| filter.text.clone())
          on:input=move |ev| filter.update(|filter| filter.text = event_target_value(&ev))
        />
        <label class="log-follow">
          <input
            type="checkbox"
            prop:checked=move || follow.get()
            on:change=move |ev| follow.set(event_target_checked(&ev))
          />
          <span>{move || i18n.t("log-follow")}</span>
        </label>
      }
    })
  };
  view! {
    <div class="log-toolbar" role="toolbar" aria-label=move || i18n.t("log-toolbar-label")>
      <div class="log-mode">
        {mode_button(FeedMode::Log, "log-mode-log")}
        {mode_button(FeedMode::Chat, "log-mode-chat")}
      </div>
      {filters}
    </div>
  }
}

/// Every matching entry of the newest messages, scrolled to the end while following.
#[component]
fn FilteredLog(
  conversation: ConversationId,
  filter: RwSignal<LogFilter>,
  follow: RwSignal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let container = NodeRef::<html::Div>::new();
  let is_alive = Arc::new(AtomicBool::new(true));
  {
    let is_alive = Arc::clone(&is_alive);
    on_cleanup(move || is_alive.store(false, Ordering::Relaxed));
  }
  let count = Memo::new(move |_| state.read(|backend| backend.message_count(conversation)));
  let scan = Memo::new(move |_| {
    let filter = filter.get();
    count.track();
    state.read(|backend| scan_entries(backend, conversation, &filter))
  });
  Effect::new(move |_| {
    scan.track();
    if !follow.get() {
      return;
    }
    let is_alive = Arc::clone(&is_alive);
    // Touching a disposed node would panic, so the callback checks that the view still exists.
    request_animation_frame(move || {
      if is_alive.load(Ordering::Relaxed)
        && let Some(element) = container.get_untracked()
      {
        element.set_scroll_top(element.scroll_height());
      }
    });
  });
  let rows = move || {
    scan.with(|scan| {
      scan
        .entries
        .iter()
        .map(|(sent_at, entry)| {
          let class = format!("msg-log log-entry level-{}", entry.level.slug());
          log_entry_line(&format_time_seconds(*sent_at, locale), entry, &class, *sent_at)
        })
        .collect::<Vec<_>>()
    })
  };
  view! {
    <div class="message-list-wrap">
      <div class="message-list log-view" role="log" tabindex="0" aria-label=move || i18n.t("messages-label") node_ref=container>
        <Show when=move || scan.with(|scan| scan.is_truncated) fallback=|| ()>
          <p class="log-notice">
            {move || i18n.t_with("log-filter-truncated", &[("count", &MAX_SHOWN_ENTRIES.to_string())])}
          </p>
        </Show>
        <Show when=move || scan.with(|scan| scan.entries.is_empty()) fallback=|| ()>
          <p class="empty-state">{move || i18n.t("log-filter-none")}</p>
        </Show>
        {rows}
      </div>
    </div>
  }
}

/// Collects the entries matching `filter` from the newest messages backwards, so that a huge channel
/// costs at most [`SCAN_LIMIT`] messages. The result is oldest first.
pub fn scan_entries(backend: &dyn Backend, conversation: ConversationId, filter: &LogFilter) -> Scan {
  let mut found: Vec<(TimestampMs, LogEntry)> = Vec::new();
  let mut end = backend.message_count(conversation);
  let mut scanned = 0;
  let mut is_truncated = end > SCAN_LIMIT;
  'chunks: while end > 0 && scanned < SCAN_LIMIT {
    let start = end.saturating_sub(SCAN_CHUNK);
    for message in backend.messages(conversation, start, end - start).iter().rev() {
      let MessageKind::Log(level) = message.kind else {
        continue;
      };
      for entry in log_entries(&message.body, level).into_iter().rev() {
        if filter.matches(&entry) {
          if found.len() == MAX_SHOWN_ENTRIES {
            is_truncated = true;
            break 'chunks;
          }
          found.push((message.sent_at, entry));
        }
      }
    }
    scanned += end - start;
    end = start;
  }
  found.reverse();
  Scan {
    entries: found,
    is_truncated,
  }
}

fn load_mode(conversation: ConversationId) -> FeedMode {
  storage()
    .and_then(|storage| storage.get_item(&storage_key(conversation)).ok().flatten())
    .map_or(FeedMode::Log, |slug| FeedMode::from_slug(&slug))
}

fn store_mode(conversation: ConversationId, mode: FeedMode) {
  // Storage can be blocked (private windows); the choice then only lasts for this session.
  if let Some(storage) = storage() {
    let _ = storage.set_item(&storage_key(conversation), mode.slug());
  }
}

fn storage_key(conversation: ConversationId) -> String {
  format!("{MODE_STORAGE_PREFIX}{conversation}")
}

fn storage() -> Option<web_sys::Storage> {
  web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::backend::ChatBackend;

  use super::*;
  use crate::mock::MockBackend;

  #[test]
  fn modes_round_trip_and_unknown_means_log() {
    assert_eq!(FeedMode::from_slug(FeedMode::Chat.slug()), FeedMode::Chat);
    assert_eq!(FeedMode::from_slug(FeedMode::Log.slug()), FeedMode::Log);
    assert_eq!(FeedMode::from_slug("whatever"), FeedMode::Log);
  }

  #[test]
  fn scanning_keeps_only_matching_entries_in_order() {
    let backend = MockBackend::new(1_800_000_000_000);
    let logs = backend
      .spaces()
      .into_iter()
      .flat_map(|space| backend.space_conversations(space.id))
      .find(|conv| conv.kind == ConversationKind::Integration)
      .expect("the demo workspace has a log channel");
    let everything = scan_entries(&backend, logs.id, &LogFilter::default());
    let errors = scan_entries(
      &backend,
      logs.id,
      &LogFilter {
        min_level: LogLevel::Error,
        ..LogFilter::default()
      },
    );
    assert!(!everything.entries.is_empty());
    assert!(errors.entries.len() < everything.entries.len());
    assert!(
      errors
        .entries
        .iter()
        .all(|(_, entry)| entry.level == LogLevel::Error)
    );
    assert!(errors.entries.windows(2).all(|pair| pair[0].0 <= pair[1].0));
  }
}
