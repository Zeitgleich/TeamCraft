//! Says what a person really has to act on about the admin roster, and nothing else (admin.md §2.3.1,
//! issue #1276): apps keep it following the identity provider and sign it again by themselves. Above
//! the app and on every page of the console, only when no owner has had a device for so long that
//! admin rights lapsed, or when evidence contradicts what is signed and an owner has to look.
//! [`unsigned_reason`] says why an action is refused, where it is refused.

use leptos::prelude::*;
use teamcraft_client_api::admin::{RoleSigningIssue, RosterStanding, roster_standing};

use crate::admin::AdminSection;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::state::{AppState, use_app_state};

/// The banner; nothing at all in normal operation.
#[component]
pub fn RosterBanner() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let line = Memo::new(move |_| banner_line(i18n, state));
  move || {
    let (text, has_action) = line.get()?;
    Some(view! {
      <div class="update-banner roster-banner" role="status">
        <Icon name=IconName::Shield />
        <span>{text}</span>
        {has_action.then(|| view! {
          <button class="btn btn-primary btn-small" on:click=move |_| state.open_admin(AdminSection::Security)>
            {move || i18n.t("roster-standing-open")}
          </button>
        })}
      </div>
    })
  }
}

/// The user's standing now, from what the backend verified.
pub fn current_standing(state: AppState) -> RosterStanding {
  state.read(|backend| {
    roster_standing(
      backend.me().id,
      &backend.my_role(),
      backend.owner_roster().as_ref(),
      backend.can_sign_workspace(),
      now_ms(),
    )
  })
}

/// Why an admin-only action isn't offered, when the reason is that the role isn't in force yet
/// (`None` when it is, or the user has none). Apps sign it by themselves, so there is nothing to do.
pub fn unsigned_reason(i18n: I18n, state: AppState) -> Option<String> {
  match current_standing(state) {
    RosterStanding::InForce => None,
    RosterStanding::Expired { owners, .. } => Some(expired_line(i18n, state, &owners)),
    RosterStanding::NotSigned { .. } | RosterStanding::NoRoster => Some(i18n.t("roster-standing-waiting")),
  }
}

/// The line to show, and whether it comes with the way to the console.
fn banner_line(i18n: I18n, state: AppState) -> Option<(String, bool)> {
  // Admin rights lapsed and this device can't renew them: only an owner's app can.
  if let RosterStanding::Expired {
    can_sign: false,
    owners,
  } = current_standing(state)
  {
    return Some((expired_line(i18n, state, &owners), false));
  }
  // The owner's lines first: the server alone can raise the next one by showing a fork, and it must
  // not be able to hide these with it.
  owner_line(i18n, state).or_else(|| role_alarm_line(i18n, state).map(|line| (line, false)))
}

/// What only an owner can do something about.
fn owner_line(i18n: I18n, state: AppState) -> Option<(String, bool)> {
  // Only an owner can do something about a contradiction or another provider: an owner of the verified
  // roster, not whom the server calls one, so that a server can't hide this from them.
  let status = state.read(|backend| backend.owner_roster())?;
  let me = state.read(|backend| backend.me().id);
  if !status.owners.iter().any(|owner| owner.user == me) {
    return None;
  }
  match status.maybe_signing_problem? {
    RoleSigningIssue::Contradictory { users } => Some((
      i18n.t_with(
        "roster-signing-contradictory",
        &[("names", &names(state, &users))],
      ),
      true,
    )),
    RoleSigningIssue::ProviderChanged { .. } => Some((i18n.t("roster-signing-provider-changed"), true)),
    // In the console's Security section: nothing to say on every page.
    RoleSigningIssue::NoRoleClaim
    | RoleSigningIssue::NoKeyClaim
    | RoleSigningIssue::ReportedGone { .. }
    | RoleSigningIssue::AwaitingOwner => None,
  }
}

/// What a workspace admin of the verified roster hears about a channel whose role logs disagree.
fn role_alarm_line(i18n: I18n, state: AppState) -> Option<String> {
  let me = state.read(|backend| backend.me().id);
  let status = state.read(|backend| backend.owner_roster())?;
  if !status.owners.iter().any(|owner| owner.user == me) && !status.admins.contains(&me) {
    return None;
  }
  let conversation = state
    .read(|backend| backend.channel_role_alarms())
    .into_iter()
    .next()?;
  Some(
    state
      .read(|backend| backend.conversation(conversation))
      .map_or_else(
        || i18n.t("channel-roles-alarm-banner-unnamed"),
        |info| i18n.t_with("channel-roles-alarm-banner", &[("channel", &info.name)]),
      ),
  )
}

/// That admin rights lapsed, and whom to ask: by name, or just "an owner" where this device knows no
/// name, as an ID tells nobody whom to ask.
fn expired_line(i18n: I18n, state: AppState, owners: &[UserId]) -> String {
  let known: Vec<UserId> = owners
    .iter()
    .copied()
    .filter(|user| state.read(|backend| backend.user(*user)).is_some())
    .collect();
  if known.is_empty() {
    return i18n.t("roster-standing-expired-ask-unnamed");
  }
  i18n.t_with(
    "roster-standing-expired-ask",
    &[("owners", &names(state, &known))],
  )
}

/// People's names, for "ask …".
pub(crate) fn names(state: AppState, users: &[UserId]) -> String {
  users
    .iter()
    .map(|user| {
      state
        .read(|backend| backend.user(*user))
        .map_or_else(|| user.to_string(), |found| found.display_name)
    })
    .collect::<Vec<_>>()
    .join(", ")
}
