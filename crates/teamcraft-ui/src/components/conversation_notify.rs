//! The bell of a conversation's header: how this conversation notifies, and for how long it is
//! muted. Both are the per-conversation settings of Settings > Notifications, so the two places
//! always agree (and `notify_sync` carries them to the other devices).

use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::prefs::{ConversationNotify, MuteFor, NotifyLevel};
use crate::state::{AppState, use_app_state};

/// Bell button with the notification menu of one conversation.
#[component]
pub fn ConversationBell(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let is_muted = move || {
    state
      .settings
      .prefs
      .with(|prefs| prefs.notifications.is_muted(&conversation.to_string()))
  };

  view! {
    <div class="conversation-bell">
      <button
        type="button"
        class="header-btn"
        class:is-muted=is_muted
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || i18n.t("conversation-bell")
        title=move || i18n.t("conversation-bell")
        on:click=move |_| is_open.update(|open| *open = !*open)
      >
        {move || view! { <Icon name=if is_muted() { IconName::BellOff } else { IconName::Bell } /> }}
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <MenuSurface
          class="conversation-menu bell-menu"
          label=Signal::derive(move || i18n.t("conversation-bell"))
          on_close=Callback::new(move |()| is_open.set(false))
        >
          <p class="menu-heading">{move || i18n.t("notif-level-col")}</p>
          <LevelItem conversation=conversation maybe_level=None />
          {NotifyLevel::ALL
            .into_iter()
            .map(|level| view! { <LevelItem conversation=conversation maybe_level=Some(level) /> })
            .collect_view()}
          <p class="menu-heading">{move || i18n.t("notif-mute-col")}</p>
          {MuteFor::ALL
            .into_iter()
            .map(|mute| view! { <MuteItem conversation=conversation mute=mute /> })
            .collect_view()}
        </MenuSurface>
      </Show>
    </div>
  }
}

/// One notification level; `None` follows the global level.
#[component]
fn LevelItem(conversation: ConversationId, maybe_level: Option<NotifyLevel>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let label_key = match maybe_level {
    None => "notif-level-default".to_owned(),
    Some(level) => format!("notify-level-{}", level.slug()),
  };
  let is_checked = move || entry_of(state, conversation).maybe_level == maybe_level;
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitemradio"
      aria-checked=move || is_checked().to_string()
      on:click=move |_| update_entry(state, conversation, |entry| entry.maybe_level = maybe_level)
    >
      <span class="menu-label">{move || i18n.t(&label_key)}</span>
      {move || is_checked().then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}

/// One mute duration.
#[component]
fn MuteItem(conversation: ConversationId, mute: MuteFor) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_checked = move || entry_of(state, conversation).mute == mute;
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitemradio"
      aria-checked=move || is_checked().to_string()
      on:click=move |_| update_entry(state, conversation, |entry| entry.mute = mute)
    >
      <span class="menu-label">{move || i18n.t(&format!("notif-mute-{}", mute.slug()))}</span>
      {move || is_checked().then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}

fn entry_of(state: AppState, conversation: ConversationId) -> ConversationNotify {
  state
    .settings
    .prefs
    .with(|prefs| prefs.notifications.conversation(&conversation.to_string()))
}

fn update_entry(state: AppState, conversation: ConversationId, change: impl FnOnce(&mut ConversationNotify)) {
  let id = conversation.to_string();
  state.settings.prefs.update(|prefs| {
    let mut entry = prefs.notifications.conversation(&id);
    change(&mut entry);
    prefs.notifications.set_conversation(&id, entry);
  });
}
