//! "Drafts & sent" (#1220): every draft of the user with a way back to its composer, and their
//! recent messages with a jump to each. Both are read on this device; which space the view shows
//! stays between the user's devices, like the other scoped views (#954).

use leptos::prelude::*;
use teamcraft_client_api::drafts::{DraftItem, SentItem};

use crate::clock::format_date_time;
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::page::ScopedSpaceName;
use crate::components::rich_text::RichText;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::tab_list::{on_tab_keys, roving_tabindex};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const DRAFTS_PANEL_ID: &str = "drafts-panel";
/// Sent messages shown at first and added by "Show more": their text is rendered as rich text, and
/// most visits only look at the newest.
const SENT_PAGE: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DraftsTab {
  Drafts,
  Sent,
}

impl DraftsTab {
  const ALL: [Self; 2] = [Self::Drafts, Self::Sent];

  fn label_key(self) -> &'static str {
    match self {
      Self::Drafts => "drafts-tab-drafts",
      Self::Sent => "drafts-tab-sent",
    }
  }

  fn tab_id(self) -> String {
    format!("{}-tab", self.label_key())
  }
}

#[component]
pub fn DraftsView() -> impl IntoView {
  let i18n = use_i18n();
  let tab = RwSignal::new(DraftsTab::Drafts);
  let on_keydown =
    move |ev: web_sys::KeyboardEvent| on_tab_keys(&ev, &DraftsTab::ALL, tab, DraftsTab::tab_id);
  view! {
    <section class="saved-view page-view" aria-label=move || i18n.t("drafts-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("drafts-title")}</h1>
          <ScopedSpaceName />
        </div>
        <div class="segmented" role="tablist" aria-label=move || i18n.t("drafts-title") on:keydown=on_keydown>
          <SlidingIndicator />
          {DraftsTab::ALL
            .into_iter()
            .map(|value| {
              view! {
                <button
                  id=value.tab_id()
                  role="tab"
                  aria-selected=move || (tab.get() == value).to_string()
                  aria-controls=DRAFTS_PANEL_ID
                  tabindex=move || roving_tabindex(tab.get() == value)
                  on:click=move |_| tab.set(value)
                >
                  {move || i18n.t(value.label_key())}
                </button>
              }
            })
            .collect_view()}
        </div>
      </header>
      <div class="saved-list panel-scroll" role="tabpanel" id=DRAFTS_PANEL_ID aria-labelledby=move || tab.get().tab_id()>
        {move || match tab.get() {
          DraftsTab::Drafts => view! { <DraftList /> }.into_any(),
          // Only read once it is looked at.
          DraftsTab::Sent => view! { <SentList /> }.into_any(),
        }}
      </div>
    </section>
  }
}

#[component]
fn DraftList() -> impl IntoView {
  let state = use_app_state();
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let drafts = Memo::new(move |_| {
    let maybe_space = scope.get();
    state.read(|backend| backend.drafts(maybe_space))
  });
  view! {
    <ul class="page-cards">
      <For
        each=move || drafts.get()
        key=|draft| (draft.conversation, draft.maybe_thread, draft.edited_at)
        let:draft
      >
        <DraftCard draft=draft />
      </For>
    </ul>
    <Show when=move || drafts.with(Vec::is_empty) fallback=|| ()>
      {move || {
        let body_key = if scope.get().is_some() { "drafts-space-empty" } else { "drafts-empty" };
        view! { <EmptyState icon=IconName::Compose title_key="drafts-empty-title" body_key=body_key /> }
      }}
    </Show>
  }
}

#[component]
fn DraftCard(draft: DraftItem) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let (conversation, maybe_thread) = (draft.conversation, draft.maybe_thread);
  // Deleting reaches all the user's devices and can't be taken back, so it asks first, in place.
  let is_confirming = RwSignal::new(false);
  let file_count = draft.file_count as usize;
  let where_it_is = draft.maybe_thread.map(|_| match draft.maybe_thread_root.clone() {
    Some(root) => i18n.t_with("drafts-in-thread", &[("root", &root)]),
    None => i18n.t("drafts-in-thread-unknown"),
  });
  view! {
    <li class="page-card saved-card">
      <div class="saved-meta">
        <span class="saved-author" translate="no">{draft.conversation_name.clone()}</span>
        {where_it_is.map(|text| view! { <span class="saved-where" translate="no">{text}</span> })}
        <time class="saved-time">{format_date_time(draft.edited_at, locale)}</time>
      </div>
      {(!draft.body.trim().is_empty()).then(|| {
        view! { <div class="saved-body draft-body"><RichText body=draft.body.clone() me=state.me().username /></div> }
      })}
      {(file_count > 0).then(|| {
        view! {
          <div class="sent-files">
            <Icon name=IconName::Paperclip />
            <span>{move || i18n.t_count("sent-files", file_count)}</span>
          </div>
        }
      })}
      <div class="saved-actions">
        {move || {
          if is_confirming.get() {
            view! {
              <span class="draft-delete-question" role="status">{i18n.t("drafts-delete-question")}</span>
              <button
                class="btn btn-small btn-danger"
                on:click=move |_| state.write(|backend| backend.discard_draft(conversation, maybe_thread))
              >
                {i18n.t("drafts-delete")}
              </button>
              <button class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(false)>
                {i18n.t("cancel")}
              </button>
            }
            .into_any()
          } else {
            view! {
              <button class="btn btn-small" on:click=move |_| state.open_draft(conversation, maybe_thread)>
                {i18n.t("drafts-continue")}
              </button>
              <button class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                {i18n.t("drafts-delete")}
              </button>
            }
            .into_any()
          }
        }}
      </div>
    </li>
  }
}

#[component]
fn SentList() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let sent = Memo::new(move |_| {
    let maybe_space = scope.get();
    state.read(|backend| backend.sent_messages(maybe_space))
  });
  let shown = RwSignal::new(SENT_PAGE);
  let total = move || sent.with(Vec::len);
  view! {
    <ul class="page-cards">
      <For
        each=move || sent.with(|items| items.iter().take(shown.get()).cloned().collect::<Vec<_>>())
        key=|item| item.message
        let:item
      >
        <SentCard item=item />
      </For>
    </ul>
    <Show when=move || { total() > shown.get() } fallback=|| ()>
      <button class="btn btn-small drafts-more" on:click=move |_| shown.update(|count| *count += SENT_PAGE)>
        {move || i18n.t("drafts-show-more")}
      </button>
    </Show>
    {move || {
      if total() == 0 {
        let body_key = if scope.get().is_some() { "sent-space-empty" } else { "sent-empty" };
        view! { <EmptyState icon=IconName::Compose title_key="sent-empty-title" body_key=body_key /> }.into_any()
      } else {
        view! { <p class="drafts-note">{move || i18n.t("sent-note")}</p> }.into_any()
      }
    }}
  }
}

#[component]
fn SentCard(item: SentItem) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let (conversation, message, maybe_thread) = (item.conversation, item.message, item.maybe_thread);
  let file_count = item.file_count as usize;
  view! {
    <li class="page-card saved-card">
      <div class="saved-meta">
        <span class="saved-author" translate="no">{item.conversation_name.clone()}</span>
        {maybe_thread.map(|_| view! { <span class="saved-where">{move || i18n.t("sent-in-thread")}</span> })}
        <time class="saved-time">{format_date_time(item.sent_at, locale)}</time>
      </div>
      {(!item.excerpt.is_empty()).then(|| {
        view! { <div class="saved-body draft-body"><RichText body=item.excerpt.clone() me=state.me().username /></div> }
      })}
      {(file_count > 0).then(|| {
        view! {
          <div class="sent-files">
            <Icon name=IconName::Paperclip />
            <span>{move || i18n.t_count("sent-files", file_count)}</span>
          </div>
        }
      })}
      <div class="saved-actions">
        <button class="btn btn-small" on:click=move |_| state.open_message(conversation, message, maybe_thread)>
          {move || i18n.t("sent-open")}
        </button>
      </div>
    </li>
  }
}
