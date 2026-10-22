use leptos::prelude::*;

use crate::calls::{CallKind, CallPhase, CallRequest};
use crate::components::avatar::PresenceDot;
use crate::components::composer::Composer;
use crate::components::huddle_bar::HuddleBar;
use crate::components::icons::{Icon, IconName};
use crate::components::message_list::MessageList;
use crate::components::security_ui::KeyChangeBanner;
use crate::i18n::use_i18n;
use crate::model::{BookmarkKind, Conversation, ConversationId, ConversationKind};
use crate::state::{SidePanel, use_app_state};

/// Header, bookmarks bar, message feed and composer of one conversation.
#[component]
pub fn ChannelView(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));

  move || {
    // Everything below is recreated per conversation, which resets scroll state and drafts.
    let conv = info.get_untracked()?;
    let name = conv.name.clone();
    let label_name = conv.name;
    let can_post = conv.can_post;
    Some(view! {
      <section class="channel-view" aria-label=move || i18n.t_with("channel-region-label", &[("name", &label_name)])>
        <ChannelHeader conversation=conversation info=info />
        <KeyChangeBanner conversation=conversation />
        <HuddleBar conversation=conversation />
        <BookmarksBar info=info />
        <MessageList conversation=conversation />
        <Composer conversation=conversation can_post=can_post conversation_name=name />
      </section>
    })
  }
}

#[component]
fn ChannelHeader(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();

  let title = move || {
    let conv = info.get()?;
    let icon = match conv.kind {
      ConversationKind::Public => view! { <Icon name=IconName::Hash class="header-icon" /> }.into_any(),
      ConversationKind::Private => view! { <Icon name=IconName::Lock class="header-icon" /> }.into_any(),
      ConversationKind::Integration => {
        view! { <Icon name=IconName::Terminal class="header-icon" /> }.into_any()
      },
      ConversationKind::Group => view! { <Icon name=IconName::Users class="header-icon" /> }.into_any(),
      ConversationKind::Direct => {
        let me = state.me();
        let other = conv.member_ids.iter().copied().find(|id| *id != me.id);
        let presence = other
          .and_then(|id| state.read(|backend| backend.user(id)))
          .map(|user| user.presence);
        match presence {
          Some(presence) => view! { <PresenceDot presence=presence /> }.into_any(),
          None => ().into_any(),
        }
      },
    };
    Some(view! {
      <h1 class="header-title" translate="no">
        {icon}
        {conv.name}
      </h1>
    })
  };

  let topic = move || {
    info
      .get()
      .map(|conv| conv.topic)
      .filter(|topic| !topic.is_empty())
  };
  let is_favorite = move || info.get().is_some_and(|conv| conv.is_favorite);
  let member_count = move || info.get().map_or(0, |conv| conv.member_ids.len());
  let members_open = move || state.panel.get() == SidePanel::Members;

  view! {
    <header class="channel-header">
      <div class="header-main">
        {title}
        <button
          class="icon-btn"
          aria-pressed=move || is_favorite().to_string()
          aria-label=move || if is_favorite() { i18n.t("favorite-remove") } else { i18n.t("favorite-add") }
          title=move || if is_favorite() { i18n.t("favorite-remove") } else { i18n.t("favorite-add") }
          on:click=move |_| state.write(|backend| backend.set_favorite(conversation, !is_favorite()))
        >
          <Icon name=IconName::Star />
        </button>
        <Show when=move || topic().is_some() fallback=|| ()>
          <span class="header-divider" aria-hidden="true"></span>
          <p class="header-topic" title=move || topic().unwrap_or_default()>
            {move || topic().unwrap_or_default()}
          </p>
        </Show>
      </div>
      <div class="header-actions">
        <CallButtons conversation=conversation info=info />
        <button
          class="header-btn"
          aria-pressed=move || members_open().to_string()
          aria-label=move || i18n.t_count("members-button-label", member_count())
          on:click=move |_| {
            state.panel.update(|panel| {
              *panel = if *panel == SidePanel::Members { SidePanel::Closed } else { SidePanel::Members };
            });
          }
        >
          <Icon name=IconName::Users />
          <span>{member_count}</span>
        </button>
      </div>
    </header>
  }
}

/// Pinned links and files under the header. Adding bookmarks is not wired up yet.
#[component]
fn BookmarksBar(info: Memo<Option<Conversation>>) -> impl IntoView {
  let i18n = use_i18n();
  let bookmarks = move || info.get().map(|conv| conv.bookmarks).unwrap_or_default();
  view! {
    <div class="bookmarks-bar" role="toolbar" aria-label=move || i18n.t("bookmarks-label")>
      <For each=bookmarks key=|bookmark| bookmark.title.clone() let:bookmark>
        <button class="bookmark" title=bookmark.title.clone()>
          <span aria-hidden="true">{bookmark.emoji.clone()}</span>
          <Icon name=if bookmark.kind == BookmarkKind::Link { IconName::Link } else { IconName::File } />
          <span class="bookmark-title">{bookmark.title.clone()}</span>
        </button>
      </For>
      <button class="bookmark bookmark-add" disabled title=move || i18n.t("bookmarks-add-soon")>
        <Icon name=IconName::Plus />
        <span>{move || i18n.t("bookmarks-add")}</span>
      </button>
    </div>
  }
}

/// Start buttons in the header: audio and video call, plus a drop-in huddle in channels.
#[component]
fn CallButtons(conversation: ConversationId, info: Memo<Option<Conversation>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let kind = move || info.get().map(|conv| conv.kind);
  let call_kind = move || match kind() {
    Some(ConversationKind::Direct) => CallKind::Direct,
    _ => CallKind::Group,
  };
  let is_callable = move || kind().is_some_and(|kind| kind != ConversationKind::Integration);
  let is_channel = move || matches!(kind(), Some(ConversationKind::Public | ConversationKind::Private));
  let is_busy = move || call.phase.get() != CallPhase::Idle;

  view! {
    <Show when=is_callable fallback=|| ()>
      <button
        class="icon-btn"
        disabled=is_busy
        aria-label=move || i18n.t("call-start-audio")
        title=move || i18n.t("call-start-audio")
        on:click=move |_| call.prepare(CallRequest { conversation, kind: call_kind() }, false)
      >
        <Icon name=IconName::Phone />
      </button>
      <button
        class="icon-btn"
        disabled=is_busy
        aria-label=move || i18n.t("call-start-video")
        title=move || i18n.t("call-start-video")
        on:click=move |_| call.prepare(CallRequest { conversation, kind: call_kind() }, true)
      >
        <Icon name=IconName::Video />
      </button>
      <Show when=is_channel fallback=|| ()>
        <button
          class="icon-btn"
          disabled=is_busy
          aria-label=move || i18n.t("huddle-start")
          title=move || i18n.t("huddle-start")
          on:click=move |_| {
            call.is_camera_on.set(false);
            call.join(CallRequest { conversation, kind: CallKind::Huddle });
          }
        >
          <Icon name=IconName::Headphones />
        </button>
      </Show>
    </Show>
  }
}
