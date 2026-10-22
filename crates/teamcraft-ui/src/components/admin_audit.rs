use leptos::prelude::*;

use crate::admin::AuditAction;
use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Audit log of admin actions, recoveries and device changes: metadata only.
#[component]
pub fn AuditSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let action = RwSignal::new(None::<AuditAction>);
  let entries = Memo::new(move |_| state.read(|backend| backend.audit_log()));
  let visible = Memo::new(move |_| {
    let needle = query.get().trim().to_lowercase();
    let wanted = action.get();
    entries
      .get()
      .into_iter()
      .filter(|entry| wanted.is_none_or(|wanted| entry.action == wanted))
      .filter(|entry| {
        needle.is_empty()
          || entry.target.to_lowercase().contains(&needle)
          || entry.detail.to_lowercase().contains(&needle)
      })
      .collect::<Vec<_>>()
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-audit-title"))
      subtitle=Signal::derive(move || i18n.t("admin-audit-subtitle"))
    />
    <div class="admin-toolbar">
      <label class="visually-hidden" for="audit-search">{move || i18n.t("admin-search")}</label>
      <input
        id="audit-search"
        class="input"
        type="search"
        name="q"
        autocomplete="off"
        spellcheck="false"
        placeholder=move || i18n.t("admin-search-placeholder")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
      <label class="visually-hidden" for="audit-action">{move || i18n.t("audit-filter-action")}</label>
      <select
        id="audit-action"
        class="input"
        on:change=move |ev| {
          let value = event_target_value(&ev);
          action.set(AuditAction::ALL.into_iter().find(|candidate| candidate.slug() == value));
        }
      >
        <option value="">{move || i18n.t("audit-all-actions")}</option>
        {AuditAction::ALL
          .into_iter()
          .map(|candidate| {
            view! { <option value=candidate.slug()>{move || i18n.t(&format!("audit-{}", candidate.slug()))}</option> }
          })
          .collect_view()}
      </select>
      <span class="grow"></span>
      <button class="btn" disabled title=move || i18n.t("admin-soon")>
        <Icon name=IconName::Download />
        <span>{move || i18n.t("audit-export")}</span>
      </button>
    </div>
    <div class="table-wrap">
      <table class="data-table">
        <caption class="visually-hidden">{move || i18n.t("admin-audit-title")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("audit-col-time")}</th>
            <th scope="col">{move || i18n.t("audit-col-actor")}</th>
            <th scope="col">{move || i18n.t("audit-col-action")}</th>
            <th scope="col">{move || i18n.t("audit-col-target")}</th>
            <th scope="col">{move || i18n.t("audit-col-detail")}</th>
          </tr>
        </thead>
        <tbody>
          <For each=move || visible.get() key=|entry| entry.id let:entry>
            {
              let actor = state.read(|backend| backend.user(entry.actor)).map(|user| user.display_name).unwrap_or_default();
              let slug = entry.action.slug();
              view! {
                <tr>
                  <td class="nowrap">{move || format_date_time(entry.at, i18n.locale().tag())}</td>
                  <td translate="no">{actor}</td>
                  <td><span class="badge">{move || i18n.t(&format!("audit-{slug}"))}</span></td>
                  <td translate="no">{entry.target.clone()}</td>
                  <td class="muted-text" translate="no">{entry.detail.clone()}</td>
                </tr>
              }
            }
          </For>
        </tbody>
      </table>
      <Show when=move || visible.get().is_empty() fallback=|| ()>
        <p class="empty-state">{move || i18n.t("admin-empty")}</p>
      </Show>
    </div>
  }
}
