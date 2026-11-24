//! Roles and permissions (issue #2360): what each role may do on the instance and, with changes,
//! in a space. Clicking a cell only collects the change; the list below the matrix says what each
//! change does, in words, before "Apply changes" sends them together.

use leptos::prelude::*;
use teamcraft_proto::{
  ALL_PERMISSIONS, OverrideEffect, Permission, PermissionSchemeResponse, PermissionScope, SchemeChange,
  SchemeRole, SchemeRoleResponse, check_scheme_change,
};

use crate::admin::AdminSpace;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_save::{SaveBar, use_save_tracker};
use crate::i18n::{I18n, use_i18n};
use crate::model::SpaceId;
use crate::motion::prefers_reduced_motion;
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

const FORM_ID: &str = "permission-scheme-form";

/// The permission scheme: what each role may do on the instance and, with changes, in a space.
/// A channel can only take permissions away again (see the channel details).
#[component]
pub fn PermissionsSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let scope = RwSignal::new(None::<SpaceId>);
  let spaces = Memo::new(move |_| state.read(|backend| backend.admin_spaces()));
  let scheme = Memo::new(move |_| state.read(|backend| backend.permission_scheme(scope.get())));
  // The changes clicked but not applied yet, in the order they were made.
  let pending = RwSignal::new(Vec::<SchemeChange>::new());
  let tracker = use_save_tracker();
  // Once the scheme shows an applied change, it is no longer pending.
  Effect::new(move |_| {
    let current = scheme.get();
    let before = pending.with_untracked(Vec::len);
    if before == 0 {
      return;
    }
    pending.update(|changes| changes.retain(|change| !is_reflected(change, &current)));
    if pending.with_untracked(Vec::is_empty) {
      tracker.settled();
    }
  });
  let is_dirty = Signal::derive(move || pending.with(|changes| !changes.is_empty()));
  let changes_list = NodeRef::<leptos::html::Section>::new();
  // The list is below a long table: the save bar's button brings it into view.
  let review = move |_| {
    if let Some(section) = changes_list.get_untracked() {
      let options = web_sys::ScrollIntoViewOptions::new();
      options.set_block(web_sys::ScrollLogicalPosition::Center);
      if !prefers_reduced_motion() {
        options.set_behavior(web_sys::ScrollBehavior::Smooth);
      }
      section.scroll_into_view_with_scroll_into_view_options(&options);
      let focus = web_sys::FocusOptions::new();
      focus.set_prevent_scroll(true);
      // Ignored on failure: the list is in view either way.
      drop(section.focus_with_options(&focus));
    }
  };
  let space_name = move || {
    scope.get().and_then(|id| {
      spaces.with(|spaces| {
        spaces
          .iter()
          .find(|space| space.id == id)
          .map(|space| space.name.clone())
      })
    })
  };
  let apply = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let changes = pending.get_untracked();
    if changes.is_empty() {
      return;
    }
    state.write(|backend| backend.change_permissions(scope.get_untracked(), changes));
    tracker.begin();
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-permissions-title"))
      subtitle=Signal::derive(move || i18n.t("admin-permissions-subtitle"))
    />
    <div class="admin-toolbar">
      <label for="permission-scope">{move || i18n.t("permissions-scope")}</label>
      // Switching the scope with changes collected would apply them to the wrong place.
      <select
        id="permission-scope"
        class="input"
        aria-describedby="permission-scope-hint"
        prop:disabled=move || is_dirty.get()
        on:change=move |ev| {
          let value = event_target_value(&ev);
          scope.set(spaces.get().into_iter().find(|space| space.id.to_string() == value).map(|space| space.id));
          tracker.clear();
        }
      >
        <option value="">{move || i18n.t("permissions-instance")}</option>
        <For each=move || spaces.get() key=|space: &AdminSpace| space.id let:space>
          <option value=space.id.to_string() translate="no">{space.name.clone()}</option>
        </For>
      </select>
      <span id="permission-scope-hint" class="muted-text small">
        {move || {
          if is_dirty.get() {
            i18n.t("permissions-scope-locked")
          } else if scope.get().is_some() {
            i18n.t("permissions-space-hint")
          } else {
            i18n.t("permissions-instance-hint")
          }
        }}
      </span>
    </div>
    <p class="muted-text small">{move || i18n.t("permissions-legend")}</p>
    <form id=FORM_ID on:submit=apply>
      <div class="table-wrap">
        <table class="data-table permission-matrix">
          <caption class="visually-hidden">{move || i18n.t("admin-permissions-title")}</caption>
          <thead>
            <tr>
              <th scope="col">{move || i18n.t("permissions-permission")}</th>
              {ROLES
                .iter()
                .map(|role| {
                  let key = role_key(role);
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
                    .map(|permission| view! { <PermissionRow permission=permission scope=scope scheme=scheme pending=pending /> })
                    .collect_view()}
                }
              })
              .collect_view()}
          </tbody>
        </table>
      </div>
      <Show when=move || is_dirty.get() fallback=|| ()>
        <section
          class="permission-changes"
          node_ref=changes_list
          tabindex="-1"
          aria-labelledby="permission-changes-title"
        >
          <h3 id="permission-changes-title" class="permission-changes-title">
            {move || match space_name() {
              Some(space) => i18n.t_with("permissions-changes-space", &[("space", &space)]),
              None => i18n.t("permissions-changes-instance"),
            }}
          </h3>
          <ul class="permission-changes-list">
            {move || {
              let is_in_space = scope.get().is_some();
              pending
                .get()
                .into_iter()
                .map(|change| view! { <li>{change_sentence(i18n, &change, is_in_space)}</li> })
                .collect_view()
            }}
          </ul>
        </section>
      </Show>
      <SaveBar
        tracker=tracker
        is_dirty=is_dirty
        on_discard=Callback::new(move |()| pending.set(Vec::new()))
        save_label=Signal::derive(move || i18n.t("permissions-apply"))
      >
        <Show when=move || is_dirty.get() fallback=|| ()>
          <button type="button" class="btn" on:click=review>
            {move || i18n.t_count("permissions-review", pending.with(Vec::len))}
          </button>
        </Show>
      </SaveBar>
    </form>
  }
}

#[component]
fn PermissionRow(
  permission: Permission,
  scope: RwSignal<Option<SpaceId>>,
  scheme: Memo<PermissionSchemeResponse>,
  pending: RwSignal<Vec<SchemeChange>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let label_key = permission_key(&permission);
  let hint_key = format!("{label_key}-hint");

  let cells = ROLES
    .iter()
    .cloned()
    .map(|role| {
      let cell = {
        let (role, permission) = (role.clone(), permission.clone());
        Memo::new(move |_| {
          let stored = cell_state(&scheme.get(), &role, &permission);
          let maybe_pending = pending.with(|changes| pending_effect(changes, &role, &permission));
          shown_cell(&stored, maybe_pending.as_ref())
        })
      };
      let label = {
        let (role, permission) = (role.clone(), permission.clone());
        move || {
          i18n.t_with(
            "permissions-cell",
            &[
              ("permission", &i18n.t(&permission_key(&permission))),
              ("role", &i18n.t(&role_key(&role))),
            ],
          )
        }
      };
      let fixed_role = role.clone();
      let fixed_permission = permission.clone();
      let permission = permission.clone();
      view! {
        <td
          class="num"
          class:overridden=move || cell.get().maybe_marker.is_some()
          class:is-pending=move || cell.get().is_pending
        >
          <input
            type="checkbox"
            aria-label=label
            prop:checked=move || cell.get().maybe_granted == Some(true)
            prop:indeterminate=move || cell.get().maybe_granted.is_none()
            prop:disabled=move || is_fixed(&fixed_role, &fixed_permission, scope.get().is_some())
            on:change=move |_| {
              let stored = cell_state(&scheme.get_untracked(), &role, &permission);
              pending.update(|changes| toggle(changes, &role, &permission, &stored));
            }
          />
          {move || {
            cell.get().maybe_marker.map(|marker| {
              let key = match marker {
                OverrideEffect::Grant => "permissions-granted",
                OverrideEffect::Revoke => "permissions-revoked",
                _ => "permissions-default",
              };
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
      </th>
      {cells}
    </tr>
  }
}

/// A matrix cell: whether the role has the permission here and the change made at this scope.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CellState {
  is_granted: bool,
  override_effect: Option<OverrideEffect>,
}

fn cell_state(scheme: &PermissionSchemeResponse, role: &SchemeRole, permission: &Permission) -> CellState {
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
fn next_effect(current: &CellState) -> OverrideEffect {
  match (&current.override_effect, current.is_granted) {
    (Some(_), _) => OverrideEffect::Inherit,
    (None, true) => OverrideEffect::Revoke,
    (None, false) => OverrideEffect::Grant,
  }
}

/// A cell as the matrix shows it, with the change collected for it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ShownCell {
  /// `None` while a change sends the cell back to what the scope above says, which only the server
  /// knows: the checkbox is then neither checked nor clear.
  maybe_granted: Option<bool>,
  /// The badge: a change stored at this scope, or the one collected.
  maybe_marker: Option<OverrideEffect>,
  is_pending: bool,
}

fn shown_cell(stored: &CellState, maybe_pending: Option<&OverrideEffect>) -> ShownCell {
  match maybe_pending {
    None => ShownCell {
      maybe_granted: Some(stored.is_granted),
      maybe_marker: stored.override_effect.clone(),
      is_pending: false,
    },
    Some(effect) => ShownCell {
      maybe_granted: match effect {
        OverrideEffect::Grant => Some(true),
        OverrideEffect::Revoke => Some(false),
        _ => None,
      },
      maybe_marker: Some(effect.clone()),
      is_pending: true,
    },
  }
}

fn pending_effect(
  changes: &[SchemeChange],
  role: &SchemeRole,
  permission: &Permission,
) -> Option<OverrideEffect> {
  changes
    .iter()
    .find(|change| change.role == *role && change.permission == *permission)
    .map(|change| change.effect.clone())
}

/// A click on a cell: a change collected for it is taken back, otherwise the cell's next change is
/// collected.
fn toggle(changes: &mut Vec<SchemeChange>, role: &SchemeRole, permission: &Permission, stored: &CellState) {
  let before = changes.len();
  changes.retain(|change| !(change.role == *role && change.permission == *permission));
  if changes.len() == before {
    changes.push(SchemeChange {
      role: role.clone(),
      permission: permission.clone(),
      effect: next_effect(stored),
    });
  }
}

/// Whether the scheme already shows the change (it was applied).
fn is_reflected(change: &SchemeChange, scheme: &PermissionSchemeResponse) -> bool {
  let stored = cell_state(scheme, &change.role, &change.permission).override_effect;
  match change.effect {
    OverrideEffect::Grant | OverrideEffect::Revoke => stored.as_ref() == Some(&change.effect),
    _ => stored.is_none(),
  }
}

/// What a change does, in a sentence: "Members can no longer: Delete channels".
fn change_sentence(i18n: I18n, change: &SchemeChange, is_in_space: bool) -> String {
  let key = match (&change.effect, is_in_space) {
    (OverrideEffect::Grant, _) => "permissions-change-grant",
    (OverrideEffect::Revoke, _) => "permissions-change-revoke",
    (_, true) => "permissions-change-inherit-space",
    (_, false) => "permissions-change-inherit-instance",
  };
  i18n.t_with(
    key,
    &[
      ("role", &i18n.t(&role_plural_key(&change.role))),
      ("permission", &i18n.t(&permission_key(&change.permission))),
    ],
  )
}

fn permission_key(permission: &Permission) -> String {
  format!("permission-{}", permission.as_str())
}

fn role_key(role: &SchemeRole) -> String {
  format!("permissions-role-{}", role.as_str())
}

fn role_plural_key(role: &SchemeRole) -> String {
  format!("permissions-roles-{}", role.as_str())
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
  use teamcraft_proto::SchemeOverride;

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
  fn a_click_collects_a_change_and_a_second_click_takes_it_back() {
    let stored = cell_state(&scheme(), &SchemeRole::Member, &Permission::CreatePost);
    let mut changes = Vec::new();
    toggle(
      &mut changes,
      &SchemeRole::Member,
      &Permission::CreatePost,
      &stored,
    );
    assert_eq!(
      pending_effect(&changes, &SchemeRole::Member, &Permission::CreatePost),
      Some(OverrideEffect::Inherit)
    );
    toggle(
      &mut changes,
      &SchemeRole::Guest,
      &Permission::CreatePost,
      &CellState {
        is_granted: false,
        override_effect: None,
      },
    );
    assert_eq!(changes.len(), 2);
    toggle(
      &mut changes,
      &SchemeRole::Member,
      &Permission::CreatePost,
      &stored,
    );
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].role, SchemeRole::Guest);
  }

  #[test]
  fn a_collected_change_shows_in_the_cell_before_it_is_applied() {
    let stored = CellState {
      is_granted: true,
      override_effect: None,
    };
    let untouched = shown_cell(&stored, None);
    assert_eq!(untouched.maybe_granted, Some(true));
    assert!(!untouched.is_pending && untouched.maybe_marker.is_none());
    let revoked = shown_cell(&stored, Some(&OverrideEffect::Revoke));
    assert_eq!(revoked.maybe_granted, Some(false));
    assert!(revoked.is_pending);
    // Back to the scope above: only the server knows what that grants.
    let inherited = shown_cell(&stored, Some(&OverrideEffect::Inherit));
    assert_eq!(inherited.maybe_granted, None);
  }

  #[test]
  fn a_change_is_done_once_the_scheme_shows_it() {
    let granted = SchemeChange {
      role: SchemeRole::Member,
      permission: Permission::CreatePost,
      effect: OverrideEffect::Grant,
    };
    assert!(is_reflected(&granted, &scheme()));
    let dropped = SchemeChange {
      effect: OverrideEffect::Inherit,
      ..granted.clone()
    };
    assert!(!is_reflected(&dropped, &scheme()));
    let revoked_elsewhere = SchemeChange {
      role: SchemeRole::Guest,
      effect: OverrideEffect::Revoke,
      ..granted
    };
    assert!(!is_reflected(&revoked_elsewhere, &scheme()));
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
