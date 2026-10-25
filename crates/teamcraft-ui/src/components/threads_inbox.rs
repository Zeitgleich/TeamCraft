use leptos::prelude::*;

use crate::clock::format_time;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::rich_text::RichText;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::thread_panel::ThreadPanel;
use crate::i18n::use_i18n;
use crate::model::{ThreadSummary, thread_id_of};
use crate::state::{SidePanel, use_app_state};

/// All followed threads with unread replies, plus the selected thread beside the list.
#[component]
pub fn ThreadsInbox() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let only_unread = RwSignal::new(false);
  let threads = Memo::new(move |_| {
    let mut all = state.read(|backend| backend.followed_threads());
    if only_unread.get() {
      all.retain(|thread| thread.unread_replies > 0);
    }
    all
  });

  view! {
    <section class="threads-view" aria-label=move || i18n.t("threads-title")>
      <div class="threads-list-col">
        <header class="panel-header">
          <CompactBack />
          <div class="panel-title">
            <h1>{move || i18n.t("threads-title")}</h1>
          </div>
          <div class="segmented" role="group" aria-label=move || i18n.t("threads-filter")>
            <SlidingIndicator />
            <button aria-pressed=move || (!only_unread.get()).to_string() on:click=move |_| only_unread.set(false)>
              {move || i18n.t("threads-all")}
            </button>
            <button aria-pressed=move || only_unread.get().to_string() on:click=move |_| only_unread.set(true)>
              {move || i18n.t("threads-unread")}
            </button>
          </div>
        </header>
        <ul class="threads-list panel-scroll">
          <For each=move || threads.get() key=|thread| (thread.root.id, thread.reply_count, thread.unread_replies) let:thread>
            <ThreadCard thread=thread />
          </For>
          <Show when=move || threads.get().is_empty() fallback=|| ()>
            <EmptyState icon=IconName::Threads title_key="threads-empty-title" body_key="threads-empty" as_item=true />
          </Show>
        </ul>
      </div>
      <div class="threads-detail-col">
        {move || match state.panel.get() {
          SidePanel::Thread { conversation, thread } => {
            view! { <ThreadPanel conversation=conversation thread=thread embedded=true /> }.into_any()
          },
          _ => {
            view! {
              <div class="empty-detail">
                <Icon name=IconName::Threads class="threads-empty-icon" />
                <p>{move || i18n.t("threads-select")}</p>
              </div>
            }
            .into_any()
          },
        }}
      </div>
    </section>
  }
}

#[component]
fn ThreadCard(thread: ThreadSummary) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let author = state.read(|backend| backend.user(thread.root.author));
  let conv_name = state
    .read(|backend| backend.conversation(thread.conversation))
    .map(|c| crate::components::thread_panel::conversation_label(&c))
    .unwrap_or_default();
  let (conversation, thread_id) = (thread.conversation, thread_id_of(thread.root.id));
  let is_selected = move || {
    state.panel.get()
      == SidePanel::Thread {
        conversation,
        thread: thread_id,
      }
  };
  let unread = thread.unread_replies;
  let time = format_time(thread.last_reply_at, i18n.locale().tag());
  let participants: Vec<_> = thread
    .participant_ids
    .iter()
    .take(4)
    .filter_map(|id| state.read(|backend| backend.user(*id)))
    .collect();

  view! {
    <li>
      <button
        class="thread-card"
        class:selected=is_selected
        class:unread={unread > 0}
        aria-current=move || is_selected().then_some("true")
        on:click=move |_| state.open_thread(conversation, thread_id)
      >
        <span class="thread-card-top">
          {author.map(|user| view! { <span class="thread-author" translate="no">{user.display_name}</span> })}
          <span class="thread-channel" translate="no">{conv_name}</span>
          <time class="thread-time">{time}</time>
        </span>
        <span class="thread-preview"><RichText body=thread.root.body.clone() /></span>
        <span class="thread-card-bottom">
          <span class="avatar-stack">
            {participants.into_iter().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> }).collect_view()}
          </span>
          <span class="thread-count">{move || i18n.t_count("thread-replies", thread.reply_count as usize)}</span>
          {(unread > 0).then(|| {
            view! {
              <span class="unread-pill" class:mention=thread.has_mention>
                {move || i18n.t_count("thread-new", unread as usize)}
              </span>
            }
          })}
        </span>
      </button>
    </li>
  }
}
