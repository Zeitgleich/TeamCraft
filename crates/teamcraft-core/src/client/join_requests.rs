//! Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md
//! §2.4.1, issue #776).
//!
//! Members' clients refuse an admin's own signed self-join under this policy, so the admin asks
//! and the server keeps the request; nothing about it is trusted. A channel owner's client checks
//! the request against what it verified itself (the policy, the requester in the fresh roster, a
//! private channel whose group it is in) and approves by adding the admin with its own MLS commit,
//! which every member checks like any addition by a member and derives the visible "joined as a
//! workspace admin, approved by …" line from (issue #811).

use teamcraft_crypto::membership::HistoryShare;
use teamcraft_proto::{
  AdminJoinRequestResponse, AdminSelfJoin, ChannelRole, ConversationId, ConversationKind,
  ConversationResponse, ConversationVisibility, HistoryPolicy, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::{CoreError, JoinRequestRefusal};
use crate::read_state;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, Table};
use crate::workspace::WorkspaceState;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// This admin asks the owners of the private channel to let them in. Asking again while a
  /// request is open changes nothing.
  pub async fn request_admin_join(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.user_id();
    if !self.is_fresh_admin(&self.workspace()?, me) {
      return Err(CoreError::NotAnAdmin(me));
    }
    self.api.request_admin_join(conversation).await?;
    self.refresh_admin_join_requests().await
  }

  /// This admin takes their request back.
  pub async fn withdraw_admin_join(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let me = self.user_id();
    self.api.close_admin_join(conversation, me).await?;
    self.refresh_admin_join_requests().await
  }

  /// A channel owner declines the request; the admin is told by the server.
  pub async fn decline_admin_join(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self.api.close_admin_join(conversation, user).await?;
    self.refresh_admin_join_requests().await
  }

  /// A channel owner lets the admin in with the history from the join on: no more than the admin's
  /// own self-join would get (e2ee.md §4.7). See [`Self::approve_admin_join_sharing`].
  pub async fn approve_admin_join(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self
      .approve_admin_join_sharing(conversation, user, HistoryPolicy::FromJoin)
      .await
  }

  /// A channel owner lets the admin in: the server membership, then this device's own MLS add with
  /// `history` (full history only when the owner chose it), then the visible message. Only when
  /// this device's own state agrees (admin.md §2.3): the verified policy asks for an owner's
  /// approval, the fresh verified roster lists the person as an admin, and the conversation is a
  /// private, unarchived channel this device knows and is in the group of, and this user's own
  /// channel role is a signed admin (a role only the server lists isn't: members would refuse the
  /// add). A request the server
  /// makes up for a DM, a meeting chat or an unknown conversation adds nobody. Somebody already in
  /// the group (another owner was first) is neither added nor announced again.
  pub async fn approve_admin_join_sharing(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
  ) -> Result<(), CoreError> {
    let workspace = self.workspace()?;
    if workspace.policy.admin_self_join != AdminSelfJoin::OwnerApproval {
      return Err(CoreError::JoinRequestNotApprovable(
        JoinRequestRefusal::PolicyWithoutApproval,
      ));
    }
    if !self.is_fresh_admin(&workspace, user) {
      return Err(CoreError::NotAnAdmin(user));
    }
    let maybe_response: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    if !maybe_response.as_ref().is_some_and(is_open_private_channel) {
      return Err(CoreError::JoinRequestNotApprovable(
        JoinRequestRefusal::NotAPrivateChannel,
      ));
    }
    // Only a member of the group can commit the add.
    if !self.has_group(conversation)? {
      return Err(CoreError::NoGroup(conversation));
    }
    // Members take in an admin's addition only from a signed channel admin (e2ee.md §4.6.1), never
    // from an owner only the server lists: read the log first, then judge by what is signed.
    self.sync_channel_roles(conversation).await?;
    if self.verified_channel_role(conversation, self.user_id())? != ChannelRole::Admin {
      return Err(CoreError::JoinRequestNotApprovable(
        JoinRequestRefusal::OwnerRoleNotSigned,
      ));
    }
    self.cancel_removal_on_add(conversation, user)?;
    self.api.approve_admin_join(conversation, user).await?;
    if !self.group_users(conversation)?.contains(&user) {
      let requested = match history {
        HistoryPolicy::Full => HistoryShare::Full,
        _ => HistoryShare::FromJoin,
      };
      let shared = self.commit_addition(conversation, user, requested).await?;
      // Every member's device (this one too) shows the line it derived from the commit (issue
      // #811); the message is for older versions. Announced once the admin's devices are in: an add
      // that had to wait (no key packages yet) is announced by nobody rather than twice.
      if self.group_users(conversation)?.contains(&user) {
        tracing::info!(conversation_id = %conversation, ?shared, "let an admin in who asked");
        self
          .announce(
            conversation,
            ConversationEvent::AdminJoined {
              user,
              is_approved: true,
            },
          )
          .await?;
      }
    }
    self.refresh_admin_join_requests().await
  }

  /// Reads the open requests from the server into the store, where the UI reads them.
  pub(crate) async fn refresh_admin_join_requests(&mut self) -> Result<(), CoreError> {
    let requests: Vec<AdminJoinRequestResponse> = self.api.admin_join_requests().await?;
    read_state::put_admin_join_requests(&self.store, &requests)
  }

  /// After a (re)connect: events in between were lost. Only while the verified policy asks for an
  /// owner's approval (otherwise there are none, and no request is made); a failure is only
  /// logged, the next connect tries again.
  pub(crate) async fn refresh_admin_join_requests_after_connect(&mut self) {
    let has_requests = match self.workspace() {
      Ok(workspace) => workspace.policy.admin_self_join == AdminSelfJoin::OwnerApproval,
      Err(error) => {
        tracing::warn!(%error, "could not read the workspace for the admin join requests");
        return;
      },
    };
    let outcome = if has_requests {
      self.refresh_admin_join_requests().await
    } else {
      read_state::put_admin_join_requests(&self.store, &[])
    };
    if let Err(error) = outcome {
      tracing::debug!(%error, "could not read the open admin join requests");
    }
  }

  /// Whether the verified roster names `user` an admin and is fresh enough to grant authority,
  /// judged like the members' clients judge commits (admin.md §2.3, freshness).
  fn is_fresh_admin(&self, workspace: &WorkspaceState, user: UserId) -> bool {
    let max_age_ms = i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    workspace.is_admin(user) && workspace.is_roster_fresh(self.now_ms(), max_age_ms)
  }
}

/// The only kind of conversation an admin's request can be approved for (admin.md §2.4): a
/// private, unarchived channel. Not a DM, a group DM or a meeting chat.
pub fn is_open_private_channel(conversation: &ConversationResponse) -> bool {
  conversation.kind == ConversationKind::Channel
    && conversation.visibility == ConversationVisibility::Private
    && !conversation.is_archived
}
