use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_proto::Role;

use crate::admin::AdminSection;
use crate::clock::{
  format_date_time, format_local_datetime_input, format_time, local_day, now_ms, parse_local_datetime,
  tomorrow_at,
};
use crate::components::avatar::{Avatar, AvatarSize, PresenceDot};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::profile_views::status_until_label;
use crate::components::sheet::MenuSurface;
use crate::i18n::use_i18n;
use crate::model::{CustomStatus, PresenceStatus, StatusExpiry, TimestampMs};
use crate::session::use_session;
use crate::state::{Overlay, SettingsTab, use_app_state};

/// "Until tomorrow morning" means 08:00 local time.
pub(crate) const MORNING_MINUTE: u16 = 8 * 60;
pub(crate) const MS_PER_HOUR: TimestampMs = 3_600_000;

/// Suggestions while the workspace configured no presets of its own; the label is an i18n key.
const STATUS_PRESETS: [(&str, &str); 4] = [
  ("🗓️", "status-preset-meeting"),
  ("🚌", "status-preset-commuting"),
  ("🤒", "status-preset-sick"),
  ("🌴", "status-preset-vacation"),
];

/// Popover under the avatar: presence, custom status and account shortcuts.
#[component]
pub fn StatusMenu() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let menu = NodeRef::<html::Div>::new();
  let session = use_session();
  let me = Memo::new(move |_| state.me());
  let is_admin = Memo::new(move |_| state.read(|backend| backend.my_role()).includes(&Role::Admin));
  // The workspace may switch statuses off (PLAN.md §4): then there is nothing to set.
  let are_statuses_enabled =
    Memo::new(move |_| state.read(|backend| backend.presence_policy()).statuses_enabled);

  Effect::new(move |_| {
    if let Some(element) = menu.get() {
      let selected = element.query_selector("[aria-checked='true']").ok().flatten();
      let target = selected.or_else(|| element.query_selector("[role^='menuitem']").ok().flatten());
      if let Some(target) = target.and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok()) {
        drop(target.focus());
      }
    }
  });

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let key = ev.key();
    match key.as_str() {
      "Escape" => {
        ev.stop_propagation();
        state.close_overlay();
        focus_account_button();
      },
      "ArrowDown" | "ArrowUp" | "Home" | "End" => {
        ev.prevent_default();
        move_focus(&ev, menu, &key);
      },
      "Tab" => state.close_overlay(),
      _ => {},
    }
  };

  let set_presence_until = move |presence: PresenceStatus, maybe_until: Option<TimestampMs>| {
    state.write(|backend| backend.set_presence_until(presence, maybe_until));
    let maybe_dnd_until = maybe_until.filter(|_| presence == PresenceStatus::DoNotDisturb);
    state
      .settings
      .prefs
      .update(|prefs| prefs.notifications.maybe_dnd_until = maybe_dnd_until);
    state.close_overlay();
    focus_account_button();
  };
  let set_presence = move |presence: PresenceStatus| set_presence_until(presence, None);
  let is_dnd = Memo::new(move |_| me.get().presence == PresenceStatus::DoNotDisturb);
  let dnd_until_label = move || {
    let until = state.settings.prefs.get().notifications.maybe_dnd_until?;
    let now = now_ms();
    if !is_dnd.get() || until <= now {
      return None;
    }
    let locale = i18n.locale().tag();
    let time = if local_day(until) == local_day(now) {
      format_time(until, locale)
    } else {
      format_date_time(until, locale)
    };
    Some(i18n.t_with("presence-dnd-until", &[("time", &time)]))
  };
  let is_custom_open = RwSignal::new(false);
  let custom_until = RwSignal::new(format_local_datetime_input(now_ms() + MS_PER_HOUR));
  let set_custom = move || {
    if let Some(until) = parse_local_datetime(&custom_until.get_untracked()).filter(|until| *until > now_ms())
    {
      set_presence_until(PresenceStatus::DoNotDisturb, Some(until));
    }
  };

  view! {
    <MenuSurface
      class="status-menu"
      sheet_class="status-menu"
      label=Signal::derive(move || i18n.t("status-menu-label"))
      on_close=Callback::new(move |()| state.close_overlay())
      node_ref=menu
      on_keydown=Callback::new(on_keydown)
    >
      <div class="menu-user">
        {move || view! { <Avatar user=me.get() size=AvatarSize::Large /> }}
        <div>
          <div class="menu-user-name" translate="no">
            {move || me.get().display_name}
          </div>
          <div class="menu-user-sub" translate="no">
            {move || format!("@{}", me.get().username)}
          </div>
        </div>
      </div>
      <Show when=move || are_statuses_enabled.get() fallback=|| ()>
        <button
          class="menu-item custom-status-row"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| state.overlay.set(Overlay::CustomStatus)
        >
          // An empty status (no emoji, no text) is no status: offer to set one.
          {move || match me.get().maybe_custom_status.filter(|status| !status.text.trim().is_empty() || !status.emoji.trim().is_empty()) {
            Some(status) => {
              let maybe_until = status.maybe_expires_at.map(|at| status_until_label(at, now_ms(), i18n));
              view! {
                <span class="menu-emoji" aria-hidden="true">{status.emoji}</span>
                <span class="menu-label">
                  {status.text}
                  {maybe_until.map(|until| view! { <small>{until}</small> })}
                </span>
              }
              .into_any()
            },
            None => {
              view! {
                <Icon name=IconName::Smile />
                <span class="menu-label muted-text">{move || i18n.t("status-custom-set")}</span>
              }
              .into_any()
            },
          }}
        </button>
        <div class="menu-sep" role="separator"></div>
      </Show>
      {PresenceStatus::ALL
        .into_iter()
        .map(|presence| {
          view! {
            <button
              class="menu-item"
              role="menuitemradio"
              tabindex="-1"
              aria-checked=move || (me.get().presence == presence).to_string()
              on:click=move |_| set_presence(presence)
            >
              <PresenceDot presence=presence />
              <span class="menu-label">
                {move || i18n.t(&format!("presence-{}", presence.slug()))}
                {(presence == PresenceStatus::DoNotDisturb)
                  .then(|| view! { <small>{move || i18n.t("presence-dnd-hint")}</small> })}
              </span>
              {move || (me.get().presence == presence).then(|| view! { <Icon name=IconName::Check /> })}
            </button>
          }
        })
        .collect_view()}
      <div class="menu-sub" role="group" aria-label=move || i18n.t("presence-dnd")>
        {move || dnd_until_label().map(|label| view! { <div class="menu-user-sub">{label}</div> })}
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| set_presence_until(PresenceStatus::DoNotDisturb, Some(now_ms() + MS_PER_HOUR))
        >
          <span class="menu-label">{move || i18n.t("presence-dnd-for-hour")}</span>
        </button>
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| set_presence_until(PresenceStatus::DoNotDisturb, Some(tomorrow_at(now_ms(), MORNING_MINUTE)))
        >
          <span class="menu-label">{move || i18n.t("presence-dnd-until-morning")}</span>
        </button>
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          aria-expanded=move || is_custom_open.get().to_string()
          on:click=move |_| is_custom_open.update(|is_open| *is_open = !*is_open)
        >
          <span class="menu-label">{move || i18n.t("presence-dnd-custom")}</span>
        </button>
        <Show when=move || is_custom_open.get() fallback=|| ()>
          <div class="field-row menu-custom-until">
            <label class="visually-hidden" for="dnd-custom-until">{move || i18n.t("presence-dnd-custom-label")}</label>
            <input
              id="dnd-custom-until"
              class="input grow"
              type="datetime-local"
              prop:value=move || custom_until.get()
              on:input=move |ev| custom_until.set(event_target_value(&ev))
              on:keydown=move |ev: web_sys::KeyboardEvent| {
                if ev.key() == "Enter" {
                  ev.prevent_default();
                  set_custom();
                }
              }
            />
            <button type="button" class="btn" on:click=move |_| set_custom()>
              {move || i18n.t("presence-dnd-custom-set")}
            </button>
          </div>
        </Show>
        <Show when=move || is_dnd.get() fallback=|| ()>
          <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| set_presence(PresenceStatus::Online)>
            <span class="menu-label">{move || i18n.t("presence-dnd-end")}</span>
          </button>
        </Show>
      </div>
      <div class="menu-sep" role="separator"></div>
      <Show when=move || is_admin.get() fallback=|| ()>
        <button
          class="menu-item"
          role="menuitem"
          tabindex="-1"
          on:click=move |_| state.open_admin(AdminSection::Channels)
        >
          <Icon name=IconName::Shield />
          <span class="menu-label">{move || i18n.t("menu-administration")}</span>
        </button>
      </Show>
      <Show when=|| crate::profiles::use_profiles().is_some() fallback=|| ()>
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.open_settings_at(SettingsTab::Account)>
          <Icon name=IconName::User />
          <span class="menu-label">{move || i18n.t("profile-edit-title")}</span>
        </button>
      </Show>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.open_settings()>
        <Icon name=IconName::Sliders />
        <span class="menu-label">{move || i18n.t("menu-settings")}</span>
      </button>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::Shortcuts)>
        <Icon name=IconName::Keyboard />
        <span class="menu-label">{move || i18n.t("shortcuts-title")}</span>
      </button>
      <button class="menu-item" role="menuitem" tabindex="-1" disabled title=move || i18n.t("admin-soon")>
        <Icon name=IconName::Users />
        <span class="menu-label">{move || i18n.t("menu-switch-account")}</span>
      </button>
      <button
        class="menu-item"
        role="menuitem"
        tabindex="-1"
        disabled=session.is_none()
        title=move || if session.is_none() { i18n.t("menu-log-out-demo") } else { String::new() }
        on:click=move |_| {
          if let Some(control) = session {
            state.close_overlay();
            control.sign_out();
          }
        }
      >
        <Icon name=IconName::Close />
        <span class="menu-label">{move || i18n.t("menu-log-out")}</span>
      </button>
    </MenuSurface>
  }
}

/// Dialog to set emoji, text and expiry of the custom status. Offers the workspace's presets (or
/// suggestions when it has none); with custom statuses switched off only the presets can be picked.
#[component]
pub fn CustomStatusDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = state.read(|backend| backend.presence_policy());
  let can_write_own = policy.custom_status_enabled;
  let current = state.me().maybe_custom_status;
  // The end of the current status, or of a preset with a duration; picking a "clear after"
  // choice replaces it.
  let chosen_end = RwSignal::new(current.as_ref().and_then(|status| status.maybe_expires_at));
  let emoji = RwSignal::new(
    current
      .as_ref()
      .map_or_else(String::new, |status| status.emoji.clone()),
  );
  let text = RwSignal::new(
    current
      .as_ref()
      .map(|status| status.text.clone())
      .unwrap_or_default(),
  );
  let expiry = RwSignal::new(
    current
      .as_ref()
      .map_or(StatusExpiry::Today, |status| status.expiry),
  );
  let has_current = current.is_some();
  let presets: Vec<Preset> = if policy.presets.is_empty() {
    if can_write_own {
      STATUS_PRESETS
        .into_iter()
        .map(|(emoji, key)| Preset {
          emoji: emoji.to_owned(),
          text: i18n.t(key),
          maybe_duration_minutes: None,
        })
        .collect()
    } else {
      Vec::new()
    }
  } else {
    policy
      .presets
      .iter()
      .map(|preset| Preset {
        emoji: preset.maybe_emoji.clone().unwrap_or_default(),
        text: preset.text.clone(),
        maybe_duration_minutes: preset.maybe_duration_minutes,
      })
      .collect()
  };
  let has_presets = !presets.is_empty();

  let save = move || {
    let status = CustomStatus {
      emoji: emoji.get_untracked().trim().to_owned(),
      text: text.get_untracked().trim().to_owned(),
      expiry: expiry.get_untracked(),
      maybe_expires_at: chosen_end.get_untracked().filter(|end| *end > now_ms()),
    };
    // Without text there is no status (the emoji alone is the default one).
    let is_empty = status.text.trim().is_empty();
    state.write(|backend| backend.set_custom_status((!is_empty).then_some(status)));
    state.close_overlay();
  };
  let chosen_end_label = move || {
    chosen_end
      .get()
      .map(|end| status_until_label(end, now_ms(), i18n))
  };

  view! {
    <Modal labelled_by="custom-status-title" class="modal-small">
      <form
        on:submit=move |ev| {
          ev.prevent_default();
          save();
        }
      >
        <header class="modal-head">
          <h2 id="custom-status-title">{move || i18n.t("status-custom-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <Show when=move || !can_write_own fallback=|| ()>
            <p class="hint">{move || i18n.t("status-presets-only")}</p>
          </Show>
          <Show when=move || can_write_own || !has_presets fallback=|| ()>
          <div class="field-row">
            <label class="visually-hidden" for="status-emoji">{move || i18n.t("status-emoji")}</label>
            <input
              id="status-emoji"
              class="input emoji-input"
              name="emoji"
              maxlength="8"
              autocomplete="off"
              prop:value=move || emoji.get()
              readonly=!can_write_own
              on:input=move |ev| emoji.set(event_target_value(&ev))
            />
            <label class="visually-hidden" for="status-text">{move || i18n.t("status-text")}</label>
            <input
              id="status-text"
              class="input grow"
              name="status"
              data-autofocus
              autocomplete="off"
              maxlength="100"
              placeholder=move || i18n.t("status-text-placeholder")
              readonly=!can_write_own
              prop:value=move || text.get()
              on:input=move |ev| text.set(event_target_value(&ev))
            />
          </div>
          </Show>
          <ul class="preset-list" aria-label=move || i18n.t("status-presets")>
            {presets
              .into_iter()
              .map(|preset| {
                let Preset { emoji: preset_emoji, text: preset_text, maybe_duration_minutes } = preset;
                let shown_emoji = preset_emoji.clone();
                let shown_text = preset_text.clone();
                let is_picked = {
                  let (preset_emoji, preset_text) = (preset_emoji.clone(), preset_text.clone());
                  move || emoji.get() == preset_emoji && text.get() == preset_text
                };
                view! {
                  <li>
                    <button
                      type="button"
                      class="preset"
                      aria-pressed=move || is_picked().to_string()
                      on:click=move |_| {
                        emoji.set(preset_emoji.clone());
                        text.set(preset_text.clone());
                        // A preset's duration is its end, unless the user picks another below.
                        if let Some(minutes) = maybe_duration_minutes {
                          chosen_end.set(Some(now_ms() + i64::from(minutes) * 60_000));
                        }
                      }
                    >
                      <span aria-hidden="true">{shown_emoji}</span>
                      <span translate="no">{shown_text}</span>
                    </button>
                  </li>
                }
              })
              .collect_view()}
          </ul>
          <div class="field">
            <label for="status-expiry">{move || i18n.t("status-clear-after")}</label>
            <select
              id="status-expiry"
              class="input"
              on:change=move |ev| {
                expiry.set(StatusExpiry::from_slug(&event_target_value(&ev)));
                chosen_end.set(None);
              }
            >
              {StatusExpiry::ALL
                .into_iter()
                .map(|option| {
                  view! {
                    <option value=option.slug() selected=move || expiry.get() == option>
                      {move || i18n.t(&format!("status-expiry-{}", option.slug()))}
                    </option>
                  }
                })
                .collect_view()}
            </select>
            {move || chosen_end_label().map(|label| view! { <p class="hint">{label}</p> })}
          </div>
        </div>
        <footer class="modal-foot">
          {has_current.then(|| {
            view! {
              <button
                type="button"
                class="btn btn-quiet"
                on:click=move |_| {
                  state.write(|backend| backend.set_custom_status(None));
                  state.close_overlay();
                }
              >
                {move || i18n.t("status-clear")}
              </button>
            }
          })}
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>
            {move || i18n.t("cancel")}
          </button>
          <button type="submit" class="btn btn-primary">
            {move || i18n.t("save")}
          </button>
        </footer>
      </form>
    </Modal>
  }
}

/// A status the dialog offers to pick.
struct Preset {
  emoji: String,
  text: String,
  maybe_duration_minutes: Option<u32>,
}

fn move_focus(ev: &web_sys::KeyboardEvent, menu: NodeRef<html::Div>, key: &str) {
  let Some(root) = menu.get_untracked() else { return };
  let Ok(list) = root.query_selector_all("[role^='menuitem']:not([disabled])") else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|i| list.item(i))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  if items.is_empty() {
    return;
  }
  let active = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok());
  let position = active.and_then(|active| items.iter().position(|item| item.is_same_node(Some(&active))));
  let target = match (key, position) {
    ("Home", _) => 0,
    ("End", _) => items.len() - 1,
    ("ArrowUp", Some(position)) => (position + items.len() - 1) % items.len(),
    ("ArrowUp", None) => items.len() - 1,
    (_, Some(position)) => (position + 1) % items.len(),
    (_, None) => 0,
  };
  drop(items[target].focus());
}

fn focus_account_button() {
  let button = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(".avatar-btn").ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(button) = button {
    drop(button.focus());
  }
}
