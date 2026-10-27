//! Admins' requests to join a private channel as the UI shows them (admin.md §2.4, issue #776):
//! the owner's side in the activity feed and the channel settings, the admin's in the channel
//! browser.
//!
//! The list is the server's word. Whether the owner may approve is decided here, from this device's
//! verified roster and policy, never from the server's role list (admin.md §2.3).

use std::collections::HashSet;

use teamcraft_client_api::activity::{JoinRequest, JoinRequestStanding};
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_proto::{AdminJoinRequestResponse, AdminSelfJoin};

use crate::read_state;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::ui_backend::CoreChatBackend;
use crate::workspace::WorkspaceState;

/// How old the newest signed roster may be before admins lose their authority, as the client's
/// default (`ClientSettings::workspace_max_age`). The UI only uses it to decide whether to offer
/// "Approve"; the client checks again with its own setting when the owner approves.
const ROSTER_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

impl<S: LocalStore> CoreChatBackend<S> {
  /// The requests waiting for this user's decision, as owner of the channels they name.
  pub(crate) fn ui_join_requests(&self) -> Vec<JoinRequest> {
    let requests = self.read(read_state::admin_join_requests(self.store_ref()));
    if requests.is_empty() {
      return Vec::new();
    }
    let workspace = self.verified_workspace();
    let is_roster_fresh = workspace.is_roster_fresh(self.now_ms(), ROSTER_MAX_AGE_MS);
    requests_to_decide(self.me_id(), &requests, &workspace, is_roster_fresh)
  }

  /// The private channels this admin asked to join and still waits for.
  pub(crate) fn my_join_requests(&self) -> HashSet<ConversationId> {
    let me = self.me_id();
    self
      .read(read_state::admin_join_requests(self.store_ref()))
      .into_iter()
      .filter(|request| request.user.id == me)
      .map(|request| request.conversation_id)
      .collect()
  }

  fn verified_workspace(&self) -> WorkspaceState {
    self.read(
      records::get_json(self.store_ref(), Table::Workspace, crate::client::WORKSPACE_KEY)
        .map(Option::unwrap_or_default),
    )
  }
}

/// The requests of others (the server lists the user's own too), each with what the owner may do.
/// None while the verified policy doesn't ask for an owner's approval: then they are moot, and the
/// server closes them anyway.
pub fn requests_to_decide(
  me: UserId,
  requests: &[AdminJoinRequestResponse],
  workspace: &WorkspaceState,
  is_roster_fresh: bool,
) -> Vec<JoinRequest> {
  if workspace.policy.admin_self_join != AdminSelfJoin::OwnerApproval {
    return Vec::new();
  }
  requests
    .iter()
    .filter(|request| request.user.id != me)
    .map(|request| JoinRequest {
      conversation: request.conversation_id,
      user: request.user.id,
      user_name: request.user.display_name.clone(),
      requested_at: request.requested_at.0,
      expires_at: request.expires_at.0,
      standing: if is_roster_fresh && workspace.is_admin(request.user.id) {
        JoinRequestStanding::Approvable
      } else {
        JoinRequestStanding::NotAnAdmin
      },
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{UnixMillis, UserKind, UserSummary};

  use super::*;
  use crate::workspace::{RosterMember, RosterRole};

  const ME: u64 = 1;
  const ADMIN: u64 = 2;
  const STRANGER: u64 = 3;

  #[test]
  fn an_admin_of_the_fresh_roster_can_be_approved() {
    let shown = requests_to_decide(
      user(ME),
      &[request(ADMIN)],
      &workspace(AdminSelfJoin::OwnerApproval),
      true,
    );
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].standing, JoinRequestStanding::Approvable);
    assert_eq!(shown[0].user_name, "Person 2");
  }

  #[test]
  fn the_servers_word_alone_or_a_stale_roster_only_allows_declining() {
    let workspace = workspace(AdminSelfJoin::OwnerApproval);
    let unknown = requests_to_decide(user(ME), &[request(STRANGER)], &workspace, true);
    assert_eq!(unknown[0].standing, JoinRequestStanding::NotAnAdmin);
    let stale = requests_to_decide(user(ME), &[request(ADMIN)], &workspace, false);
    assert_eq!(stale[0].standing, JoinRequestStanding::NotAnAdmin);
  }

  #[test]
  fn own_requests_and_other_policies_show_nothing_to_decide() {
    let own = requests_to_decide(
      user(ADMIN),
      &[request(ADMIN)],
      &workspace(AdminSelfJoin::OwnerApproval),
      true,
    );
    assert!(own.is_empty());
    for policy in [
      AdminSelfJoin::Visible,
      AdminSelfJoin::Disabled,
      AdminSelfJoin::Unknown("later".into()),
    ] {
      assert!(requests_to_decide(user(ME), &[request(ADMIN)], &workspace(policy), true).is_empty());
    }
  }

  fn workspace(self_join: AdminSelfJoin) -> WorkspaceState {
    let mut state = WorkspaceState::default();
    state.policy.admin_self_join = self_join;
    state.roster.members.push(RosterMember {
      user_id: user(ADMIN),
      role: RosterRole::Admin,
      identity_key: teamcraft_proto::Base64Bytes::new(vec![7; 32]),
    });
    state
  }

  fn request(raw: u64) -> AdminJoinRequestResponse {
    AdminJoinRequestResponse {
      conversation_id: teamcraft_client_api::model::id_from_raw(50),
      user: UserSummary {
        id: user(raw),
        maybe_username: None,
        display_name: format!("Person {raw}"),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_deactivated: false,
        is_deleted: false,
      },
      requested_at: UnixMillis(10),
      expires_at: UnixMillis(20),
    }
  }

  fn user(raw: u64) -> UserId {
    teamcraft_client_api::model::id_from_raw(raw)
  }
}
