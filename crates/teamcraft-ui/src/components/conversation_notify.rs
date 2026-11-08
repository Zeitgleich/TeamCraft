//! The bell of a conversation's header: how this conversation notifies, and for how long it is
//! muted. Both are the per-conversation settings of Settings > Notifications, so the two places
//! always agree (and `notify_sync` carries them to the other devices). The sidebar's entry menu
//! (#903) offers the same choices for a conversation and for a whole space. A conversation also
//! has its filters (notifications.md, "Per-conversation filters"): whether `@channel` notifies,
//! and quiet hours, edited one level deeper in [`QuietHoursDialog`].

use leptos::prelude::*;

use teamcraft_proto::{ChannelMentions, DndWindow};

use crate::clock::local_time_zone;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::settings_account::WeekdayChips;
use crate::components::sheet::MenuSurface;
use crate::form_errors::error_id;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, SpaceId};
use crate::notify_sync::{MAX_DND_WINDOWS, format_minutes, is_valid_window, parse_minutes};
use crate::prefs::{ConversationFilters, ConversationNotify, MuteFor, NotifyLevel, default_dnd_window};
use crate::state::{AppState, Overlay, use_app_state};

/// The `@channel` choices of a conversation, in menu order.
const CHANNEL_MENTION_CHOICES: [ChannelMentions; 3] = [
  ChannelMentions::Default,
  ChannelMentions::Notify,
  ChannelMentions::Ignore,
];

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
          <NotifyChoices
            target=NotifyTarget::Conversation(conversation)
            on_close=Callback::new(move |()| is_open.set(false))
          />
        </MenuSurface>
      </Show>
    </div>
  }
}

/// What a notification choice applies to: a conversation, or a space whose channels follow it
/// unless they have a setting of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyTarget {
  Conversation(ConversationId),
  Space(SpaceId),
}

/// The notification levels, then the mute durations, each a radio group of menu items; for a
/// conversation also its filters. `on_close` closes the menu when an item opens a dialog.
#[component]
pub fn NotifyChoices(target: NotifyTarget, on_close: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let filters = match target {
    NotifyTarget::Conversation(conversation) => {
      Some(view! { <FilterChoices conversation=conversation on_close=on_close /> })
    },
    NotifyTarget::Space(_) => None,
  };
  view! {
    <p class="menu-heading">{move || i18n.t("notif-level-col")}</p>
    <LevelItem target=target maybe_level=None />
    {NotifyLevel::ALL
      .into_iter()
      .map(|level| view! { <LevelItem target=target maybe_level=Some(level) /> })
      .collect_view()}
    <p class="menu-heading">{move || i18n.t("notif-mute-col")}</p>
    {MuteFor::ALL
      .into_iter()
      .map(|mute| view! { <MuteItem target=target mute=mute /> })
      .collect_view()}
    {filters}
  }
}

/// Whether `@channel` notifies here, and the way to the quiet hours.
#[component]
fn FilterChoices(conversation: ConversationId, on_close: Callback<()>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let has_quiet_hours = move || !filters_of(state, conversation).quiet_windows.is_empty();
  view! {
    <p class="menu-heading">{move || i18n.t("notif-channel-mentions-col")}</p>
    {CHANNEL_MENTION_CHOICES
      .into_iter()
      .map(|choice| view! { <ChannelMentionsItem conversation=conversation choice=choice /> })
      .collect_view()}
    <p class="menu-heading">{move || i18n.t("notif-quiet-col")}</p>
    <button
      type="button"
      class="menu-item"
      role="menuitem"
      aria-haspopup="dialog"
      on:click=move |_| {
        on_close.run(());
        state.overlay.set(Overlay::QuietHours(conversation));
      }
    >
      <Icon name=IconName::Clock />
      <span class="menu-label">
        {move || i18n.t("notif-quiet-edit")}
        <small>{move || i18n.t(if has_quiet_hours() { "notif-quiet-on" } else { "notif-quiet-off" })}</small>
      </span>
    </button>
  }
}

/// One `@channel` choice of a conversation.
#[component]
fn ChannelMentionsItem(conversation: ConversationId, choice: ChannelMentions) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let label_key = format!("notif-channel-mentions-{}", choice.as_str());
  let checked_choice = choice.clone();
  let is_checked = Signal::derive(move || filters_of(state, conversation).channel_mentions == checked_choice);
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitemradio"
      aria-checked=move || is_checked.get().to_string()
      on:click=move |_| {
        let choice = choice.clone();
        update_filters(state, conversation, move |filters| filters.channel_mentions = choice);
      }
    >
      <span class="menu-label">{move || i18n.t(&label_key)}</span>
      {move || is_checked.get().then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}

/// Quiet hours of one conversation: weekly ranges in this device's time during which it stays
/// silent as if muted. Changes apply as they are made, like the do-not-disturb schedule; a range
/// that isn't complete yet says so at the range and is left out until it is.
#[component]
pub fn QuietHoursDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let windows = Memo::new(move |_| filters_of(state, conversation).quiet_windows);
  let can_add = move || windows.with(|list| list.len() < MAX_DND_WINDOWS);
  let zone = local_time_zone();
  let zone_hint = move || match &zone {
    Some(zone) => i18n.t_with("notif-dnd-zone", &[("zone", zone)]),
    None => i18n.t("notif-dnd-zone-unknown"),
  };
  let name = state
    .read(|backend| backend.conversation(conversation))
    .map(|found| found.name)
    .unwrap_or_default();
  view! {
    <Modal labelled_by="quiet-hours-title" class="modal-small quiet-hours">
      <header class="modal-head">
        <h2 id="quiet-hours-title">{move || i18n.t("notif-quiet-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p class="muted-text">{move || i18n.t_with("notif-quiet-hint", &[("name", &name)])}</p>
        <ul class="set-ranges" aria-label=move || i18n.t("notif-quiet-title")>
          {move || {
            windows
              .get()
              .into_iter()
              .enumerate()
              .map(|(index, window)| view! { <QuietRange conversation=conversation index=index window=window /> })
              .collect_view()
          }}
        </ul>
        <Show when=move || windows.with(Vec::is_empty) fallback=|| ()>
          <p class="muted-text">{move || i18n.t("notif-quiet-none")}</p>
        </Show>
        <p class="muted-text">{zone_hint}</p>
        <button
          type="button"
          class="btn btn-small"
          prop:disabled=move || !can_add()
          on:click=move |_| update_filters(state, conversation, |filters| filters.quiet_windows.push(default_dnd_window()))
        >
          <Icon name=IconName::Plus />
          <span>{move || i18n.t("notif-dnd-add-range")}</span>
        </button>
      </div>
      <footer class="modal-foot">
        <button class="btn btn-primary" data-autofocus on:click=move |_| state.close_overlay()>
          {move || i18n.t("close")}
        </button>
      </footer>
    </Modal>
  }
}

/// One range of a conversation's quiet hours.
#[component]
fn QuietRange(conversation: ConversationId, index: usize, window: DndWindow) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let edit = move |change: Box<dyn FnOnce(&mut DndWindow)>| {
    update_filters(state, conversation, move |filters| {
      if let Some(window) = filters.quiet_windows.get_mut(index) {
        change(window);
      }
    });
  };
  let days = RwSignal::new(window.weekdays.clone());
  // The chips edit a copy; changes flow into the filters.
  Effect::new(move |previous: Option<()>| {
    let chosen = days.get();
    if previous.is_some() {
      edit(Box::new(move |window| window.weekdays = chosen));
    }
  });
  let field_id = format!("quiet-{index}");
  let from_id = format!("quiet-from-{index}");
  let to_id = format!("quiet-to-{index}");
  let is_invalid = !is_valid_window(&window);
  let number = (index + 1).to_string();
  view! {
    <li
      class="set-range"
      role="group"
      tabindex="-1"
      id=field_id.clone()
      aria-label=move || i18n.t_with("notif-dnd-range", &[("number", &number)])
      aria-invalid=is_invalid.then_some("true")
      aria-describedby=is_invalid.then(|| error_id(&field_id))
    >
      <WeekdayChips days=days />
      <div class="set-range-times">
        <label for=from_id.clone()>{move || i18n.t("notif-dnd-from")}</label>
        <input
          id=from_id
          class="input"
          type="time"
          prop:value=format_minutes(window.start_minute)
          on:change=move |ev| {
            if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
              edit(Box::new(move |window| window.start_minute = minute));
            }
          }
        />
        <label for=to_id.clone()>{move || i18n.t("notif-dnd-to")}</label>
        <input
          id=to_id
          class="input"
          type="time"
          prop:value=format_minutes(window.end_minute)
          on:change=move |ev| {
            if let Some(minute) = parse_minutes(&event_target_value(&ev)) {
              edit(Box::new(move |window| window.end_minute = minute));
            }
          }
        />
        <button
          type="button"
          class="icon-btn"
          aria-label=move || i18n.t("notif-dnd-remove-range")
          title=move || i18n.t("notif-dnd-remove-range")
          on:click=move |_| {
            update_filters(state, conversation, |filters| {
              if index < filters.quiet_windows.len() {
                filters.quiet_windows.remove(index);
              }
            })
          }
        >
          <Icon name=IconName::Trash />
        </button>
      </div>
      {is_invalid.then(|| view! {
        <p class="field-error" id=error_id(&field_id) role="alert">{move || i18n.t("notif-dnd-range-invalid")}</p>
      })}
    </li>
  }
}

/// One notification level; `None` follows the global level.
#[component]
fn LevelItem(target: NotifyTarget, maybe_level: Option<NotifyLevel>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let label_key = match maybe_level {
    None => "notif-level-default".to_owned(),
    Some(level) => format!("notify-level-{}", level.slug()),
  };
  let is_checked = move || entry_of(state, target).maybe_level == maybe_level;
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitemradio"
      aria-checked=move || is_checked().to_string()
      on:click=move |_| update_entry(state, target, |entry| entry.maybe_level = maybe_level)
    >
      <span class="menu-label">{move || i18n.t(&label_key)}</span>
      {move || is_checked().then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}

/// One mute duration.
#[component]
fn MuteItem(target: NotifyTarget, mute: MuteFor) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_checked = move || entry_of(state, target).mute == mute;
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitemradio"
      aria-checked=move || is_checked().to_string()
      on:click=move |_| update_entry(state, target, |entry| entry.mute = mute)
    >
      <span class="menu-label">{move || i18n.t(&format!("notif-mute-{}", mute.slug()))}</span>
      {move || is_checked().then(|| view! { <Icon name=IconName::Check /> })}
    </button>
  }
}

/// The target's own setting (a channel's inherited one is not its own).
pub fn entry_of(state: AppState, target: NotifyTarget) -> ConversationNotify {
  state.settings.prefs.with(|prefs| match target {
    NotifyTarget::Conversation(conversation) => prefs.notifications.conversation(&conversation.to_string()),
    NotifyTarget::Space(space) => prefs.notifications.space(&space.to_string()),
  })
}

/// Changes the target's setting; `notify_sync` sends it to the server.
pub fn update_entry(state: AppState, target: NotifyTarget, change: impl FnOnce(&mut ConversationNotify)) {
  state.settings.prefs.update(|prefs| match target {
    NotifyTarget::Conversation(conversation) => {
      let id = conversation.to_string();
      let mut entry = prefs.notifications.conversation(&id);
      change(&mut entry);
      prefs.notifications.set_conversation(&id, entry);
    },
    NotifyTarget::Space(space) => {
      let id = space.to_string();
      let mut entry = prefs.notifications.space(&id);
      change(&mut entry);
      prefs.notifications.set_space(&id, entry);
    },
  });
}

pub fn filters_of(state: AppState, conversation: ConversationId) -> ConversationFilters {
  state
    .settings
    .prefs
    .with(|prefs| prefs.notifications.filters(&conversation.to_string()))
}

/// Changes a conversation's filters; `notify_sync` sends them to the server.
pub fn update_filters(
  state: AppState,
  conversation: ConversationId,
  change: impl FnOnce(&mut ConversationFilters),
) {
  state.settings.prefs.update(|prefs| {
    let id = conversation.to_string();
    let mut filters = prefs.notifications.filters(&id);
    change(&mut filters);
    prefs.notifications.set_filters(&id, filters);
  });
}
