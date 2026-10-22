use leptos::prelude::*;

use crate::admin::{AuditAction, AuditFilter};
use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::UserId;
use crate::state::use_app_state;

const DAY_MS: i64 = 86_400_000;

/// Audit log of admin actions, recoveries and device changes: metadata only.
#[component]
pub fn AuditSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let action = RwSignal::new(None::<AuditAction>);
  let actor = RwSignal::new(None::<UserId>);
  let since = RwSignal::new(String::new());
  let until = RwSignal::new(String::new());
  let users = Memo::new(move |_| state.read(|backend| backend.admin_users()));
  // The server filters by actor and time, so a long log does not have to be loaded to be searched.
  let apply = move || {
    let filter = AuditFilter {
      maybe_actor: actor.get_untracked(),
      maybe_since: utc_day_start_ms(&since.get_untracked()),
      maybe_until: utc_day_start_ms(&until.get_untracked()).map(|start| start + DAY_MS - 1),
    };
    state.write(|backend| backend.set_audit_filter(filter));
  };
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
      <label class="visually-hidden" for="audit-actor">{move || i18n.t("audit-filter-actor")}</label>
      <select
        id="audit-actor"
        class="input"
        on:change=move |ev| {
          let value = event_target_value(&ev);
          actor.set(users.get_untracked().into_iter().map(|entry| entry.user.id).find(|id| id.to_string() == value));
          apply();
        }
      >
        <option value="">{move || i18n.t("audit-all-actors")}</option>
        <For each=move || users.get() key=|entry| entry.user.id let:entry>
          <option value=entry.user.id.to_string() translate="no">{entry.user.display_name.clone()}</option>
        </For>
      </select>
      <label for="audit-since">{move || i18n.t("audit-filter-since")}</label>
      <input
        id="audit-since"
        class="input"
        type="date"
        prop:value=move || since.get()
        on:change=move |ev| {
          since.set(event_target_value(&ev));
          apply();
        }
      />
      <label for="audit-until">{move || i18n.t("audit-filter-until")}</label>
      <input
        id="audit-until"
        class="input"
        type="date"
        prop:value=move || until.get()
        on:change=move |ev| {
          until.set(event_target_value(&ev));
          apply();
        }
      />
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

/// Start of the UTC day of a `YYYY-MM-DD` date input, in Unix milliseconds. Empty or malformed
/// input means "no limit".
pub fn utc_day_start_ms(date: &str) -> Option<i64> {
  let mut parts = date.split('-');
  let year: i64 = parts.next()?.parse().ok()?;
  let month: i64 = parts.next()?.parse().ok()?;
  let day: i64 = parts.next()?.parse().ok()?;
  if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
    return None;
  }
  // Days since 1970-01-01 (Howard Hinnant's civil-from-days, inverted).
  let year = if month <= 2 { year - 1 } else { year };
  let era = year.div_euclid(400);
  let year_of_era = year - era * 400;
  let month_index = (month + 9) % 12;
  let day_of_year = (153 * month_index + 2) / 5 + day - 1;
  let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
  Some((era * 146_097 + day_of_era - 719_468) * DAY_MS)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn dates_become_utc_midnight() {
    assert_eq!(utc_day_start_ms("1970-01-01"), Some(0));
    assert_eq!(utc_day_start_ms("2026-10-22"), Some(1_792_627_200_000));
    assert_eq!(utc_day_start_ms("2024-02-29"), Some(1_709_164_800_000));
  }

  #[test]
  fn bad_dates_mean_no_limit() {
    for input in ["", "2026", "2026-13-01", "x-y-z", "2026-10-22-1"] {
      assert_eq!(utc_day_start_ms(input), None, "{input}");
    }
  }
}
