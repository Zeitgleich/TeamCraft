//! Says when the signed-in user's admin role isn't in force on other people's apps (admin.md
//! §2.3, issue #765): the identity provider gave a role nobody signed, the signed roster expired,
//! none was signed yet, or a signing owner has provider roles waiting to be signed. Shown above
//! the app and on every page of the admin console, with the way out.

use leptos::prelude::*;
use teamcraft_client_api::admin::{RosterStanding, roster_standing, signed_roster};
use teamcraft_proto::Role;

use crate::admin::AdminSection;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::state::{AppState, use_app_state};

/// The banner; nothing for members and for admins whose role is in force.
#[component]
pub fn RosterBanner() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // A signing owner learns about provider roles to sign only from the admin data: fetch it once.
  if state.read(|backend| backend.my_role()) == Role::Owner
    && state.read(|backend| backend.can_sign_workspace())
  {
    state.read(|backend| backend.admin_refresh());
  }
  let standing = Memo::new(move |_| current_standing(state));
  move || {
    let (text, maybe_action) = banner_text(i18n, state, &standing.get())?;
    Some(view! {
      <div class="update-banner roster-banner" role="status">
        <Icon name=IconName::Shield />
        <span>{text}</span>
        {maybe_action.map(|action| view! {
          <button class="btn btn-primary btn-small" on:click=move |_| run(state, action)>
            {move || i18n.t(action.label_key())}
          </button>
        })}
      </div>
    })
  }
}

/// What the banner's button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BannerAction {
  /// Administration → Security, where the owners and the roster editor are.
  OpenOwners,
  /// Signs the verified roster again as it is: nobody is added or removed.
  SignAgain,
}

impl BannerAction {
  fn label_key(self) -> &'static str {
    match self {
      Self::OpenOwners => "roster-standing-open",
      Self::SignAgain => "roster-standing-sign",
    }
  }
}

fn run(state: AppState, action: BannerAction) {
  match action {
    BannerAction::OpenOwners => state.open_admin(AdminSection::Security),
    BannerAction::SignAgain => {
      if let Some(status) = state.read(|backend| backend.owner_roster()) {
        state.write(|backend| backend.admin_publish_roster(status.version, signed_roster(&status)));
      }
    },
  }
}

/// The user's standing now, from what the backend verified.
pub fn current_standing(state: AppState) -> RosterStanding {
  state.read(|backend| {
    let (idp_owners, idp_admins) = backend.admin_role_holders();
    roster_standing(
      backend.me().id,
      &backend.my_role(),
      backend.owner_roster().as_ref(),
      (&idp_owners, &idp_admins),
      backend.can_sign_workspace(),
      now_ms(),
    )
  })
}

/// Why an admin-only action isn't offered, when the reason is the unsigned role (`None` when the
/// role is in force or the user has none).
pub fn unsigned_reason(i18n: I18n, state: AppState) -> Option<String> {
  let standing = current_standing(state);
  standing
    .is_unsigned()
    .then(|| banner_text(i18n, state, &standing).map(|(text, _)| text))
    .flatten()
}

/// The text and the button for a standing.
fn banner_text(
  i18n: I18n,
  state: AppState,
  standing: &RosterStanding,
) -> Option<(String, Option<BannerAction>)> {
  Some(match standing {
    RosterStanding::InForce => return None,
    RosterStanding::NotSigned { role, owners } => {
      let key = if *role == Role::Owner {
        "roster-standing-owner-unsigned"
      } else {
        "roster-standing-admin-unsigned"
      };
      (
        i18n.t_with(key, &[("owners", &names(state, owners))]),
        Some(BannerAction::OpenOwners),
      )
    },
    RosterStanding::Expired { can_sign: true, .. } => (
      i18n.t("roster-standing-expired-sign"),
      Some(BannerAction::SignAgain),
    ),
    RosterStanding::Expired {
      can_sign: false,
      owners,
    } => (
      i18n.t_with(
        "roster-standing-expired-ask",
        &[("owners", &names(state, owners))],
      ),
      None,
    ),
    RosterStanding::NoRoster => (i18n.t("roster-standing-none"), Some(BannerAction::OpenOwners)),
    RosterStanding::AdditionsToSign { count } => (
      i18n.t_count("roster-standing-additions", *count),
      Some(BannerAction::OpenOwners),
    ),
  })
}

/// The owners' names, for "ask …".
fn names(state: AppState, users: &[UserId]) -> String {
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
