use leptos::prelude::*;
use teamcraft_proto::Role;

use crate::admin::{AdminUser, DeviceInfo};
use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::admin_console::SectionHeader;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::use_i18n;
use crate::model::{DeviceId, UserId};
use crate::reactive::{items_by_key, keyed_item};
use crate::state::use_app_state;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserTab {
  Users,
  Guests,
}

/// User list, devices and guests with their sponsors.
#[component]
pub fn UsersSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let tab = RwSignal::new(UserTab::Users);
  let selected = RwSignal::new(None::<UserId>);
  let now = now_ms();
  let users = Memo::new(move |_| state.read(|backend| backend.admin_users()));
  let shown = Memo::new(move |_| {
    let is_guests = tab.get() == UserTab::Guests;
    users
      .get()
      .into_iter()
      .filter(|entry| entry.maybe_guest.is_some() == is_guests)
      .collect::<Vec<_>>()
  });
  let selected_user = Memo::new(move |_| {
    selected
      .get()
      .and_then(|id| users.get().into_iter().find(|entry| entry.user.id == id))
  });
  // The detail is built again for another person or when what it offers changes; a refresh that
  // brings new "last seen" times or a presence updates the device list in place, so the pane
  // doesn't flicker and a started confirmation stays (issue #687).
  let detail_key = Memo::new(move |_| {
    selected_user.with(|maybe_entry| {
      maybe_entry.as_ref().map(|entry| {
        (
          entry.user.id,
          entry.user.display_name.clone(),
          entry.user.username.clone(),
          entry.is_active,
          entry.is_in_chats,
          entry.maybe_guest.is_some(),
        )
      })
    })
  });
  let selected_devices = Memo::new(move |_| {
    selected_user.with(|maybe_entry| {
      maybe_entry
        .as_ref()
        .map(|entry| entry.devices.clone())
        .unwrap_or_default()
    })
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-users-title"))
      subtitle=Signal::derive(move || i18n.t("admin-users-subtitle"))
    />
    <div class="admin-toolbar">
      <div class="segmented" role="group" aria-label=move || i18n.t("admin-users-tabs")>
        <SlidingIndicator />
        <button aria-pressed=move || (tab.get() == UserTab::Users).to_string() on:click=move |_| tab.set(UserTab::Users)>
          {move || i18n.t("admin-tab-users")}
        </button>
        <button aria-pressed=move || (tab.get() == UserTab::Guests).to_string() on:click=move |_| tab.set(UserTab::Guests)>
          {move || i18n.t("admin-tab-guests")}
        </button>
      </div>
      <span class="grow"></span>
      <button class="btn" disabled title=move || i18n.t("admin-soon")>
        <Icon name=IconName::Plus />
        <span>{move || if tab.get() == UserTab::Guests { i18n.t("admin-invite-guest") } else { i18n.t("admin-invite-user") }}</span>
      </button>
    </div>
    <div class="admin-split">
      <div class="table-wrap">
        <table class="data-table">
          <caption class="visually-hidden">{move || i18n.t("admin-users-title")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("admin-col-user")}</th>
              <th scope="col">{move || i18n.t("admin-col-role")}</th>
              <th scope="col" class="num">{move || i18n.t("admin-col-devices")}</th>
              <th scope="col">{move || i18n.t("admin-col-last-seen")}</th>
              {move || {
                if tab.get() == UserTab::Guests {
                  view! {
                    <th scope="col">{move || i18n.t("admin-col-sponsor")}</th>
                    <th scope="col">{move || i18n.t("admin-col-expires")}</th>
                  }
                  .into_any()
                } else {
                  view! { <th scope="col">{move || i18n.t("admin-col-spaces")}</th> }.into_any()
                }
              }}
            </tr>
          </thead>
          <tbody>
            <For each=move || shown.get() key=|entry| (entry.user.id, entry.is_active, entry.devices.len()) let:entry>
              <UserRow entry=entry selected=selected now=now guests=tab.get_untracked() == UserTab::Guests />
            </For>
          </tbody>
        </table>
        <Show when=move || shown.get().is_empty() fallback=|| ()>
          <p class="empty-state">{move || i18n.t("admin-empty")}</p>
        </Show>
      </div>
      <aside class="detail-pane" aria-label=move || i18n.t("admin-detail-label")>
        {move || match detail_key.with(Option::is_some).then(|| selected_user.get_untracked()).flatten() {
          Some(entry) => view! { <UserDetail entry=entry devices=selected_devices now=now /> }.into_any(),
          None => view! { <p class="empty-state">{move || i18n.t("admin-user-select")}</p> }.into_any(),
        }}
      </aside>
    </div>
  }
}

fn role_key(role: &Role) -> &'static str {
  match role {
    Role::Owner => "admin-role-owner",
    Role::Admin => "admin-role-admin",
    Role::Member => "admin-role-member",
    Role::Guest => "admin-role-guest",
    Role::Unknown(_) => "admin-role-unknown",
  }
}

#[component]
fn UserRow(entry: AdminUser, selected: RwSignal<Option<UserId>>, now: i64, guests: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = entry.user.id;
  let is_selected = move || selected.get() == Some(id);
  let seen = move || format_relative(entry.last_seen, now, i18n.locale().tag());
  let role = role_key(&entry.role);
  let guest_cells = entry.maybe_guest.map(|guest| {
    let sponsor = state
      .read(|backend| backend.user(guest.sponsor))
      .map(|user| user.display_name)
      .unwrap_or_default();
    let expires = move || format_date_time(guest.expires_at, i18n.locale().tag());
    view! {
      <td translate="no">{sponsor}</td>
      <td>{expires}</td>
    }
  });

  view! {
    <tr class:selected=is_selected class:inactive=!entry.is_active>
      <th scope="row">
        <button class="row-button" aria-pressed=move || is_selected().to_string() on:click=move |_| selected.set(Some(id))>
          <Avatar user=entry.user.clone() size=AvatarSize::Small />
          <span class="row-name" translate="no">{entry.user.display_name.clone()}</span>
          {(!entry.is_active).then(|| view! { <span class="badge">{move || i18n.t("admin-deactivated")}</span> })}
        </button>
      </th>
      <td><span class="badge">{move || i18n.t(role)}</span></td>
      <td class="num">{entry.devices.len()}</td>
      <td>{seen}</td>
      {if guests {
        guest_cells.into_any()
      } else {
        view! { <td translate="no">{entry.space_names.join(", ")}</td> }.into_any()
      }}
    </tr>
  }
}

#[component]
fn UserDetail(entry: AdminUser, devices: Memo<Vec<DeviceInfo>>, now: i64) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let user_id = entry.user.id;
  let by_id = items_by_key(devices.into(), |device: &DeviceInfo| device.id);
  let pending_device = RwSignal::new(None::<DeviceId>);
  let confirm_deactivate = RwSignal::new(false);
  let confirm_removal = RwSignal::new(false);
  let recovery_started = RwSignal::new(false);
  let (_, policy) = state.read(|backend| backend.workspace_policy());
  let is_recovery_allowed = if entry.maybe_guest.is_some() {
    policy.org_recovery.is_enabled_for_guests
  } else {
    policy.org_recovery.is_enabled_for_members
  };
  let is_recovery_available = state.read(|backend| backend.can_start_org_recovery());
  let can_recover = is_recovery_allowed && is_recovery_available;
  let recovery_hint = if !is_recovery_allowed {
    "admin-recovery-off"
  } else if !is_recovery_available {
    "admin-recovery-unavailable"
  } else {
    "admin-recovery-needs-second"
  };
  let is_active = entry.is_active;
  let can_remove_from_chats = !entry.is_active && entry.is_in_chats;
  // The removals are signed by this client (admin.md §3.2.1); without its keys nothing is offered.
  let can_sign_removals = state.read(|backend| backend.can_remove_from_chats());

  view! {
    <h3 translate="no">{entry.user.display_name.clone()}</h3>
    <p class="muted-text" translate="no">{format!("@{}", entry.user.username)}</p>
    <h4 class="detail-heading">{move || i18n.t("admin-devices")}</h4>
    <ul class="device-list">
      <For each=move || devices.get() key=|device| device.id let:device>
        {
          let device_id = device.id;
          let device = keyed_item(by_id, device_id, device);
          let is_pending = move || pending_device.get() == Some(device_id);
          let seen = move || format_relative(device.with(|device| device.last_seen), now, i18n.locale().tag());
          view! {
            <li class="device">
              <Icon name=IconName::Smartphone />
              <div class="device-info">
                <span translate="no">{move || device.with(|device| device.name.clone())}</span>
                <span class="muted-text small">{seen}</span>
              </div>
              <Show
                when=is_pending
                fallback=move || {
                  view! {
                    <button class="btn btn-small" on:click=move |_| pending_device.set(Some(device_id))>
                      {move || i18n.t("admin-device-remove")}
                    </button>
                  }
                }
              >
                <button
                  class="btn btn-small btn-danger"
                  on:click=move |_| {
                    state.write(|backend| backend.remove_device(user_id, device_id));
                    pending_device.set(None);
                  }
                >
                  {move || i18n.t("admin-device-confirm")}
                </button>
                <button class="btn btn-small" on:click=move |_| pending_device.set(None)>
                  {move || i18n.t("cancel")}
                </button>
              </Show>
            </li>
          }
        }
      </For>
      <Show when=move || devices.with(Vec::is_empty) fallback=|| ()>
        <li class="muted-text">{move || i18n.t("admin-no-devices")}</li>
      </Show>
    </ul>
    <div class="detail-actions">
      <Show
        when=move || confirm_deactivate.get()
        fallback=move || {
          view! {
            <button class="btn" on:click=move |_| {
              if is_active {
                confirm_deactivate.set(true);
              } else {
                state.write(|backend| backend.set_user_active(user_id, true));
              }
            }>
              {move || if is_active { i18n.t("admin-deactivate") } else { i18n.t("admin-reactivate") }}
            </button>
          }
        }
      >
        <button
          class="btn btn-danger"
          on:click=move |_| {
            state.write(|backend| backend.set_user_active(user_id, false));
            confirm_deactivate.set(false);
          }
        >
          {move || i18n.t("admin-deactivate-confirm")}
        </button>
        <button class="btn" on:click=move |_| confirm_deactivate.set(false)>{move || i18n.t("cancel")}</button>
      </Show>
      {(can_remove_from_chats && can_sign_removals)
        .then(|| {
          view! {
            <Show
              when=move || confirm_removal.get()
              fallback=move || {
                view! {
                  <button class="btn" on:click=move |_| confirm_removal.set(true)>
                    {move || i18n.t("admin-remove-from-chats")}
                  </button>
                }
              }
            >
              <button
                class="btn btn-danger"
                on:click=move |_| {
                  state.write(|backend| backend.remove_user_from_chats(user_id));
                  confirm_removal.set(false);
                }
              >
                {move || i18n.t("admin-remove-from-chats-confirm")}
              </button>
              <button class="btn" on:click=move |_| confirm_removal.set(false)>{move || i18n.t("cancel")}</button>
            </Show>
          }
        })}
      <button
        class="btn"
        disabled=!can_recover
        title=move || i18n.t(recovery_hint)
        on:click=move |_| {
          state.write(|backend| backend.start_org_recovery(user_id));
          recovery_started.set(true);
        }
      >
        {move || i18n.t("admin-recovery-start")}
      </button>
    </div>
    <p class="muted-text small">
      {move || {
        if confirm_deactivate.get() {
          Some(i18n.t("admin-deactivate-hint"))
        } else if confirm_removal.get() {
          Some(i18n.t("admin-remove-from-chats-hint"))
        } else if can_remove_from_chats && !can_sign_removals {
          Some(i18n.t("admin-remove-from-chats-needs-keys"))
        } else if can_remove_from_chats {
          Some(i18n.t("admin-deactivated-hint"))
        } else {
          None
        }
      }}
    </p>
    <p class="status-line" role="status">
      {move || recovery_started.get().then(|| i18n.t("admin-recovery-started"))}
    </p>
  }
}
