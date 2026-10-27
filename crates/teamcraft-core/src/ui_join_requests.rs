//! Admins' requests to join a private channel as the UI shows them (admin.md §2.4, issue #776):
//! the owner's side in the activity feed and the channel settings, the admin's in the channel
//! browser.
//!
//! The list is the server's word. What is shown and whether the owner may approve is decided here,
//! from this device's verified roster and policy and the conversations it knows, never from the
//! server's role list (admin.md §2.3): a request for anything but a private, unarchived channel
//! (a DM, a meeting chat, an unknown conversation) isn't shown at all.

use std::collections::HashSet;

use teamcraft_client_api::activity::{JoinRequest, JoinRequestStanding};
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_proto::{AdminJoinRequestResponse, AdminSelfJoin, ChannelRole};

use crate::client::is_open_private_channel;
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
    let channels: HashSet<ConversationId> = self
      .conversations_raw()
      .into_iter()
      .filter(|conversation| {
        conversation.maybe_my_membership.is_some() && is_open_private_channel(conversation)
      })
      .map(|conversation| conversation.id)
      .collect();
    requests_to_decide(
      self.me_id(),
      &requests,
      &workspace,
      is_roster_fresh,
      |conversation| channels.contains(&conversation),
      |conversation| self.is_my_role_signed_admin(conversation),
    )
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

  /// Whether the channel's signed roles make this user an admin: only then members take in the
  /// admin this user approves (e2ee.md §4.6.1). The server's member list doesn't count.
  fn is_my_role_signed_admin(&self, conversation: ConversationId) -> bool {
    let state: crate::channel_roles::ChannelRoleState = self.read(
      records::get_json(
        self.store_ref(),
        Table::Workspace,
        &crate::client::channel_roles_key(conversation),
      )
      .map(Option::unwrap_or_default),
    );
    state.role_of(self.me_id()) == ChannelRole::Admin
  }

  fn verified_workspace(&self) -> WorkspaceState {
    self.read(
      records::get_json(self.store_ref(), Table::Workspace, crate::client::WORKSPACE_KEY)
        .map(Option::unwrap_or_default),
    )
  }
}

/// The requests of others (the server lists the user's own too) for conversations
/// `is_my_private_channel` accepts (private, unarchived channels the user is in), each with what
/// the owner may do (`is_signed_admin`: whether this user's role in the channel is a signed admin).
/// None while the verified policy doesn't ask for an owner's approval: then they
/// are moot, and the server closes them anyway.
pub fn requests_to_decide(
  me: UserId,
  requests: &[AdminJoinRequestResponse],
  workspace: &WorkspaceState,
  is_roster_fresh: bool,
  is_my_private_channel: impl Fn(ConversationId) -> bool,
  is_signed_admin: impl Fn(ConversationId) -> bool,
) -> Vec<JoinRequest> {
  if workspace.policy.admin_self_join != AdminSelfJoin::OwnerApproval {
    return Vec::new();
  }
  requests
    .iter()
    .filter(|request| request.user.id != me && is_my_private_channel(request.conversation_id))
    .map(|request| JoinRequest {
      conversation: request.conversation_id,
      user: request.user.id,
      user_name: request.user.display_name.clone(),
      maybe_username: request.user.maybe_username.clone(),
      requested_at: request.requested_at.0,
      expires_at: request.expires_at.0,
      standing: if !is_roster_fresh || !workspace.is_admin(request.user.id) {
        JoinRequestStanding::NotAnAdmin
      } else if is_signed_admin(request.conversation_id) {
        JoinRequestStanding::Approvable
      } else {
        JoinRequestStanding::OwnerRoleNotSigned
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

  const CHANNEL: u64 = 50;
  const DM: u64 = 60;

  #[test]
  fn an_admin_of_the_fresh_roster_can_be_approved() {
    let shown = decide(ME, &[request(ADMIN, CHANNEL)], AdminSelfJoin::OwnerApproval, true);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].standing, JoinRequestStanding::Approvable);
    assert_eq!(shown[0].user_name, "Person 2");
    assert_eq!(shown[0].maybe_username.as_deref(), Some("person2"));
  }

  #[test]
  fn the_servers_word_alone_or_a_stale_roster_only_allows_declining() {
    let unknown = decide(
      ME,
      &[request(STRANGER, CHANNEL)],
      AdminSelfJoin::OwnerApproval,
      true,
    );
    assert_eq!(unknown[0].standing, JoinRequestStanding::NotAnAdmin);
    let stale = decide(
      ME,
      &[request(ADMIN, CHANNEL)],
      AdminSelfJoin::OwnerApproval,
      false,
    );
    assert_eq!(stale[0].standing, JoinRequestStanding::NotAnAdmin);
  }

  /// Review M1: a request the server makes up for a DM, a meeting chat or a conversation this
  /// device doesn't know as one of its private channels isn't shown at all.
  #[test]
  fn requests_for_anything_but_my_private_channels_are_not_shown() {
    let shown = decide(
      ME,
      &[request(ADMIN, DM), request(ADMIN, 70), request(ADMIN, CHANNEL)],
      AdminSelfJoin::OwnerApproval,
      true,
    );
    let conversations: Vec<_> = shown.iter().map(|request| request.conversation).collect();
    assert_eq!(conversations, vec![conversation(CHANNEL)]);
  }

  #[test]
  fn own_requests_and_other_policies_show_nothing_to_decide() {
    assert!(
      decide(
        ADMIN,
        &[request(ADMIN, CHANNEL)],
        AdminSelfJoin::OwnerApproval,
        true
      )
      .is_empty()
    );
    for policy in [
      AdminSelfJoin::Visible,
      AdminSelfJoin::Disabled,
      AdminSelfJoin::Unknown("later".into()),
    ] {
      assert!(decide(ME, &[request(ADMIN, CHANNEL)], policy, true).is_empty());
    }
  }

  /// An owner whom only the server lists can't approve: members would refuse the add (issue #380).
  #[test]
  fn an_owner_without_a_signed_admin_role_can_only_decline() {
    let shown = requests_to_decide(
      user(ME),
      &[request(ADMIN, CHANNEL)],
      &workspace(AdminSelfJoin::OwnerApproval),
      true,
      |id| id == conversation(CHANNEL),
      |_| false,
    );
    assert_eq!(shown[0].standing, JoinRequestStanding::OwnerRoleNotSigned);
  }

  /// [`requests_to_decide`] for `me`, where only [`CHANNEL`] is one of the user's private channels.
  fn decide(
    me: u64,
    requests: &[AdminJoinRequestResponse],
    self_join: AdminSelfJoin,
    is_roster_fresh: bool,
  ) -> Vec<JoinRequest> {
    requests_to_decide(
      user(me),
      requests,
      &workspace(self_join),
      is_roster_fresh,
      |id| id == conversation(CHANNEL),
      |_| true,
    )
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

  fn request(raw: u64, channel: u64) -> AdminJoinRequestResponse {
    AdminJoinRequestResponse {
      conversation_id: conversation(channel),
      user: UserSummary {
        id: user(raw),
        maybe_username: Some(format!("person{raw}")),
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

  fn conversation(raw: u64) -> ConversationId {
    teamcraft_client_api::model::id_from_raw(raw)
  }
}
