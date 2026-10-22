use leptos::prelude::*;

use crate::components::composer::Composer;
use crate::components::icons::{Icon, IconName};
use crate::components::message_row::MessageRow;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, Message, ThreadId};
use crate::state::{SidePanel, use_app_state};

/// Right-hand thread view: root message, replies and a reply composer.
#[component]
pub fn ThreadPanel(
  conversation: ConversationId,
  thread: ThreadId,
  /// Embedded in the threads inbox, where closing returns to the empty state.
  #[prop(default = false)]
  embedded: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let thread_messages = Memo::new(move |_| state.read(|backend| backend.thread(conversation, thread)));
  let conv_name = Memo::new(move |_| {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|conv| conv.name)
      .unwrap_or_default()
  });
  let can_post = state
    .read(|backend| backend.conversation(conversation))
    .is_none_or(|conv| conv.can_post);
  let is_following = RwSignal::new(state.read(|backend| backend.is_thread_followed(conversation, thread)));
  // Opening the thread (and every new reply while it is open) counts as reading it.
  Effect::new(move |_| {
    thread_messages.track();
    state.write(|backend| backend.mark_thread_read(conversation, thread));
  });

  view! {
    <aside class="side-panel thread-panel" class:embedded=embedded aria-label=move || i18n.t("thread-panel-label")>
      <header class="panel-header">
        <div class="panel-title">
          <h2>{move || i18n.t("thread-title")}</h2>
          <span class="panel-subtitle" translate="no">
            {move || format!("# {}", conv_name.get())}
          </span>
        </div>
        <button
          class="header-btn"
          aria-pressed=move || is_following.get().to_string()
          on:click=move |_| {
            is_following.update(|value| *value = !*value);
            let is_followed = is_following.get_untracked();
            state.write(|backend| backend.set_thread_followed(conversation, thread, is_followed));
          }
        >
          <Icon name=IconName::Bell />
          <span>{move || if is_following.get() { i18n.t("thread-following") } else { i18n.t("thread-follow") }}</span>
        </button>
        <button
          class="icon-btn"
          aria-label=move || i18n.t("close")
          title=move || i18n.t("close")
          on:click=move |_| state.panel.set(SidePanel::Closed)
        >
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="panel-scroll" tabindex="0" role="log" aria-label=move || i18n.t("thread-messages-label")>
        {move || {
          let mut prev: Option<Message> = None;
          let mut out = Vec::new();
          for (index, message) in thread_messages.get().into_iter().enumerate() {
            if index == 1 {
              out.push(
                view! {
                  <div class="thread-divider" role="separator">
                    <span>{move || i18n.t("thread-replies-divider")}</span>
                  </div>
                }
                .into_any(),
              );
            }
            // The root always shows its header, and so does the first reply below the divider.
            let row_prev = if index <= 1 { None } else { prev.clone() };
            out.push(view! { <MessageRow message=message.clone() maybe_prev=row_prev in_thread=true /> }.into_any());
            prev = Some(message);
          }
          out
        }}
      </div>
      <Composer
        conversation=conversation
        maybe_thread=thread
        placeholder_key="composer-placeholder-reply"
        can_post=can_post
      />
    </aside>
  }
}
