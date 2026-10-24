//! The owners of the signed admin roster in the security section: who they are, when each last
//! signed, and warnings before the workspace could end up without anyone who can administer it
//! (admin.md §2.3, issue #188).

use leptos::prelude::*;

use crate::admin::{
  OwnerRosterStatus, OwnerWarning, RosterChange, RosterEntry, RosterRoleChoice, normalized_roster,
  roster_changes, roster_from_roles, roster_problem, signed_roster,
};
use crate::clock::{format_date_time, now_ms};
use crate::components::admin_policy::{PolicyDraft, SelectRow};
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::state::use_app_state;

/// Choices for the recommended number of owners; the policy allows up to 10.
const RECOMMENDED_OWNERS: [u8; 4] = [1, 2, 3, 4];

/// Owners, their last signatures and the warnings, plus the recommended minimum (signed policy).
#[component]
pub fn OwnersPanel(policy: PolicyDraft) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let draft = policy.draft;
  let roster = Memo::new(move |_| state.read(|backend| backend.admin_owner_roster()));
  let minimum_options = Signal::derive(move || {
    RECOMMENDED_OWNERS
      .into_iter()
      .map(|count| {
        (
          count.to_string(),
          i18n.t_count("policy-owners-count", usize::from(count)),
        )
      })
      .collect()
  });

  view! {
    <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
      <legend>{move || i18n.t("policy-owners")}</legend>
      <p class="muted-text small">{move || i18n.t("policy-owners-hint")}</p>
      {move || match roster.get() {
        Some(status) => view! {
          <OwnerList status=status.clone() />
          <RosterEditor status=status />
        }
        .into_any(),
        None => view! { <p class="muted-text small">{i18n.t("policy-owners-unknown")}</p> }.into_any(),
      }}
      <SelectRow
        id="policy-owners-minimum"
        label=Signal::derive(move || i18n.t("policy-owners-minimum"))
        options=minimum_options
        selected=Signal::derive(move || draft.get().owners.recommended_minimum.to_string())
        on_select=move |value: String| {
          let count = value.parse::<u8>().unwrap_or(teamcraft_proto::DEFAULT_RECOMMENDED_OWNERS);
          policy.edit(|p| p.owners.recommended_minimum = count);
        }
      />
      <p class="muted-text small">{move || i18n.t("policy-owners-minimum-hint")}</p>
    </fieldset>
  }
}

/// The warnings first, then each owner with their last signature and the roster's expiry.
#[component]
fn OwnerList(status: OwnerRosterStatus) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let now = now_ms();
  let locale = i18n.locale().tag();
  let warnings = status
    .warnings(now)
    .into_iter()
    .map(|warning| view! { <p class="auth-alert" role="status">{warning_text(i18n, warning)}</p> })
    .collect_view();
  let owners = status
    .owners
    .iter()
    .map(|owner| {
      let name = state
        .read(|backend| backend.user(owner.user))
        .map(|user| user.display_name)
        .unwrap_or_else(|| owner.user.to_string());
      let signed = match owner.maybe_last_signed_at {
        Some(at) => i18n.t_with(
          "policy-owners-last-signed",
          &[("time", &format_date_time(at, locale))],
        ),
        None => i18n.t("policy-owners-never-signed"),
      };
      view! {
        <li>
          <strong translate="no">{name}</strong>
          " "
          <span class="muted-text small">{signed}</span>
        </li>
      }
    })
    .collect_view();
  let valid_until = status.maybe_expires_at.filter(|at| *at > now).map(|at| {
    let text = i18n.t_with(
      "policy-owners-valid-until",
      &[("time", &format_date_time(at, locale))],
    );
    view! { <p class="muted-text small">{text}</p> }
  });

  view! {
    {warnings}
    <ul class="owner-list">{owners}</ul>
    {valid_until}
  }
}

/// Proposes a new roster and signs it on this device (admin.md §2.3.1): change roles, remove and add
/// people, or take over the identity provider's role holders. Read-only for anyone but an owner
/// with signing keys (the surrounding fieldset); the core refuses a roster without an owner.
#[component]
fn RosterEditor(status: OwnerRosterStatus) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let signed = StoredValue::new(signed_roster(&status));
  let base_version = status.version;
  let draft = RwSignal::new(signed.get_value());
  // The roster waiting for the owner's confirmation (every change with ID and key, review #263).
  let confirming = RwSignal::new(None::<Vec<RosterEntry>>);
  let picked = RwSignal::new(String::new());
  let (idp_owners, idp_admins) = state.read(|backend| backend.admin_role_holders());
  let from_roles = roster_from_roles(&idp_owners, &idp_admins);
  let idp_changes = if from_roles.is_empty() {
    Vec::new()
  } else {
    roster_changes(&signed.get_value(), &from_roles)
  };
  let name_of = move |user: UserId| {
    state
      .read(|backend| backend.user(user))
      .map(|user| user.display_name)
      .unwrap_or_else(|| user.to_string())
  };
  let changes = Memo::new(move |_| roster_changes(&signed.get_value(), &draft.get()));
  let problem = Memo::new(move |_| roster_problem(&draft.get()));
  let addable = Memo::new(move |_| {
    let current = draft.get();
    let mut users: Vec<(String, String)> = state
      .read(|backend| backend.users())
      .into_iter()
      .filter(|user| !current.iter().any(|entry| entry.user == user.id))
      .map(|user| {
        let label = format!("{} ({})", user.display_name, user.id);
        (user.id.to_string(), label)
      })
      .collect();
    users.sort_by(|a, b| a.1.cmp(&b.1));
    users
  });
  let set_role = move |user: UserId, maybe_role: Option<RosterRoleChoice>| {
    draft.update(|entries| {
      entries.retain(|entry| entry.user != user);
      if let Some(role) = maybe_role {
        entries.push(RosterEntry { user, role });
      }
      *entries = normalized_roster(entries);
    });
  };
  let add = move |role: RosterRoleChoice| {
    if let Ok(user) = picked.get_untracked().parse::<UserId>() {
      set_role(user, Some(role));
      picked.set(String::new());
    }
  };
  // Nothing is signed straight away: names come from the server, so the owner checks each change
  // with its user ID and pinned key first.
  let review = move |members: Vec<RosterEntry>| {
    if roster_problem(&members).is_none() {
      confirming.set(Some(members));
    }
  };
  let confirm = move || {
    if let Some(members) = confirming.get_untracked()
      && roster_problem(&members).is_none()
    {
      state.write(|backend| backend.admin_publish_roster(base_version, members));
      confirming.set(None);
    }
  };

  let idp_block = (!idp_changes.is_empty()).then(|| {
    let lines = idp_changes
      .iter()
      .map(|change| view! { <li>{change_text(i18n, *change, &name_of)}</li> })
      .collect_view();
    let target = from_roles.clone();
    let has_problem = roster_problem(&from_roles).is_some();
    view! {
      <div class="roster-idp">
        <p class="auth-alert" role="status">{i18n.t("roster-idp-title")}</p>
        <ul>{lines}</ul>
        <button type="button" class="btn" disabled=has_problem on:click=move |_| review(target.clone())>
          {i18n.t("roster-idp-sign")}
        </button>
      </div>
    }
  });

  view! {
    <div class="roster-editor">
      <h4 class="detail-heading">{move || i18n.t("roster-title")}</h4>
      <ul class="owner-list">
        <For each=move || draft.get() key=|entry| (entry.user, entry.role) let:entry>
          {
            let user = entry.user;
            let (role_key, toggle_key, toggled) = match entry.role {
              RosterRoleChoice::Owner => ("roster-role-owner", "roster-make-admin", RosterRoleChoice::Admin),
              RosterRoleChoice::Admin => ("roster-role-admin", "roster-make-owner", RosterRoleChoice::Owner),
            };
            view! {
              <li>
                <strong translate="no">{name_of(user)}</strong>
                " "
                <span class="muted-text small">{i18n.t(role_key)}</span>
                " "
                <button type="button" class="btn btn-small" on:click=move |_| set_role(user, Some(toggled))>
                  {i18n.t(toggle_key)}
                </button>
                <button type="button" class="btn btn-small" on:click=move |_| set_role(user, None)>
                  {i18n.t("roster-remove")}
                </button>
              </li>
            }
          }
        </For>
      </ul>
      <div class="field inline">
        <label for="roster-add">{move || i18n.t("roster-add-label")}</label>
        <select
          id="roster-add"
          class="input"
          prop:value=move || picked.get()
          on:change=move |ev| picked.set(event_target_value(&ev))
        >
          <option value="">{move || i18n.t("roster-add-choose")}</option>
          {move || {
            addable
              .get()
              .into_iter()
              .map(|(value, name)| view! { <option value=value>{name}</option> })
              .collect_view()
          }}
        </select>
        <button type="button" class="btn btn-small" disabled=move || picked.get().is_empty() on:click=move |_| add(RosterRoleChoice::Owner)>
          {move || i18n.t("roster-add-owner")}
        </button>
        <button type="button" class="btn btn-small" disabled=move || picked.get().is_empty() on:click=move |_| add(RosterRoleChoice::Admin)>
          {move || i18n.t("roster-add-admin")}
        </button>
      </div>
      {move || {
        let pending = changes.get();
        (!pending.is_empty()).then(|| {
          let lines = pending
            .into_iter()
            .map(|change| view! { <li>{change_text(i18n, change, &name_of)}</li> })
            .collect_view();
          view! {
            <h4 class="detail-heading">{i18n.t("roster-pending")}</h4>
            <ul>{lines}</ul>
          }
        })
      }}
      {move || problem.get().map(|_| view! { <p class="auth-alert" role="alert">{i18n.t("roster-no-owner")}</p> })}
      <p class="muted-text small">{move || i18n.t("roster-sign-hint")}</p>
      <button type="button" class="btn" disabled=move || changes.get().is_empty() on:click=move |_| draft.set(signed.get_value())>
        {move || i18n.t("roster-reset")}
      </button>
      <button
        type="button"
        class="btn btn-primary"
        disabled=move || changes.get().is_empty() || problem.get().is_some()
        on:click=move |_| review(draft.get_untracked())
      >
        {move || i18n.t("roster-review")}
      </button>
      {idp_block}
      {move || {
        confirming.get().map(|members| {
          view! {
            <RosterConfirm
              signed=signed.get_value()
              members=members
              on_cancel=Callback::new(move |()| confirming.set(None))
              on_confirm=Callback::new(move |()| confirm())
              maybe_on_sign_alone=None
            />
          }
        })
      }}
    </div>
  }
}

/// The confirmation before an owner signs a roster: every change with the user ID and the pinned
/// key's fingerprint, attested, trusted on first use or the owner's own (review #263). The first
/// roster goes through it too, with `signed` empty (#265); `maybe_on_sign_alone` then offers to
/// sign the signer alone and add the others later.
#[component]
pub(crate) fn RosterConfirm(
  signed: Vec<RosterEntry>,
  members: Vec<RosterEntry>,
  on_cancel: Callback<()>,
  on_confirm: Callback<()>,
  maybe_on_sign_alone: Option<Callback<()>>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name_of = move |user: UserId| {
    state
      .read(|backend| backend.user(user))
      .map(|user| user.display_name)
      .unwrap_or_else(|| user.to_string())
  };
  let lines = roster_changes(&signed, &members)
    .into_iter()
    .map(|change| {
      let user = change_user(change);
      let key = key_line(i18n, user);
      view! {
        <li class=if key.1 { "roster-confirm-item warning" } else { "roster-confirm-item" }>
          <strong>{change_text(i18n, change, &name_of)}</strong>
          <br />
          <code translate="no">{i18n.t_with("roster-confirm-id", &[("id", &user.to_string())])}</code>
          <br />
          <span class="small">{key.0}</span>
        </li>
      }
    })
    .collect_view();
  let is_genesis = signed.is_empty();
  let (title_key, hint_key) = if is_genesis {
    ("roster-genesis-title", "roster-genesis-hint")
  } else {
    ("roster-confirm-title", "roster-confirm-hint")
  };
  let sign_alone = maybe_on_sign_alone.map(|on_sign_alone| {
    view! {
      <button type="button" class="btn" on:click=move |_| on_sign_alone.run(())>
        {i18n.t("roster-genesis-sign-alone")}
      </button>
    }
  });

  view! {
    <div class="roster-confirm" role="dialog" aria-label=i18n.t(title_key)>
      <h4 class="detail-heading">{i18n.t(title_key)}</h4>
      <p class="auth-alert" role="status">{i18n.t(hint_key)}</p>
      <ul>{lines}</ul>
      <button type="button" class="btn" on:click=move |_| on_cancel.run(())>
        {i18n.t("roster-confirm-cancel")}
      </button>
      {sign_alone}
      <button type="button" class="btn btn-primary" on:click=move |_| on_confirm.run(())>
        {i18n.t("roster-confirm-sign")}
      </button>
    </div>
  }
}

fn change_user(change: RosterChange) -> UserId {
  match change {
    RosterChange::Add { user, .. }
    | RosterChange::ChangeRole { user, .. }
    | RosterChange::Remove { user } => user,
  }
}

/// The pinned key of `user` as the owner should compare it, and whether it deserves a warning
/// (not attested, not pinned yet, or a changed key the user hasn't decided on).
fn key_line(i18n: I18n, user: UserId) -> (String, bool) {
  let state = use_app_state();
  let (me, keys) = state.read(|backend| (backend.me().id, backend.admin_identity_keys()));
  if user == me {
    let text = format!(
      "{} · {}",
      i18n.t_with(
        "roster-confirm-fingerprint",
        &[("fingerprint", &keys.my_fingerprint)]
      ),
      i18n.t("roster-confirm-own")
    );
    return (text, false);
  }
  match keys.contacts.iter().find(|contact| contact.user == user) {
    None => (i18n.t("roster-confirm-new-key"), true),
    Some(contact) => {
      let fingerprint = i18n.t_with(
        "roster-confirm-fingerprint",
        &[("fingerprint", &contact.fingerprint)],
      );
      let (note, is_warning) = if contact.maybe_new_fingerprint.is_some() {
        (i18n.t("roster-confirm-changed-key"), true)
      } else if contact.is_attested {
        (i18n.t("roster-confirm-attested"), false)
      } else {
        (i18n.t("roster-confirm-unattested"), true)
      };
      (format!("{fingerprint} · {note}"), is_warning)
    },
  }
}

fn change_text(i18n: I18n, change: RosterChange, name_of: &impl Fn(UserId) -> String) -> String {
  let (key, user) = match change {
    RosterChange::Add {
      user,
      role: RosterRoleChoice::Owner,
    } => ("roster-change-add-owner", user),
    RosterChange::Add {
      user,
      role: RosterRoleChoice::Admin,
    } => ("roster-change-add-admin", user),
    RosterChange::ChangeRole {
      user,
      role: RosterRoleChoice::Owner,
    } => ("roster-change-make-owner", user),
    RosterChange::ChangeRole {
      user,
      role: RosterRoleChoice::Admin,
    } => ("roster-change-make-admin", user),
    RosterChange::Remove { user } => ("roster-change-remove", user),
  };
  i18n.t_with(key, &[("name", &name_of(user))])
}

fn warning_text(i18n: I18n, warning: OwnerWarning) -> String {
  match warning {
    OwnerWarning::Expired => i18n.t("policy-owners-expired"),
    OwnerWarning::ExpiresSoon { at } => i18n.t_with(
      "policy-owners-expires-soon",
      &[("time", &format_date_time(at, i18n.locale().tag()))],
    ),
    OwnerWarning::TooFewOwners { count, minimum } => i18n.t_with(
      "policy-owners-too-few",
      &[("count", &count.to_string()), ("minimum", &minimum.to_string())],
    ),
  }
}
