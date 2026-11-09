use leptos::prelude::*;

use crate::admin::{AdminSection, admin_error_i18n_key};
use crate::components::admin_announcement::AnnouncementSection;
use crate::components::admin_audit::AuditSection;
use crate::components::admin_backgrounds::BackgroundsSection;
use crate::components::admin_brands::BrandsSection;
use crate::components::admin_calendar::CalendarSection;
use crate::components::admin_channels::ChannelsSection;
use crate::components::admin_health::HealthSection;
use crate::components::admin_import::ImportSection;
use crate::components::admin_instance::InstanceSection;
use crate::components::admin_integrations::IntegrationsSection;
use crate::components::admin_mail::MailSection;
use crate::components::admin_notetaker::NotetakerSection;
use crate::components::admin_people::PeopleSection;
use crate::components::admin_permissions::PermissionsSection;
use crate::components::admin_retention::RetentionSection;
use crate::components::admin_security::SecuritySection;
use crate::components::admin_settings::SettingsSection;
use crate::components::admin_updates::UpdatesPolicySection;
use crate::components::admin_users::UsersSection;
use crate::components::admin_webhooks::WebhooksSection;
use crate::components::icons::{Icon, IconName};
use crate::components::roster_banner::RosterBanner;
use crate::history::back_or_home;
use crate::i18n::use_i18n;
use crate::reactive::use_interval_while_visible;
use crate::state::use_app_state;

/// How often the console reloads the data of the open section while it is open and the page is
/// visible: admins watch audit entries and device lists change, nobody else needs the requests.
/// Only that section's lists are read (a few requests), never the whole console: the address's rate
/// limit is shared with every colleague behind it (issue #1217, docs/performance.md "Admin console").
pub(crate) const REFRESH_MS: u64 = 30_000;

/// Administration inside the normal client. Everything here is metadata: admins can manage rooms
/// without reading them (admin.md §2).
#[component]
pub fn AdminConsole(section: AdminSection) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // Backends that read the server fetch fresh data when the console opens (everything the first
  // time, then what this section shows) and while it stays open.
  state.read(|backend| backend.admin_refresh_section(section));
  use_interval_while_visible(REFRESH_MS, move || {
    state.read(|backend| backend.admin_refresh_section(section))
  });
  let maybe_error = Memo::new(move |_| state.read(|backend| backend.admin_error()));
  let alerts = Memo::new(move |_| state.read(|backend| backend.health_alert_count()));

  let nav_item = move |target: AdminSection, icon: IconName| {
    view! {
      <li>
        <button
          class="admin-nav-item"
          aria-current=move || (target == section).then_some("page")
          on:click=move |_| state.open_admin(target)
        >
          <Icon name=icon />
          <span>{move || i18n.t(&format!("admin-nav-{}", target.slug()))}</span>
          {(target == AdminSection::Health)
            .then(|| {
              view! {
                <Show when=move || { alerts.get() > 0 } fallback=|| ()>
                  <span class="badge-count" role="img" aria-label=move || i18n.t_count("health-alerts-badge", alerts.get())>
                    {move || alerts.get()}
                  </span>
                </Show>
              }
            })}
        </button>
      </li>
    }
  };

  view! {
    <section class="admin" aria-label=move || i18n.t("admin-title")>
      <nav class="admin-nav" aria-label=move || i18n.t("admin-nav-label")>
        <button class="admin-back" on:click=move |_| back_or_home(state)>
          <Icon name=IconName::ChevronRight class="flip" />
          <span>{move || i18n.t("admin-back")}</span>
        </button>
        <h1>{move || i18n.t("admin-title")}</h1>
        <ul>
          {nav_item(AdminSection::Channels, IconName::Layers)}
          {nav_item(AdminSection::Users, IconName::Users)}
          {nav_item(AdminSection::Permissions, IconName::Shield)}
          {nav_item(AdminSection::Security, IconName::Key)}
          {nav_item(AdminSection::People, IconName::User)}
          {nav_item(AdminSection::Announcement, IconName::Bell)}
          {nav_item(AdminSection::Backgrounds, IconName::Image)}
          {nav_item(AdminSection::Brands, IconName::Spotlight)}
          {nav_item(AdminSection::Mail, IconName::Send)}
          {nav_item(AdminSection::Calendar, IconName::Calendar)}
          {nav_item(AdminSection::Settings, IconName::Sliders)}
          {nav_item(AdminSection::Retention, IconName::Clock)}
          {nav_item(AdminSection::Updates, IconName::Download)}
          {nav_item(AdminSection::Health, IconName::Activity)}
          {nav_item(AdminSection::Instance, IconName::Monitor)}
          {nav_item(AdminSection::Integrations, IconName::Terminal)}
          {nav_item(AdminSection::Webhooks, IconName::Link)}
          {nav_item(AdminSection::Notetaker, IconName::Compose)}
          {nav_item(AdminSection::Import, IconName::Download)}
          {nav_item(AdminSection::Audit, IconName::List)}
        </ul>
      </nav>
      <div class="admin-content">
        // On every page: an admin role nobody signed is refused by other people's apps (#765).
        <RosterBanner />
        {move || {
          maybe_error.get().map(|detail| {
            let text = match admin_error_i18n_key(&detail) {
              Some(key) => i18n.t(key),
              None => i18n.t_with("admin-error", &[("detail", &detail)]),
            };
            // Sticky: a save refused far down the page still shows (forms.md, #1054).
            view! { <p class="auth-alert admin-error" role="alert">{text}</p> }
          })
        }}
        {move || match section {
          AdminSection::Channels => view! { <ChannelsSection /> }.into_any(),
          AdminSection::Users => view! { <UsersSection /> }.into_any(),
          AdminSection::Permissions => view! { <PermissionsSection /> }.into_any(),
          AdminSection::Security => view! { <SecuritySection /> }.into_any(),
          AdminSection::People => view! { <PeopleSection /> }.into_any(),
          AdminSection::Announcement => view! { <AnnouncementSection /> }.into_any(),
          AdminSection::Backgrounds => view! { <BackgroundsSection /> }.into_any(),
          AdminSection::Brands => view! { <BrandsSection /> }.into_any(),
          AdminSection::Mail => view! { <MailSection /> }.into_any(),
          AdminSection::Calendar => view! { <CalendarSection /> }.into_any(),
          AdminSection::Settings => view! { <SettingsSection /> }.into_any(),
          AdminSection::Health => view! { <HealthSection /> }.into_any(),
          AdminSection::Instance => view! { <InstanceSection /> }.into_any(),
          AdminSection::Retention => view! { <RetentionSection /> }.into_any(),
          AdminSection::Updates => view! { <UpdatesPolicySection /> }.into_any(),
          AdminSection::Integrations => view! { <IntegrationsSection /> }.into_any(),
          AdminSection::Webhooks => view! { <WebhooksSection /> }.into_any(),
          AdminSection::Notetaker => view! { <NotetakerSection /> }.into_any(),
          AdminSection::Import => view! { <ImportSection /> }.into_any(),
          AdminSection::Audit => view! { <AuditSection /> }.into_any(),
        }}
      </div>
    </section>
  }
}

/// Header of an admin section with the "metadata only" reminder.
#[component]
pub fn SectionHeader(
  #[prop(into)] title: Signal<String>,
  #[prop(into)] subtitle: Signal<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <header class="admin-section-head">
      <div>
        <h2>{move || title.get()}</h2>
        <p class="muted-text">{move || subtitle.get()}</p>
      </div>
      <span class="metadata-pill" title=move || i18n.t("admin-metadata-detail")>
        <Icon name=IconName::EyeOff />
        <span>{move || i18n.t("admin-metadata-only")}</span>
      </span>
    </header>
  }
}

/// The foot of a list that loads page by page (issue #1241): "Show more" while the instance has
/// more entries than the list holds. The next page also loads by itself when the foot scrolls into
/// view (issue #1433); the button stays for a failed read, which the console banner reports and
/// which isn't retried on its own, and for browsers without the observer.
#[component]
pub fn ListMore(#[prop(into)] has_more: Signal<bool>, #[prop(into)] on_more: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let is_loading = Memo::new(move |_| state.read(|backend| backend.admin_is_loading_more()));
  let has_error = Memo::new(move |_| state.read(|backend| backend.admin_error()).is_some());
  let sentinel = NodeRef::<leptos::html::Div>::new();
  let is_near_end = RwSignal::new(false);
  let observer = StoredValue::new_local(None::<NearEndObserver>);
  Effect::new(move |_| {
    let Some(element) = sentinel.get() else {
      return;
    };
    if observer.with_value(Option::is_none) {
      observer.set_value(NearEndObserver::new(&element, is_near_end));
    }
  });
  on_cleanup(move || {
    if let Some(Some(observer)) = observer.try_update_value(Option::take) {
      observer.disconnect();
    }
  });
  // Runs again when a page arrived and the foot is still in view, so a short list fills the screen.
  Effect::new(move |_| {
    if is_near_end.get() && has_more.get() && !is_loading.get() && !has_error.get() {
      on_more.run(());
    }
  });
  view! {
    <div class="admin-more-sentinel" node_ref=sentinel aria-hidden="true"></div>
    <Show when=move || has_more.get() fallback=|| ()>
      <div class="admin-more">
        <button
          type="button"
          class="btn"
          prop:disabled=move || is_loading.get()
          aria-busy=move || is_loading.get().then_some("true")
          on:click=move |_| on_more.run(())
        >
          {move || i18n.t("admin-show-more")}
        </button>
      </div>
    </Show>
  }
}

/// Follows whether the foot of a list is on screen.
#[cfg(target_arch = "wasm32")]
struct NearEndObserver {
  observer: web_sys::IntersectionObserver,
  _callback: wasm_bindgen::closure::Closure<dyn FnMut(js_sys::Array)>,
}

#[cfg(target_arch = "wasm32")]
impl NearEndObserver {
  fn new(element: &web_sys::HtmlDivElement, is_near_end: RwSignal<bool>) -> Option<Self> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    use crate::reactive::SetIfAlive;

    let callback = Closure::<dyn FnMut(js_sys::Array)>::new(move |entries: js_sys::Array| {
      // The last entry is the newest state of the one observed element.
      if let Some(entry) = entries
        .iter()
        .last()
        .and_then(|entry| entry.dyn_into::<web_sys::IntersectionObserverEntry>().ok())
      {
        is_near_end.set_if_alive(entry.is_intersecting());
      }
    });
    let observer = web_sys::IntersectionObserver::new(callback.as_ref().unchecked_ref()).ok()?;
    observer.observe(element);
    Some(Self {
      observer,
      _callback: callback,
    })
  }

  fn disconnect(&self) {
    self.observer.disconnect();
  }
}

#[cfg(not(target_arch = "wasm32"))]
struct NearEndObserver;

#[cfg(not(target_arch = "wasm32"))]
impl NearEndObserver {
  fn new(_element: &web_sys::HtmlDivElement, _is_near_end: RwSignal<bool>) -> Option<Self> {
    None
  }

  fn disconnect(&self) {}
}

/// Says that a list ends before the instance's last entry: the console reads a bounded number of
/// pages, and an admin must not take the end of the list for the end of the instance.
#[component]
pub fn ListCutNotice(#[prop(into)] is_cut: Signal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    is_cut
      .get()
      .then(|| view! { <p class="muted-text small" role="status">{i18n.t("admin-list-cut")}</p> })
  }
}
