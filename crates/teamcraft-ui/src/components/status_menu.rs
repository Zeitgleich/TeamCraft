use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::avatar::{Avatar, AvatarSize, PresenceDot};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{CustomStatus, PresenceStatus, StatusExpiry};
use crate::state::{Overlay, use_app_state};

/// Presets an admin can configure later; the label is an i18n key.
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
  let me = Memo::new(move |_| state.me());

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

  let set_presence = move |presence: PresenceStatus| {
    state.write(|backend| backend.set_presence(presence));
    state.close_overlay();
    focus_account_button();
  };

  view! {
    <div class="popover-backdrop" on:mousedown=move |_| state.close_overlay()></div>
    <div class="popover status-menu" role="menu" aria-label=move || i18n.t("status-menu-label") node_ref=menu on:keydown=on_keydown>
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
      <button
        class="menu-item custom-status-row"
        role="menuitem"
        tabindex="-1"
        on:click=move |_| state.overlay.set(Overlay::CustomStatus)
      >
        {move || match me.get().maybe_custom_status {
          Some(status) => {
            view! {
              <span class="menu-emoji" aria-hidden="true">{status.emoji}</span>
              <span class="menu-label">{status.text}</span>
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
      <div class="menu-sep" role="separator"></div>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::Settings)>
        <Icon name=IconName::Sliders />
        <span class="menu-label">{move || i18n.t("menu-settings")}</span>
      </button>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::Shortcuts)>
        <Icon name=IconName::Keyboard />
        <span class="menu-label">{move || i18n.t("shortcuts-title")}</span>
      </button>
      <button class="menu-item" role="menuitem" tabindex="-1" disabled>
        <Icon name=IconName::Close />
        <span class="menu-label">{move || i18n.t("menu-log-out")}</span>
      </button>
    </div>
  }
}

/// Dialog to set emoji, text and expiry of the custom status.
#[component]
pub fn CustomStatusDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = state.me().maybe_custom_status;
  let emoji = RwSignal::new(
    current
      .as_ref()
      .map_or_else(|| "💬".to_owned(), |status| status.emoji.clone()),
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

  let save = move || {
    let status = CustomStatus {
      emoji: emoji.get_untracked(),
      text: text.get_untracked().trim().to_owned(),
      expiry: expiry.get_untracked(),
    };
    state.write(|backend| backend.set_custom_status((!status.text.is_empty()).then_some(status)));
    state.close_overlay();
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
          <div class="field-row">
            <label class="visually-hidden" for="status-emoji">{move || i18n.t("status-emoji")}</label>
            <input
              id="status-emoji"
              class="input emoji-input"
              name="emoji"
              maxlength="8"
              autocomplete="off"
              prop:value=move || emoji.get()
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
              prop:value=move || text.get()
              on:input=move |ev| text.set(event_target_value(&ev))
            />
          </div>
          <ul class="preset-list" aria-label=move || i18n.t("status-presets")>
            {STATUS_PRESETS
              .into_iter()
              .map(|(preset_emoji, key)| {
                view! {
                  <li>
                    <button
                      type="button"
                      class="preset"
                      on:click=move |_| {
                        emoji.set(preset_emoji.to_owned());
                        text.set(i18n.t(key));
                      }
                    >
                      <span aria-hidden="true">{preset_emoji}</span>
                      <span>{move || i18n.t(key)}</span>
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
              on:change=move |ev| expiry.set(StatusExpiry::from_slug(&event_target_value(&ev)))
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
