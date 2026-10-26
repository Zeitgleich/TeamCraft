//! Admins asking to join a private channel under `admin_self_join = owner_approval` (admin.md §2.4,
//! issue #776).
//!
//! Members' clients refuse an admin's own signed self-join under this policy, so the admin asks
//! and the server keeps the request; nothing about it is trusted. A channel owner's client checks
//! the requester against the fresh verified roster and approves by adding them with its own MLS
//! commit, which every member checks like any addition by a member, then posts the visible "joined
//! as a workspace admin" message.

use teamcraft_crypto::membership::HistoryShare;
use teamcraft_proto::{AdminJoinRequestResponse, AdminSelfJoin, ConversationId, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::read_state;
use crate::store::{ConversationEvent, LocalStore};
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

  /// A channel owner lets the admin in: the server membership, then this device's own MLS add with
  /// the signed default history (as a self-join would get), then the visible message. Only for
  /// somebody the fresh verified roster lists as an admin: the server's word that they are one is
  /// not enough (admin.md §2.3).
  pub async fn approve_admin_join(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    if !self.is_fresh_admin(&self.workspace()?, user) {
      return Err(CoreError::NotAnAdmin(user));
    }
    // Only a member of the group can commit the add.
    if !self.has_group(conversation)? {
      return Err(CoreError::NoGroup(conversation));
    }
    self.cancel_removal_on_add(conversation, user)?;
    self.api.approve_admin_join(conversation, user).await?;
    let requested = self.implicit_history_in(conversation, user).await?;
    let history: HistoryShare = self.commit_addition(conversation, user, requested).await?;
    tracing::info!(conversation_id = %conversation, ?history, "let an admin in who asked");
    self
      .announce(
        conversation,
        ConversationEvent::AdminJoined {
          user,
          is_approved: true,
        },
      )
      .await?;
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
