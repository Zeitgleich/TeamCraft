//! The guests tab of the admin console (issue #212): every guest membership, soonest expiry first,
//! with filters and bulk extend and remove. Metadata only; admin.md §3.2.2.

use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_proto::{AdminGuestFilter, AdminGuestResponse, ListAdminGuestsQuery, Role, SpaceId, UserId};

use crate::clock::format_date;
use crate::components::admin_console::ListCutNotice;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// One guest membership.
type Key = (SpaceId, UserId);

#[component]
pub fn GuestsOverview() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(ListAdminGuestsQuery::default());
  let selected = RwSignal::new(HashSet::<Key>::new());
  let is_confirming_removal = RwSignal::new(false);
  Effect::new(move |_| {
    let current = query.get();
    state.write(|backend| backend.set_admin_guest_query(current));
  });
  let list = Memo::new(move |_| state.read(|backend| backend.admin_guests()));
  let chosen = move || {
    let picked = selected.get();
    list.with(|list| {
      list
        .guests
        .iter()
        .filter(|guest| picked.contains(&(guest.space_id, guest.user.id)))
        .cloned()
        .collect::<Vec<_>>()
    })
  };
  let extend = move |days: i64| {
    let requests: Vec<_> = chosen()
      .into_iter()
      .filter_map(|guest| {
        let expires_at = guest.maybe_expires_at?;
        Some((guest.space_id, guest.user.id, expires_at.0 + days * DAY_MS))
      })
      .collect();
    state.write(|backend| backend.admin_extend_guests(requests));
    selected.set(HashSet::new());
  };
  let remove = move || {
    let requests: Vec<_> = chosen()
      .into_iter()
      .map(|guest| (guest.space_id, guest.user.id))
      .collect();
    state.write(|backend| backend.admin_remove_guests(requests));
    selected.set(HashSet::new());
    is_confirming_removal.set(false);
  };
  let spaces = Memo::new(move |_| state.read(|backend| backend.admin_spaces()));
  let sponsors = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .admin_users()
        .into_iter()
        .filter(|entry| entry.role != Role::Guest && entry.is_active)
        .map(|entry| (entry.user.id, entry.user.display_name))
        .collect::<Vec<_>>()
    })
  });
  view! {
    <div class="admin-toolbar guest-filters">
      <label class="field-inline">
        <span>{move || i18n.t("admin-guests-filter")}</span>
        <select class="input" on:change=move |event| {
          let value = event_target_value(&event);
          query.update(|query| query.maybe_filter = (!value.is_empty()).then(|| AdminGuestFilter::from(value)));
        }>
          <option value="">{move || i18n.t("admin-guests-filter-all")}</option>
          <option value="expiring_soon">{move || i18n.t("admin-guests-filter-expiring")}</option>
          <option value="no_sponsor">{move || i18n.t("admin-guests-filter-no-sponsor")}</option>
          <option value="never_signed_in">{move || i18n.t("admin-guests-filter-never")}</option>
        </select>
      </label>
      <label class="field-inline">
        <span>{move || i18n.t("admin-col-space")}</span>
        <select class="input" on:change=move |event| {
          let value = event_target_value(&event);
          query.update(|query| query.maybe_space_id = value.parse().ok());
        }>
          <option value="">{move || i18n.t("admin-guests-any")}</option>
          <For each=move || spaces.get() key=|space| space.id let:space>
            <option value=space.id.to_string() translate="no">{space.name.clone()}</option>
          </For>
        </select>
      </label>
      <label class="field-inline">
        <span>{move || i18n.t("admin-col-sponsor")}</span>
        <select class="input" on:change=move |event| {
          let value = event_target_value(&event);
          query.update(|query| query.maybe_sponsor_id = value.parse().ok());
        }>
          <option value="">{move || i18n.t("admin-guests-any")}</option>
          <For each=move || sponsors.get() key=|sponsor| sponsor.0 let:sponsor>
            <option value=sponsor.0.to_string() translate="no">{sponsor.1.clone()}</option>
          </For>
        </select>
      </label>
    </div>
    <Show when=move || !chosen().is_empty() fallback=|| ()>
      <div class="admin-toolbar guest-bulk" role="group" aria-label=move || i18n.t("admin-guests-bulk")>
        <span>{move || i18n.t_count("admin-guests-selected", chosen().len())}</span>
        {move || {
          let without_expiry = chosen().iter().filter(|guest| guest.maybe_expires_at.is_none()).count();
          (without_expiry > 0).then(|| view! { <span class="muted-text small">{move || i18n.t_count("admin-guests-skipped", without_expiry)}</span> })
        }}
        <button type="button" class="btn btn-small" on:click=move |_| extend(30)>{move || i18n.t_count("guest-notice-extend", 30)}</button>
        <button type="button" class="btn btn-small" on:click=move |_| extend(90)>{move || i18n.t_count("guest-notice-extend", 90)}</button>
        <button type="button" class="btn btn-small btn-danger" on:click=move |_| is_confirming_removal.set(true)>{move || i18n.t("admin-guests-remove")}</button>
      </div>
      <Show when=move || is_confirming_removal.get() fallback=|| ()>
        <div class="guest-bulk" role="alert">
          <p>{move || i18n.t_count("admin-guests-remove-confirm", chosen().len())}</p>
          <button type="button" class="btn btn-small btn-danger" on:click=move |_| remove()>{move || i18n.t("admin-guests-remove")}</button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_removal.set(false)>{move || i18n.t("cancel")}</button>
        </div>
      </Show>
    </Show>
    <div class="table-wrap">
      <table class="data-table">
        <caption class="visually-hidden">{move || i18n.t("admin-tab-guests")}</caption>
        <thead>
          <tr>
            <th scope="col"><span class="visually-hidden">{move || i18n.t("admin-guests-bulk")}</span></th>
            <th scope="col">{move || i18n.t("admin-col-user")}</th>
            <th scope="col">{move || i18n.t("admin-col-space")}</th>
            <th scope="col">{move || i18n.t("admin-col-sponsor")}</th>
            <th scope="col">{move || i18n.t("admin-col-expires")}</th>
          </tr>
        </thead>
        <tbody>
          <For
            each=move || list.with(|list| list.guests.clone())
            key=|guest| (guest.space_id, guest.user.id, guest.maybe_expires_at, guest.maybe_sponsor.as_ref().map(|sponsor| sponsor.id))
            let:guest
          >
            <GuestRow guest=guest selected=selected />
          </For>
        </tbody>
      </table>
      <Show when=move || list.with(|list| list.guests.is_empty()) fallback=|| ()>
        <p class="empty-state">{move || i18n.t("admin-empty")}</p>
      </Show>
      <ListCutNotice is_cut=Signal::derive(move || list.with(|list| list.has_more)) />
    </div>
  }
}

#[component]
fn GuestRow(guest: AdminGuestResponse, selected: RwSignal<HashSet<Key>>) -> impl IntoView {
  let i18n = use_i18n();
  let key = (guest.space_id, guest.user.id);
  let name = guest.user.display_name.clone();
  let label = name.clone();
  let expires = guest.maybe_expires_at;
  view! {
    <tr>
      <td>
        <input
          type="checkbox"
          aria-label=move || i18n.t_with("admin-guests-select", &[("name", &label)])
          prop:checked=move || selected.with(|picked| picked.contains(&key))
          on:change=move |event| {
            let is_checked = event_target_checked(&event);
            selected.update(|picked| {
              if is_checked {
                picked.insert(key);
              } else {
                picked.remove(&key);
              }
            });
          }
        />
      </td>
      <th scope="row">
        <span class="row-name" translate="no">{name}</span>
        {(!guest.has_signed_in).then(|| view! { <span class="badge">{move || i18n.t("admin-guests-never")}</span> })}
      </th>
      <td translate="no">{guest.space_name.clone()}</td>
      <td translate="no">
        {match guest.maybe_sponsor {
          Some(sponsor) => sponsor.display_name.into_any(),
          None => view! { <span class="badge badge-warn">{move || i18n.t("admin-guests-no-sponsor")}</span> }.into_any(),
        }}
      </td>
      <td>
        {move || match expires {
          Some(at) => format_date(at.0, i18n.locale().tag()),
          None => i18n.t("admin-guests-no-expiry"),
        }}
      </td>
    </tr>
  }
}
