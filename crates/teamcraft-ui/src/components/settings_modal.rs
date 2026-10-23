use leptos::prelude::*;

use crate::auth::tauri;
use crate::calls::IncomingCall;
use crate::components::bundle_banner::AboutSection;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::settings_tabs::{
  CallsTab, DevicesTab, DisplayTab, LanguageTab, NotificationsTab, PrivacyTab, SidebarTab,
};
use crate::components::sidebar::shortcut_label;
use crate::i18n::use_i18n;
use crate::model::id_from_raw;
use crate::security::DeviceApproval;
use crate::settings::BackendMode;
use crate::state::{Overlay, use_app_state};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
  Notifications,
  Display,
  Sidebar,
  Language,
  Privacy,
  Devices,
  Calls,
  Advanced,
}

impl Tab {
  const ALL: [Tab; 8] = [
    Self::Notifications,
    Self::Display,
    Self::Sidebar,
    Self::Language,
    Self::Privacy,
    Self::Devices,
    Self::Calls,
    Self::Advanced,
  ];

  fn slug(self) -> &'static str {
    match self {
      Self::Notifications => "notifications",
      Self::Display => "display",
      Self::Sidebar => "sidebar",
      Self::Language => "language",
      Self::Privacy => "privacy",
      Self::Devices => "devices",
      Self::Calls => "calls",
      Self::Advanced => "advanced",
    }
  }
}

/// User settings, one tab per area (PLAN.md §4 and §5); the tabs live in `settings_tabs`.
#[component]
pub fn SettingsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let tab = RwSignal::new(Tab::Notifications);

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
                  data-autofocus=(candidate == Tab::Notifications).then_some("")
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
            Tab::Notifications => view! { <NotificationsTab /> }.into_any(),
            Tab::Display => view! { <DisplayTab /> }.into_any(),
            Tab::Sidebar => view! { <SidebarTab /> }.into_any(),
            Tab::Language => view! { <LanguageTab /> }.into_any(),
            Tab::Privacy => view! { <PrivacyTab /> }.into_any(),
            Tab::Devices => view! { <DevicesTab /> }.into_any(),
            Tab::Calls => view! { <CallsTab /> }.into_any(),
            Tab::Advanced => view! { <AdvancedTab /> }.into_any(),
          }}
        </div>
      </div>
    </Modal>
  }
}

/// Advanced settings. Until the real client core exists this tab also hosts the demo triggers that
/// stand in for events from the server.
#[component]
fn AdvancedTab() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    {tauri::is_available().then(|| view! { <DesktopSection /> })}
    <p class="muted-text">{move || i18n.t("settings-advanced-stub")}</p>
    <div class="field">
      <label for="setting-backend">{move || i18n.t("settings-backend")}</label>
      <select
        id="setting-backend"
        class="input"
        on:change=move |ev| {
          if let Some(mode) = BackendMode::from_slug(&event_target_value(&ev)) {
            mode.save();
            if let Some(window) = web_sys::window() {
              drop(window.location().reload());
            }
          }
        }
      >
        {[BackendMode::Mock, BackendMode::Server]
          .into_iter()
          .map(|mode| {
            view! {
              <option value=mode.slug() selected=move || BackendMode::load() == mode>
                {move || i18n.t(&format!("settings-backend-{}", mode.slug()))}
              </option>
            }
          })
          .collect_view()}
      </select>
      <p class="muted-text small">{move || i18n.t("settings-backend-hint")}</p>
    </div>
    <AboutSection />
    <h3 class="settings-heading">{move || i18n.t("settings-demo-title")}</h3>
    <p class="muted-text small">{move || i18n.t("settings-demo-hint")}</p>
    <div class="demo-actions">
      <button
        class="btn"
        on:click=move |_| {
          state.close_overlay();
          state.call.incoming.set(Some(IncomingCall {
            caller: id_from_raw(2),
            conversation: id_from_raw(30),
            with_video: true,
          }));
        }
      >
        {move || i18n.t("settings-demo-incoming")}
      </button>
      <button
        class="btn"
        on:click=move |_| {
          state.device_prompt.set(Some(DeviceApproval {
            device: id_from_raw(777),
            device_name: "Firefox on Linux".into(),
            location: "Berlin, Germany".into(),
            requested_at: 0,
            code: "4719".into(),
          }));
          state.overlay.set(Overlay::DeviceApproval);
        }
      >
        {move || i18n.t("settings-demo-login")}
      </button>
      <button class="btn" on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
        {move || i18n.t("settings-demo-link")}
      </button>
      <button class="btn" on:click=move |_| state.open_conversation(id_from_raw(32))>
        {move || i18n.t("settings-demo-key-change")}
      </button>
    </div>
  }
}

/// The user's own devices, with removal and linking of a new one.
/// Reference of the global keyboard shortcuts.
#[component]
pub fn ShortcutsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let rows: [(String, &'static str); 13] = [
    (shortcut_label("K"), "shortcut-switcher"),
    (shortcut_label(","), "shortcut-settings"),
    (shortcut_label("/"), "shortcut-shortcuts"),
    (
      format!("{}+Shift+F", shortcut_label("").trim_end_matches('+')),
      "shortcut-search",
    ),
    (shortcut_label("F"), "shortcut-search-here"),
    ("Alt+↑ / Alt+↓".to_owned(), "shortcut-prev-next"),
    ("Alt+Shift+↑ / Alt+Shift+↓".to_owned(), "shortcut-next-unread"),
    ("Alt+T".to_owned(), "shortcut-threads"),
    ("Ctrl+Shift+↑ / Ctrl+Shift+↓".to_owned(), "shortcut-move-favorite"),
    ("Esc".to_owned(), "shortcut-close"),
    ("↑ / ↓".to_owned(), "shortcut-messages"),
    ("Enter".to_owned(), "shortcut-open-thread"),
    (
      format!(
        "{} / {} / {}",
        shortcut_label("+"),
        shortcut_label("-"),
        shortcut_label("0")
      ),
      "shortcut-zoom",
    ),
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

/// Desktop app only: start at login, close to the tray, start minimized. The shell stores them.
#[component]
fn DesktopSection() -> impl IntoView {
  let i18n = use_i18n();
  let prefs = RwSignal::new(None::<tauri::ShellPrefs>);
  let autostart = RwSignal::new(None::<bool>);
  let error = RwSignal::new(None::<String>);
  leptos::task::spawn_local(async move {
    prefs.set(tauri::shell_prefs().await);
    autostart.set(tauri::autostart().await);
  });
  let save = move |change: fn(&mut tauri::ShellPrefs, bool), value: bool| {
    let Some(mut next) = prefs.get_untracked() else {
      return;
    };
    change(&mut next, value);
    prefs.set(Some(next));
    leptos::task::spawn_local(async move {
      error.set(tauri::set_shell_prefs(next).await.err());
    });
  };
  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("settings-desktop")}</legend>
      {move || autostart.get().map(|is_enabled| view! {
        <label class="check-row">
          <input
            type="checkbox"
            prop:checked=is_enabled
            on:change=move |ev| {
              let on = event_target_checked(&ev);
              autostart.set(Some(on));
              leptos::task::spawn_local(async move { error.set(tauri::set_autostart(on).await.err()) });
            }
          />
          <span>{move || i18n.t("settings-autostart")}</span>
        </label>
      })}
      {move || prefs.get().map(|current| view! {
        <label class="check-row">
          <input
            type="checkbox"
            prop:checked=current.is_close_to_tray
            on:change=move |ev| save(|prefs, on| prefs.is_close_to_tray = on, event_target_checked(&ev))
          />
          <span>
            {move || i18n.t("settings-close-to-tray")}
            <small class="muted-text">{move || i18n.t("settings-close-to-tray-hint")}</small>
          </span>
        </label>
        <label class="check-row">
          <input
            type="checkbox"
            prop:checked=current.is_start_minimized
            on:change=move |ev| save(|prefs, on| prefs.is_start_minimized = on, event_target_checked(&ev))
          />
          <span>{move || i18n.t("settings-start-minimized")}</span>
        </label>
      })}
      {move || error.get().map(|message| view! { <p class="composer-error" role="alert">{message}</p> })}
    </fieldset>
  }
}
