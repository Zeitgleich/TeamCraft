use leptos::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::sidebar::shortcut_label;
use crate::i18n::{Locale, use_i18n};
use crate::settings::{SendMode, Theme};
use crate::state::use_app_state;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
  Display,
  Sidebar,
  Notifications,
  Advanced,
}

impl Tab {
  const ALL: [Tab; 4] = [Self::Display, Self::Sidebar, Self::Notifications, Self::Advanced];

  fn slug(self) -> &'static str {
    match self {
      Self::Display => "display",
      Self::Sidebar => "sidebar",
      Self::Notifications => "notifications",
      Self::Advanced => "advanced",
    }
  }
}

/// User settings. Display and sidebar options work; notifications and advanced are placeholders.
#[component]
pub fn SettingsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let tab = RwSignal::new(Tab::Display);

  let on_tab_keys = move |ev: web_sys::KeyboardEvent| {
    let position = Tab::ALL
      .iter()
      .position(|candidate| *candidate == tab.get_untracked())
      .unwrap_or(0);
    let next = match ev.key().as_str() {
      "ArrowDown" | "ArrowRight" => (position + 1) % Tab::ALL.len(),
      "ArrowUp" | "ArrowLeft" => (position + Tab::ALL.len() - 1) % Tab::ALL.len(),
      _ => return,
    };
    ev.prevent_default();
    tab.set(Tab::ALL[next]);
    if let Some(button) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(&format!("settings-tab-{}", Tab::ALL[next].slug())))
    {
      use wasm_bindgen::JsCast;
      if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
        drop(button.focus());
      }
    }
  };

  view! {
    <Modal labelled_by="settings-title" class="modal-settings">
      <header class="modal-head">
        <h2 id="settings-title">{move || i18n.t("settings-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="settings-layout">
        <div class="settings-tabs" role="tablist" aria-orientation="vertical" on:keydown=on_tab_keys>
          {Tab::ALL
            .into_iter()
            .map(|candidate| {
              view! {
                <button
                  id=format!("settings-tab-{}", candidate.slug())
                  role="tab"
                  aria-selected=move || (tab.get() == candidate).to_string()
                  aria-controls=format!("settings-panel-{}", candidate.slug())
                  tabindex=move || if tab.get() == candidate { "0" } else { "-1" }
                  data-autofocus=(candidate == Tab::Display).then_some("")
                  on:click=move |_| tab.set(candidate)
                >
                  {move || i18n.t(&format!("settings-tab-{}", candidate.slug()))}
                </button>
              }
            })
            .collect_view()}
        </div>
        <div
          class="settings-panel"
          role="tabpanel"
          id=move || format!("settings-panel-{}", tab.get().slug())
          aria-labelledby=move || format!("settings-tab-{}", tab.get().slug())
        >
          {move || match tab.get() {
            Tab::Display => view! { <DisplayTab /> }.into_any(),
            Tab::Sidebar => view! { <SidebarTab /> }.into_any(),
            Tab::Notifications => view! { <StubTab key="settings-notifications-stub" /> }.into_any(),
            Tab::Advanced => view! { <StubTab key="settings-advanced-stub" /> }.into_any(),
          }}
        </div>
      </div>
    </Modal>
  }
}

#[component]
fn DisplayTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;

  view! {
    <div class="field">
      <label for="setting-theme">{move || i18n.t("settings-theme")}</label>
      <select
        id="setting-theme"
        class="input"
        on:change=move |ev| {
          if let Some(theme) = Theme::from_slug(&event_target_value(&ev)) {
            settings.theme.set(theme);
          }
        }
      >
        {Theme::ALL
          .into_iter()
          .map(|theme| {
            view! {
              <option value=theme.slug() selected=move || settings.theme.get() == theme>
                {move || i18n.t(&format!("settings-theme-{}", theme.slug()))}
              </option>
            }
          })
          .collect_view()}
      </select>
    </div>
    <div class="field">
      <label for="setting-language">{move || i18n.t("settings-language")}</label>
      <select
        id="setting-language"
        class="input"
        on:change=move |ev| {
          if let Some(locale) = Locale::from_tag(&event_target_value(&ev)) {
            settings.locale.set(locale);
          }
        }
      >
        {Locale::ALL
          .into_iter()
          .map(|locale| {
            view! {
              <option value=locale.tag() selected=move || settings.locale.get() == locale>
                {locale.native_name()}
              </option>
            }
          })
          .collect_view()}
      </select>
    </div>
    <fieldset class="field">
      <legend>{move || i18n.t("settings-send-mode")}</legend>
      {SendMode::ALL
        .into_iter()
        .map(|mode| {
          view! {
            <label class="radio">
              <input
                type="radio"
                name="send-mode"
                checked=move || settings.send_mode.get() == mode
                on:change=move |_| settings.send_mode.set(mode)
              />
              <span>{move || i18n.t(&format!("settings-send-{}", mode.slug()))}</span>
            </label>
          }
        })
        .collect_view()}
    </fieldset>
    <label class="checkbox">
      <input
        type="checkbox"
        prop:checked=move || settings.show_join_leave.get()
        on:change=move |ev| settings.show_join_leave.set(event_target_checked(&ev))
      />
      <span>{move || i18n.t("settings-join-leave")}</span>
    </label>
  }
}

#[component]
fn SidebarTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let settings = state.settings;
  view! {
    <label class="checkbox">
      <input
        type="checkbox"
        prop:checked=move || settings.show_unreads_group.get()
        on:change=move |ev| settings.show_unreads_group.set(event_target_checked(&ev))
      />
      <span>{move || i18n.t("settings-unreads-group")}</span>
    </label>
  }
}

#[component]
fn StubTab(key: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  view! { <p class="muted-text">{move || i18n.t(key)}</p> }
}

/// Reference of the global keyboard shortcuts.
#[component]
pub fn ShortcutsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let rows: [(String, &'static str); 8] = [
    (shortcut_label("K"), "shortcut-switcher"),
    (shortcut_label(","), "shortcut-settings"),
    (shortcut_label("/"), "shortcut-shortcuts"),
    ("Alt+↑ / Alt+↓".to_owned(), "shortcut-prev-next"),
    ("Alt+T".to_owned(), "shortcut-threads"),
    ("Esc".to_owned(), "shortcut-close"),
    ("↑ / ↓".to_owned(), "shortcut-messages"),
    ("Enter".to_owned(), "shortcut-open-thread"),
  ];
  view! {
    <Modal labelled_by="shortcuts-modal-title" class="modal-small">
      <header class="modal-head">
        <h2 id="shortcuts-modal-title">{move || i18n.t("shortcuts-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") data-autofocus on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <table class="shortcut-table">
        <tbody>
          {rows
            .into_iter()
            .map(|(keys, key)| {
              view! {
                <tr>
                  <th scope="row">{move || i18n.t(key)}</th>
                  <td><kbd>{keys}</kbd></td>
                </tr>
              }
            })
            .collect_view()}
        </tbody>
      </table>
    </Modal>
  }
}
