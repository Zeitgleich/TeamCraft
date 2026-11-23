//! What needs an admin's attention outside the health checks (issue #958), at the top of the
//! console's first page: the owners and their signatures, roles that can't follow the identity
//! provider by themselves, and requests waiting for this person. Each item says where it is fixed and
//! goes there. Read from what the console holds already; it asks the server for nothing.

use leptos::prelude::*;

use crate::admin::{AdminSection, OwnerRosterStatus, OwnerWarning, RoleSigningIssue};
use crate::clock::now_ms;
use crate::components::admin_owners::warning_text;
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::TimestampMs;
use crate::state::{AppState, use_app_state};

/// The card; nothing while there is nothing to do, so a healthy instance shows the checks only.
#[component]
pub fn AttentionCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let items = Memo::new(move |_| {
    let maybe_roster = state.read(|backend| backend.admin_owner_roster());
    let join_requests = state.read(|backend| backend.join_requests().len());
    attention_items(maybe_roster.as_ref(), join_requests, now_ms())
  });
  move || {
    let list = items.get();
    (!list.is_empty()).then(|| {
      view! {
        <section class="instance-card attention-card" aria-labelledby="admin-attention">
          <h3 id="admin-attention"><Icon name=IconName::Alert /> {move || i18n.t("admin-attention-title")}</h3>
          <ul class="attention-list">
            {list.into_iter().map(|item| view! { <AttentionRow item=item state=state i18n=i18n /> }).collect_view()}
          </ul>
        </section>
      }
    })
  }
}

#[component]
fn AttentionRow(item: Attention, state: AppState, i18n: I18n) -> impl IntoView {
  let text = item_text(i18n, &item);
  let (button_key, target) = match item.target() {
    Target::Admin(section) => ("admin-attention-open", Some(section)),
    Target::Activity => ("admin-attention-open-activity", None),
  };
  let page = target
    .map(|section| i18n.t(&format!("admin-nav-{}", section.slug())))
    .unwrap_or_default();
  view! {
    <li class="attention-item">
      <p>{text}</p>
      <button
        type="button"
        class="btn btn-small"
        on:click=move |_| match target {
          Some(section) => state.open_admin(section),
          None => state.open_activity(),
        }
      >
        {move || i18n.t_with(button_key, &[("page", &page)])}
        <Icon name=IconName::ChevronRight />
      </button>
    </li>
  }
}

/// One thing to look at.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Attention {
  Owners(OwnerWarning),
  /// Owners whose identity key this device can't use: they can't sign and don't count.
  UnverifiableOwners(usize),
  /// Roles can't follow the identity provider by themselves until somebody acts.
  RoleSigning(RoleSigningAction),
  /// Admins asking this person to join one of their private channels.
  JoinRequests(usize),
}

/// Who has to act on a [`RoleSigningIssue`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoleSigningAction {
  /// An owner looks at people or the provider (Security policy).
  Owner,
  /// The identity provider needs a setting of the operator.
  Operator,
}

/// Where an item is fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
  Admin(AdminSection),
  Activity,
}

impl Attention {
  fn target(&self) -> Target {
    match self {
      Self::Owners(_) | Self::UnverifiableOwners(_) | Self::RoleSigning(_) => {
        Target::Admin(AdminSection::Security)
      },
      Self::JoinRequests(_) => Target::Activity,
    }
  }
}

fn item_text(i18n: I18n, item: &Attention) -> String {
  match item {
    Attention::Owners(warning) => warning_text(i18n, *warning),
    Attention::UnverifiableOwners(count) => i18n.t_count("admin-attention-unverifiable-owners", *count),
    Attention::RoleSigning(RoleSigningAction::Owner) => i18n.t("admin-attention-roles-owner"),
    Attention::RoleSigning(RoleSigningAction::Operator) => i18n.t("admin-attention-roles-operator"),
    Attention::JoinRequests(count) => i18n.t_count("admin-attention-join-requests", *count),
  }
}

/// The items in order of urgency. Signing issues the apps settle by themselves (people the server
/// reports gone, an owner role not signed yet) aren't listed: there is nothing to do.
fn attention_items(
  maybe_roster: Option<&OwnerRosterStatus>,
  join_requests: usize,
  now: TimestampMs,
) -> Vec<Attention> {
  let mut items = Vec::new();
  if let Some(roster) = maybe_roster {
    items.extend(roster.warnings(now).into_iter().map(Attention::Owners));
    if !roster.unverifiable_owners.is_empty() {
      items.push(Attention::UnverifiableOwners(roster.unverifiable_owners.len()));
    }
    let maybe_action = roster
      .maybe_signing_problem
      .as_ref()
      .and_then(|issue| match issue {
        RoleSigningIssue::Contradictory { .. } | RoleSigningIssue::ProviderChanged { .. } => {
          Some(RoleSigningAction::Owner)
        },
        RoleSigningIssue::NoRoleClaim | RoleSigningIssue::NoKeyClaim => Some(RoleSigningAction::Operator),
        RoleSigningIssue::ReportedGone { .. } | RoleSigningIssue::AwaitingOwner => None,
      });
    items.extend(maybe_action.map(Attention::RoleSigning));
  }
  if join_requests > 0 {
    items.push(Attention::JoinRequests(join_requests));
  }
  items
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::admin::OwnerSigning;
  use teamcraft_proto::Id;

  use crate::model::UserId;

  const NOW: TimestampMs = 1_000_000_000;

  #[test]
  fn nothing_to_do_lists_nothing() {
    assert!(attention_items(None, 0, NOW).is_empty());
    assert!(attention_items(Some(&roster(2)), 0, NOW).is_empty());
  }

  #[test]
  fn too_few_owners_and_unusable_keys_point_to_security() {
    let mut status = roster(1);
    status.unverifiable_owners = vec![user(7)];
    let items = attention_items(Some(&status), 0, NOW);
    assert_eq!(
      items,
      [
        Attention::Owners(OwnerWarning::TooFewOwners { count: 1, minimum: 2 }),
        Attention::UnverifiableOwners(1),
      ]
    );
    assert!(
      items
        .iter()
        .all(|item| item.target() == Target::Admin(AdminSection::Security))
    );
  }

  #[test]
  fn only_signing_issues_somebody_must_act_on_are_listed() {
    let with = |issue| {
      let mut status = roster(2);
      status.maybe_signing_problem = Some(issue);
      attention_items(Some(&status), 0, NOW)
    };
    assert_eq!(
      with(RoleSigningIssue::Contradictory { users: vec![user(3)] }),
      [Attention::RoleSigning(RoleSigningAction::Owner)]
    );
    assert_eq!(
      with(RoleSigningIssue::NoRoleClaim),
      [Attention::RoleSigning(RoleSigningAction::Operator)]
    );
    assert!(with(RoleSigningIssue::ReportedGone { users: vec![user(3)] }).is_empty());
    assert!(with(RoleSigningIssue::AwaitingOwner).is_empty());
  }

  #[test]
  fn waiting_join_requests_go_to_activity() {
    let items = attention_items(None, 2, NOW);
    assert_eq!(items, [Attention::JoinRequests(2)]);
    assert_eq!(items[0].target(), Target::Activity);
  }

  fn roster(owners: i64) -> OwnerRosterStatus {
    OwnerRosterStatus {
      version: 1,
      owners: (1..=owners)
        .map(|id| OwnerSigning {
          user: user(id),
          maybe_last_signed_at: Some(NOW),
        })
        .collect(),
      admins: Vec::new(),
      unverifiable_owners: Vec::new(),
      maybe_expires_at: None,
      recommended_minimum: 2,
      warning_lead_ms: 0,
      is_signing_manual: false,
      is_own_account_mismatched: false,
      maybe_signing_problem: None,
      contradicted_accounts: Vec::new(),
    }
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(crate::model::Snowflake::new(raw).unwrap())
  }
}
