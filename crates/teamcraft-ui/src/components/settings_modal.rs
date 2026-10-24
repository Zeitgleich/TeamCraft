use leptos::prelude::*;
use teamcraft_client_api::window_chrome::TitleBar;

use crate::auth::tauri;
use crate::components::bundle_banner::AboutSection;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::settings_tabs::{
  CallsTab, DevicesTab, DisplayTab, LanguageTab, NotificationsTab, PrivacyTab, SidebarTab,
};
use crate::components::tab_list::{on_tab_keys, roving_tabindex};
use crate::components::update_notice::UpdatesSection;
use crate::i18n::use_i18n;
use crate::keys::{ENTER, ESCAPE, Key, Modifier, QUICK_SWITCHER, Shortcut, ShortcutList};
use crate::reactive::SetIfAlive;
use crate::state::{SettingsTab as Tab, use_app_state};

/// User settings, one tab per area (PLAN.md §4 and §5); the tabs live in `settings_tabs`.
#[component]
pub fn SettingsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The tab is part of the URL (`/settings/{tab}`), so it lives in the app state.
  let tab = state.settings_tab;

  let on_tablist_keydown = move |ev: web_sys::KeyboardEvent| {
    on_tab_keys(&ev, &Tab::ALL, tab, |candidate| {
      format!("settings-tab-{}", candidate.slug())
    });
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
        <div class="settings-tabs" role="tablist" aria-orientation="vertical" on:keydown=on_tablist_keydown>
          {Tab::ALL
            .into_iter()
            .map(|candidate| {
              view! {
                <button
                  id=format!("settings-tab-{}", candidate.slug())
                  role="tab"
                  aria-selected=move || (tab.get() == candidate).to_string()
                  aria-controls=format!("settings-panel-{}", candidate.slug())
                  tabindex=move || roving_tabindex(tab.get() == candidate)
                  data-autofocus=(candidate == tab.get_untracked()).then_some("")
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

/// Advanced settings: the desktop shell's options, updates and what is running.
#[component]
fn AdvancedTab() -> impl IntoView {
  let i18n = use_i18n();
  view! {
    {tauri::is_available().then(|| view! { <DesktopSection /> <UpdatesSection /> })}
    <p class="muted-text">{move || i18n.t("settings-advanced-stub")}</p>
    {demo_tools()}
    <AboutSection />
  }
}

#[cfg(not(feature = "demo"))]
fn demo_tools() -> impl IntoView {}

/// Development builds only: the data source and triggers that stand in for events from the server.
#[cfg(feature = "demo")]
fn demo_tools() -> impl IntoView {
  use crate::calls::IncomingCall;
  use crate::model::id_from_raw;
  use crate::security::DeviceApproval;
  use crate::settings::BackendMode;
  use crate::state::Overlay;

  let state = use_app_state();
  let i18n = use_i18n();
  view! {
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
  let rows: [(Vec<Shortcut>, &'static str); 13] = [
    (
      vec![
        QUICK_SWITCHER,
        Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Char("F")),
      ],
      "shortcut-switcher",
    ),
    (vec![Shortcut::primary(",")], "shortcut-settings"),
    (vec![Shortcut::primary("/")], "shortcut-shortcuts"),
    (vec![Shortcut::primary("F")], "shortcut-search-here"),
    (
      vec![
        Shortcut::new(&[Modifier::Alt], Key::Up),
        Shortcut::new(&[Modifier::Alt], Key::Down),
      ],
      "shortcut-prev-next",
    ),
    (
      vec![
        Shortcut::new(&[Modifier::Alt, Modifier::Shift], Key::Up),
        Shortcut::new(&[Modifier::Alt, Modifier::Shift], Key::Down),
      ],
      "shortcut-next-unread",
    ),
    (
      vec![Shortcut::new(&[Modifier::Alt], Key::Char("T"))],
      "shortcut-threads",
    ),
    (
      vec![
        Shortcut::new(&[Modifier::Alt], Key::Left),
        Shortcut::new(&[Modifier::Alt], Key::Right),
      ],
      "shortcut-back-forward",
    ),
    (
      vec![
        Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Up),
        Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Down),
      ],
      "shortcut-move-favorite",
    ),
    (vec![ESCAPE], "shortcut-close"),
    (
      vec![Shortcut::new(&[], Key::Up), Shortcut::new(&[], Key::Down)],
      "shortcut-messages",
    ),
    (vec![ENTER], "shortcut-open-thread"),
    (
      vec![
        Shortcut::primary("+"),
        Shortcut::primary("-"),
        Shortcut::primary("0"),
      ],
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
                  <td><ShortcutList shortcuts=keys /></td>
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
    prefs.set_if_alive(tauri::shell_prefs().await);
    autostart.set_if_alive(tauri::autostart().await);
  });
  let save = move |change: fn(&mut tauri::ShellPrefs, bool), value: bool| {
    let Some(mut next) = prefs.get_untracked() else {
      return;
    };
    change(&mut next, value);
    prefs.set(Some(next));
    leptos::task::spawn_local(async move {
      error.set_if_alive(tauri::set_shell_prefs(next).await.err());
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
              leptos::task::spawn_local(async move { error.set_if_alive(tauri::set_autostart(on).await.err()) });
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
        <label class="check-row">
          <input
            type="checkbox"
            prop:checked=current.title_bar == TitleBar::System
            on:change=move |ev| save(
              |prefs, on| prefs.title_bar = if on { TitleBar::System } else { TitleBar::Custom },
              event_target_checked(&ev),
            )
          />
          <span>
            {move || i18n.t("settings-system-title-bar")}
            <small class="muted-text">{move || i18n.t("settings-system-title-bar-hint")}</small>
          </span>
        </label>
      })}
      {move || error.get().map(|message| view! { <p class="composer-error" role="alert">{message}</p> })}
    </fieldset>
  }
}
