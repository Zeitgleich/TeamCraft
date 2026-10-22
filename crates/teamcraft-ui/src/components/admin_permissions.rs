use leptos::prelude::*;
use teamcraft_proto::{
  ALL_PERMISSIONS, OverrideEffect, Permission, PermissionScope, SchemeChange, SchemeRole, SchemeRoleResponse,
  check_scheme_change,
};

use crate::admin::AdminSpace;
use crate::components::admin_console::SectionHeader;
use crate::i18n::use_i18n;
use crate::model::SpaceId;
use crate::state::use_app_state;

const ROLES: [SchemeRole; 6] = [
  SchemeRole::Guest,
  SchemeRole::Member,
  SchemeRole::SpaceAdmin,
  SchemeRole::ChannelAdmin,
  SchemeRole::Admin,
  SchemeRole::Owner,
];

/// Permission groups in the order the matrix shows them.
const GROUPS: [PermissionScope; 4] = [
  PermissionScope::ChannelContent,
  PermissionScope::ChannelManagement,
  PermissionScope::Space,
  PermissionScope::Instance,
];

/// The permission scheme: what each role may do on the instance and, with changes, in a space.
/// A channel can only take permissions away again (see the channel details).
#[component]
pub fn PermissionsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let scope = RwSignal::new(None::<SpaceId>);
  let spaces = Memo::new(move |_| state.read(|backend| backend.admin_spaces()));
  let scheme = Memo::new(move |_| state.read(|backend| backend.permission_scheme(scope.get())));

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-permissions-title"))
      subtitle=Signal::derive(move || i18n.t("admin-permissions-subtitle"))
    />
    <div class="admin-toolbar">
      <label for="permission-scope">{move || i18n.t("permissions-scope")}</label>
      <select
        id="permission-scope"
        class="input"
        on:change=move |ev| {
          let value = event_target_value(&ev);
          scope.set(spaces.get().into_iter().find(|space| space.id.to_string() == value).map(|space| space.id));
        }
      >
        <option value="">{move || i18n.t("permissions-instance")}</option>
        <For each=move || spaces.get() key=|space: &AdminSpace| space.id let:space>
          <option value=space.id.to_string() translate="no">{space.name.clone()}</option>
        </For>
      </select>
      <span class="muted-text small">
        {move || if scope.get().is_some() { i18n.t("permissions-space-hint") } else { i18n.t("permissions-instance-hint") }}
      </span>
    </div>
    <p class="muted-text small">{move || i18n.t("permissions-legend")}</p>
    <div class="table-wrap">
      <table class="data-table permission-matrix">
        <caption class="visually-hidden">{move || i18n.t("admin-permissions-title")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("permissions-permission")}</th>
            {ROLES
              .iter()
              .map(|role| {
                let key = format!("permissions-role-{}", role.as_str());
                view! { <th scope="col" class="num">{move || i18n.t(&key)}</th> }
              })
              .collect_view()}
          </tr>
        </thead>
        <tbody>
          {GROUPS
            .into_iter()
            .map(|group| {
              let heading = format!("permissions-group-{}", group_slug(group));
              view! {
                <tr class="group-row">
                  <th scope="colgroup" colspan="7">{move || i18n.t(&heading)}</th>
                </tr>
                {ALL_PERMISSIONS
                  .iter()
                  .filter(|permission| permission.scope() == Some(group))
                  .cloned()
                  .map(|permission| view! { <PermissionRow permission=permission scope=scope scheme=scheme /> })
                  .collect_view()}
              }
            })
            .collect_view()}
        </tbody>
      </table>
    </div>
  }
}

#[component]
fn PermissionRow(
  permission: Permission,
  scope: RwSignal<Option<SpaceId>>,
  scheme: Memo<teamcraft_proto::PermissionSchemeResponse>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let label_key = format!("permission-{}", permission.as_str());
  let hint_key = format!("permission-{}-hint", permission.as_str());
  let permission_name = permission.clone();

  let cells = ROLES
    .iter()
    .cloned()
    .map(|role| {
      let permission = permission.clone();
      let cell = {
        let (role, permission) = (role.clone(), permission.clone());
        Memo::new(move |_| cell_state(&scheme.get(), &role, &permission))
      };
      let label = format!("{} / {}", permission.as_str(), role.as_str());
      let fixed_role = role.clone();
      let fixed_permission = permission.clone();
      view! {
        <td class="num" class:overridden=move || cell.get().override_effect.is_some()>
          <input
            type="checkbox"
            aria-label=label
            prop:checked=move || cell.get().is_granted
            prop:disabled=move || is_fixed(&fixed_role, &fixed_permission, scope.get().is_some())
            on:change=move |_| {
              let change = SchemeChange {
                role: role.clone(),
                permission: permission.clone(),
                effect: next_effect(&cell.get_untracked()),
              };
              state.write(|backend| backend.change_permissions(scope.get_untracked(), vec![change]));
            }
          />
          {move || {
            cell.get().override_effect.map(|effect| {
              let key = if effect == OverrideEffect::Grant { "permissions-granted" } else { "permissions-revoked" };
              view! { <span class="badge badge-small">{move || i18n.t(key)}</span> }
            })
          }}
        </td>
      }
    })
    .collect_view();

  view! {
    <tr>
      <th scope="row">
        <span>{move || i18n.t(&label_key)}</span>
        <span class="muted-text small hint-line">{move || i18n.t(&hint_key)}</span>
        <span class="visually-hidden">{permission_name.as_str().to_owned()}</span>
      </th>
      {cells}
    </tr>
  }
}

/// A matrix cell: whether the role has the permission here and the change made at this scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellState {
  pub is_granted: bool,
  pub override_effect: Option<OverrideEffect>,
}

pub fn cell_state(
  scheme: &teamcraft_proto::PermissionSchemeResponse,
  role: &SchemeRole,
  permission: &Permission,
) -> CellState {
  let Some(entry): Option<&SchemeRoleResponse> = scheme.roles.iter().find(|entry| entry.role == *role) else {
    return CellState {
      is_granted: false,
      override_effect: None,
    };
  };
  CellState {
    is_granted: entry.granted.contains(permission),
    override_effect: entry
      .overrides
      .iter()
      .find(|change| change.permission == *permission)
      .map(|change| change.effect.clone()),
  }
}

/// What a click on the cell does: a change at this scope is dropped again, otherwise the opposite
/// of what is in force is set.
pub fn next_effect(current: &CellState) -> OverrideEffect {
  match (&current.override_effect, current.is_granted) {
    (Some(_), _) => OverrideEffect::Inherit,
    (None, true) => OverrideEffect::Revoke,
    (None, false) => OverrideEffect::Grant,
  }
}

/// Whether the cell can never change (owner, signing, instance-only permissions in a space).
fn is_fixed(role: &SchemeRole, permission: &Permission, is_in_space: bool) -> bool {
  match check_scheme_change(role, permission, false, is_in_space) {
    Ok(()) => false,
    // Guests cannot get more than they were designed for, but may lose permissions.
    Err(teamcraft_proto::SchemeChangeError::NotForGuests) => false,
    Err(_) => true,
  }
}

fn group_slug(group: PermissionScope) -> &'static str {
  match group {
    PermissionScope::ChannelContent => "content",
    PermissionScope::ChannelManagement => "channel",
    PermissionScope::Space => "space",
    PermissionScope::Instance => "instance",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{PermissionSchemeResponse, SchemeOverride};

  fn scheme() -> PermissionSchemeResponse {
    PermissionSchemeResponse {
      roles: vec![SchemeRoleResponse {
        role: SchemeRole::Member,
        granted: vec![Permission::CreatePost],
        overrides: vec![SchemeOverride {
          permission: Permission::CreatePost,
          effect: OverrideEffect::Grant,
        }],
      }],
    }
  }

  #[test]
  fn a_cell_shows_the_grant_and_its_override() {
    let cell = cell_state(&scheme(), &SchemeRole::Member, &Permission::CreatePost);
    assert!(cell.is_granted);
    assert_eq!(cell.override_effect, Some(OverrideEffect::Grant));
    let missing = cell_state(&scheme(), &SchemeRole::Guest, &Permission::CreatePost);
    assert!(!missing.is_granted && missing.override_effect.is_none());
  }

  #[test]
  fn clicking_toggles_or_drops_the_change() {
    let granted = CellState {
      is_granted: true,
      override_effect: None,
    };
    let denied = CellState {
      is_granted: false,
      override_effect: None,
    };
    let changed = CellState {
      is_granted: true,
      override_effect: Some(OverrideEffect::Grant),
    };
    assert_eq!(next_effect(&granted), OverrideEffect::Revoke);
    assert_eq!(next_effect(&denied), OverrideEffect::Grant);
    assert_eq!(next_effect(&changed), OverrideEffect::Inherit);
  }

  #[test]
  fn the_owner_and_signing_are_fixed_and_guests_can_still_lose_things() {
    assert!(is_fixed(&SchemeRole::Owner, &Permission::CreatePost, false));
    assert!(is_fixed(&SchemeRole::Admin, &Permission::SignWorkspace, false));
    assert!(is_fixed(&SchemeRole::Admin, &Permission::CreatePost, true));
    assert!(!is_fixed(&SchemeRole::Guest, &Permission::CreateChannel, false));
    assert!(!is_fixed(&SchemeRole::Member, &Permission::CreatePost, false));
  }
}
