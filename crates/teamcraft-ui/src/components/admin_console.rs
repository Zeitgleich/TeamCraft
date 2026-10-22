use leptos::prelude::*;

use crate::admin::AdminSection;
use crate::components::admin_audit::AuditSection;
use crate::components::admin_channels::ChannelsSection;
use crate::components::admin_retention::RetentionSection;
use crate::components::admin_security::SecuritySection;
use crate::components::admin_users::UsersSection;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Administration inside the normal client. Everything here is metadata: admins can manage rooms
/// without reading them (admin.md §2).
#[component]
pub fn AdminConsole(section: AdminSection) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();

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
        </button>
      </li>
    }
  };

  view! {
    <section class="admin" aria-label=move || i18n.t("admin-title")>
      <nav class="admin-nav" aria-label=move || i18n.t("admin-nav-label")>
        <h1>{move || i18n.t("admin-title")}</h1>
        <ul>
          {nav_item(AdminSection::Channels, IconName::Layers)}
          {nav_item(AdminSection::Users, IconName::Users)}
          {nav_item(AdminSection::Security, IconName::Key)}
          {nav_item(AdminSection::Retention, IconName::Clock)}
          {nav_item(AdminSection::Audit, IconName::List)}
        </ul>
        <button class="admin-back" on:click=move |_| state.exit_admin()>
          <Icon name=IconName::ChevronRight class="flip" />
          <span>{move || i18n.t("admin-back")}</span>
        </button>
      </nav>
      <div class="admin-content">
        {move || match section {
          AdminSection::Channels => view! { <ChannelsSection /> }.into_any(),
          AdminSection::Users => view! { <UsersSection /> }.into_any(),
          AdminSection::Security => view! { <SecuritySection /> }.into_any(),
          AdminSection::Retention => view! { <RetentionSection /> }.into_any(),
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
