use std::time::Duration;

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::components::settings_account::AccountSection;
use crate::components::settings_keyboard::KeyboardSection;
use crate::components::settings_notifications::NotificationsSection;
use crate::components::settings_rows::row_id;
use crate::components::settings_search::{SettingsHit, search, section_label_key};
use crate::components::settings_security::{DevicesSection, SecuritySection};
use crate::components::settings_tabs::{
  AppearanceSection, AudioVideoSection, HelpSection, PreferencesSection,
};
use crate::history::{Travel, travel};
use crate::i18n::use_i18n;
use crate::state::{SettingsPage, SettingsTab as Tab, use_app_state};

pub use crate::components::settings_keyboard::ShortcutsModal;

/// How long a row found by the search stays highlighted.
const FOUND_HIGHLIGHT: Duration = Duration::from_millis(1600);

/// User settings (PLAN.md §4 and §5, issue #327): a labelled section list with a search on the
/// left, the section on the right. On phones the list is a page of its own and a section opens
/// over it, with Back to return.
#[component]
pub fn SettingsModal() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The page is part of the URL (`/settings`, `/settings/{tab}`), so it lives in the app state.
  let page = state.settings_page;
  let query = RwSignal::new(String::new());
  // Set when a phone opened the section from the list: Back then returns to that history entry.
  let came_from_list = RwSignal::new(false);
  let shown_tab = Memo::new(move |_| match page.get() {
    SettingsPage::Section(tab) => tab,
    SettingsPage::Sections => Tab::DEFAULT,
  });
  let is_list_page = Memo::new(move |_| page.get() == SettingsPage::Sections);

  let open = move |tab: Tab| {
    came_from_list.set(state.is_compact() && page.get_untracked() == SettingsPage::Sections);
    page.set(SettingsPage::Section(tab));
  };
  let open_hit = Callback::new(move |hit: SettingsHit| {
    open(hit.tab);
    query.set(String::new());
    if let Some(key) = hit.maybe_label_key {
      reveal_row(key);
    }
  });
  let back_to_list = move |_| {
    if came_from_list.get_untracked() {
      came_from_list.set(false);
      travel(Travel::Back);
    } else {
      page.set(SettingsPage::Sections);
    }
  };
  let page_title_id = "settings-page-title";

  view! {
    <Modal labelled_by="settings-title" class="modal-settings set-modal">
      <div class="set-shell" data-page=move || if is_list_page.get() { "list" } else { "section" }>
        <nav class="set-nav" aria-labelledby="settings-title">
          <header class="set-nav-head">
            <h2 id="settings-title">{move || i18n.t("settings-title")}</h2>
            <button type="button" class="icon-btn set-nav-close" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
              <Icon name=IconName::Close />
            </button>
          </header>
          <div class="set-search">
            <Icon name=IconName::Search class="set-search-icon" />
            <input
              type="search"
              class="input"
              enterkeyhint="search"
              autocomplete="off"
              aria-label=move || i18n.t("settings-search")
              aria-controls="settings-nav-list"
              placeholder=move || i18n.t("settings-search")
              data-autofocus=is_list_page.get_untracked().then_some("")
              prop:value=move || query.get()
              on:input=move |ev| query.set(event_target_value(&ev))
              on:keydown=move |ev| {
                if ev.key() == "Escape" && !query.get_untracked().is_empty() {
                  ev.stop_propagation();
                  query.set(String::new());
                }
              }
            />
          </div>
          <div id="settings-nav-list" class="set-nav-list" on:keydown=move_focus_in_list>
            <Show
              when=move || query.with(|text| text.trim().is_empty())
              fallback=move || view! { <SearchHits query=query on_open=open_hit /> }
            >
              <ul class="set-sections">
                {Tab::ALL
                  .into_iter()
                  .map(|tab| {
                    let is_current = move || !is_list_page.get() && shown_tab.get() == tab;
                    view! {
                      <li>
                        <button
                          type="button"
                          class="set-section"
                          aria-current=move || is_current().then_some("page")
                          data-autofocus=(!is_list_page.get_untracked() && tab == shown_tab.get_untracked()).then_some("")
                          on:click=move |_| open(tab)
                        >
                          <span class=format!("set-section-icon is-{}", tab.slug()) aria-hidden="true">
                            <Icon name=section_icon(tab) />
                          </span>
                          <span class="set-section-name">{move || i18n.t(&section_label_key(tab))}</span>
                          <span class="set-section-chevron" aria-hidden="true"><Icon name=IconName::ChevronRight /></span>
                        </button>
                      </li>
                    }
                  })
                  .collect_view()}
              </ul>
            </Show>
          </div>
        </nav>
        <section class="set-page" aria-labelledby=page_title_id>
          <header class="set-page-head">
            <button type="button" class="icon-btn set-back" aria-label=move || i18n.t("settings-back-to-list") on:click=back_to_list>
              <Icon name=IconName::ChevronLeft />
            </button>
            <h2 id=page_title_id>{move || i18n.t(&section_label_key(shown_tab.get()))}</h2>
            <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
              <Icon name=IconName::Close />
            </button>
          </header>
          <div class="set-page-body">
            {move || match shown_tab.get() {
              Tab::Account => view! { <AccountSection /> }.into_any(),
              Tab::Appearance => view! { <AppearanceSection /> }.into_any(),
              Tab::Notifications => view! { <NotificationsSection /> }.into_any(),
              Tab::Preferences => view! { <PreferencesSection /> }.into_any(),
              Tab::Keyboard => view! { <KeyboardSection /> }.into_any(),
              Tab::AudioVideo => view! { <AudioVideoSection /> }.into_any(),
              Tab::Devices => view! { <DevicesSection /> }.into_any(),
              Tab::Security => view! { <SecuritySection /> }.into_any(),
              Tab::About => view! { <HelpSection /> }.into_any(),
            }}
          </div>
        </section>
      </div>
    </Modal>
  }
}

/// What the search found: rows with their section, or a line saying there is nothing.
#[component]
fn SearchHits(query: RwSignal<String>, on_open: Callback<SettingsHit>) -> impl IntoView {
  let i18n = use_i18n();
  let hits = Memo::new(move |_| search(i18n, &query.get(), tauri::is_available()));
  view! {
    <p class="visually-hidden" role="status">
      {move || i18n.t_count("settings-search-count", hits.with(Vec::len))}
    </p>
    <Show
      when=move || !hits.with(Vec::is_empty)
      fallback=move || view! { <p class="set-empty">{move || i18n.t("settings-search-none")}</p> }
    >
      <ul class="set-sections is-hits">
        {move || {
          hits
            .get()
            .into_iter()
            .map(|hit| {
              let tab = hit.tab;
              let text = hit.text.clone();
              let is_section = hit.maybe_label_key.is_none();
              view! {
                <li>
                  <button type="button" class="set-section" on:click=move |_| on_open.run(hit.clone())>
                    <span class=format!("set-section-icon is-{}", tab.slug()) aria-hidden="true">
                      <Icon name=section_icon(tab) />
                    </span>
                    <span class="set-section-name">
                      {text}
                      {(!is_section).then(|| view! { <small>{move || i18n.t(&section_label_key(tab))}</small> })}
                    </span>
                  </button>
                </li>
              }
            })
            .collect_view()
        }}
      </ul>
    </Show>
  }
}

fn section_icon(tab: Tab) -> IconName {
  match tab {
    Tab::Account => IconName::User,
    Tab::Appearance => IconName::Sun,
    Tab::Notifications => IconName::Bell,
    Tab::Preferences => IconName::Sliders,
    Tab::Keyboard => IconName::Keyboard,
    Tab::AudioVideo => IconName::Headphones,
    Tab::Devices => IconName::Smartphone,
    Tab::Security => IconName::Shield,
    Tab::About => IconName::Info,
  }
}

/// Up and Down move between the entries of the section list, as in a native list.
fn move_focus_in_list(ev: web_sys::KeyboardEvent) {
  let step: i32 = match ev.key().as_str() {
    "ArrowDown" => 1,
    "ArrowUp" => -1,
    _ => return,
  };
  let Some(list) = ev
    .current_target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
  else {
    return;
  };
  let Ok(buttons) = list.query_selector_all("button.set-section") else {
    return;
  };
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let count = buttons.length() as i32;
  let current = (0..count).find(|index| {
    buttons
      .item(*index as u32)
      .zip(active.clone())
      .is_some_and(|(button, focused)| button.is_same_node(Some(&focused)))
  });
  let next = current.map_or(0, |index| (index + step).rem_euclid(count.max(1)));
  if let Some(button) = buttons
    .item(next as u32)
    .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
  {
    ev.prevent_default();
    drop(button.focus());
  }
}

/// After the section rendered: scrolls the found row into view, focuses its control and
/// highlights it for a moment. Rows inside a closed disclosure open it first.
fn reveal_row(label_key: &'static str) {
  request_animation_frame(move || {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
      return;
    };
    let id = row_id(label_key);
    let Some(row) = document
      .get_element_by_id(&id)
      .or_else(|| document.get_element_by_id(&format!("{id}-heading")))
    else {
      return;
    };
    if let Ok(Some(details)) = row.closest("details") {
      drop(details.set_attribute("open", ""));
    }
    // `scroll-margin` on the rows keeps them clear of the sticky header.
    row.scroll_into_view();
    const CONTROLS: &str = "input, select, button";
    let maybe_control = if row.matches(CONTROLS).unwrap_or(false) {
      Some(row.clone())
    } else {
      row.query_selector(CONTROLS).ok().flatten()
    };
    if let Some(control) = maybe_control.and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()) {
      drop(control.focus());
    }
    let classes = row.class_list();
    drop(classes.add_1("is-found"));
    set_timeout(move || drop(classes.remove_1("is-found")), FOUND_HIGHLIGHT);
  });
}
