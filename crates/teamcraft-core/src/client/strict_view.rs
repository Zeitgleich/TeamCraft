//! What the UI shows about a private channel's strict mode (e2ee.md §4.6.2, issue #1897): a view
//! made from the group this device holds, kept in the store so the UI reads it without the group.
//!
//! The view is written only while this build's leaves announce strict channels
//! ([`announces_strict_roles`]): an app that doesn't implement the rule shows nothing about it and
//! claims no protection. It is made afresh when the UI asks ([`Client::refresh_strict_view`]),
//! after each strict step this device takes, after a commit changed the group, at a stop and at a
//! held welcome; a channel with nothing to show and no view yet gets none.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::mls::announces_strict_roles;
#[cfg(any(not(target_arch = "wasm32"), feature = "strict-channels"))]
use teamcraft_crypto::strict_roles::StrictRoleKind;
#[cfg(any(not(target_arch = "wasm32"), feature = "strict-channels"))]
use teamcraft_proto::{ChannelRole, ConversationKind, ConversationResponse, ConversationVisibility};
use teamcraft_proto::{ConversationId, DeviceId, UserId};

use super::Client;
use crate::api::ServerApi;
#[cfg(any(not(target_arch = "wasm32"), feature = "strict-channels"))]
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// A private channel's strict mode as this device sees it; the UI's `StrictChannel`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictView {
  #[serde(default)]
  pub has_group: bool,
  #[serde(default)]
  pub is_strict: bool,
  /// The context's roles while strict, otherwise the roles turning it on would sign.
  #[serde(default)]
  pub roles: Vec<StrictViewRole>,
  #[serde(default)]
  pub workspace_signers: Vec<UserId>,
  #[serde(default, rename = "readiness", skip_serializing_if = "Option::is_none")]
  pub maybe_readiness: Option<StrictViewReadiness>,
  #[serde(default, rename = "stop", skip_serializing_if = "Option::is_none")]
  pub maybe_stop: Option<StrictViewStop>,
  #[serde(default, rename = "rejoin", skip_serializing_if = "Option::is_none")]
  pub maybe_rejoin: Option<StrictViewRejoin>,
}

/// A role in the view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictViewRole {
  pub user: UserId,
  /// An admin, otherwise a moderator.
  pub is_admin: bool,
}

/// Steps 2 and 3 in the view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictViewReadiness {
  #[serde(default, rename = "asked_by", skip_serializing_if = "Option::is_none")]
  pub maybe_asked_by: Option<UserId>,
  #[serde(default)]
  pub waiting: Vec<(UserId, DeviceId)>,
  #[serde(default)]
  pub refreshing: Vec<(UserId, DeviceId)>,
  #[serde(default)]
  pub is_ready: bool,
}

/// A stop in the view (D4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictViewStop {
  pub at_ms: i64,
  #[serde(default, rename = "signer", skip_serializing_if = "Option::is_none")]
  pub maybe_signer: Option<UserId>,
}

/// A held welcome in the view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictViewRejoin {
  pub sender: UserId,
  #[serde(default)]
  pub joined: Vec<UserId>,
  #[serde(default)]
  pub gone: Vec<UserId>,
  #[serde(default)]
  pub is_due: bool,
  #[serde(default)]
  pub is_accepted: bool,
}

/// A strict channel step the UI asks for (`CoreCommand::Strict`); each one makes the view afresh
/// when it is done, so the UI shows where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StrictStep {
  /// Only the view, read afresh from the group.
  Refresh,
  /// Ask the devices whether they are ready (step 2).
  AskReady,
  /// Turn strict on with the proposed roles (step 4).
  TurnOn,
  /// Turn strict off.
  TurnOff,
  /// Remove a device that blocks turning strict on (D6).
  RemoveBlocker { user: UserId, device: DeviceId },
  /// Continue from the stop (D4), removing `maybe_remove` first.
  Continue { maybe_remove: Option<UserId> },
  /// Take a member's welcome back on the user's word.
  AcceptRejoin,
}

/// The stored view of a conversation, if the core made one ([`Client::refresh_strict_view`]).
pub fn stored_strict_view(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<StrictView>, CoreError> {
  if !announces_strict_roles() {
    return Ok(None);
  }
  records::get_json(store, Table::AppliedChanges, &view_key(conversation))
}

#[cfg(any(not(target_arch = "wasm32"), feature = "strict-channels"))]
impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Makes the conversation's strict view afresh for the UI (the channel or its settings opened):
  /// for a private channel only.
  pub fn refresh_strict_view(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !announces_strict_roles() {
      return Ok(());
    }
    if !self.is_private_channel(conversation)? {
      self
        .store
        .delete(Table::AppliedChanges, &view_key(conversation))?;
      return Ok(());
    }
    self.write_strict_view(conversation)
  }

  /// The conversation's stored view, as the UI reads it.
  pub fn strict_view(&self, conversation: ConversationId) -> Result<Option<StrictView>, CoreError> {
    stored_strict_view(&self.store, conversation)
  }

  /// Runs a step the UI asked for and makes the view afresh, also when the step failed (the view
  /// then shows where the channel stands, and the failure reaches the user as a toast).
  pub async fn run_strict_step(
    &mut self,
    conversation: ConversationId,
    step: StrictStep,
  ) -> Result<(), CoreError> {
    if !announces_strict_roles() {
      return Err(CoreError::StrictNotNow(
        conversation,
        "this app doesn't implement strict channels",
      ));
    }
    let result = match step {
      StrictStep::Refresh => Ok(()),
      StrictStep::AskReady => Box::pin(self.ask_strict_ready(conversation)).await,
      StrictStep::TurnOn => Box::pin(self.turn_strict_on(conversation)).await,
      StrictStep::TurnOff => Box::pin(self.set_strict_roles(conversation, &[])).await,
      StrictStep::RemoveBlocker { user, device } => {
        Box::pin(self.remove_strict_blocker(conversation, user, device)).await
      },
      StrictStep::Continue { maybe_remove } => {
        Box::pin(self.continue_strict_channel(conversation, maybe_remove)).await
      },
      StrictStep::AcceptRejoin => Box::pin(self.accept_strict_rejoin(conversation)).await,
    };
    let refreshed = self.refresh_strict_view(conversation);
    result.and(refreshed)
  }

  /// Turns the channel strict with its signed channel admins and moderators in the group
  /// ([`Self::proposed_strict_roles`]); at least one admin, or nobody could add anyone (D5).
  async fn turn_strict_on(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let roles: Vec<(UserId, StrictRoleKind)> = self
      .proposed_strict_roles(conversation)?
      .into_iter()
      .map(|role| {
        let kind = if role.is_admin {
          StrictRoleKind::Admin
        } else {
          StrictRoleKind::Moderator
        };
        (role.user, kind)
      })
      .collect();
    if !roles.iter().any(|(_, kind)| *kind == StrictRoleKind::Admin) {
      return Err(CoreError::StrictNotNow(
        conversation,
        "no signed channel admin is in the channel",
      ));
    }
    self.set_strict_roles(conversation, &roles).await
  }

  /// Makes the view afresh if the UI has one or there is something to show (a strict group, a
  /// stop, a held welcome, an open ask): after a commit, a stop, a held welcome or a strict step.
  /// Nothing to do for a channel that never was strict.
  pub(super) fn note_strict_view(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !announces_strict_roles() {
      return Ok(());
    }
    let has_view = self
      .store
      .get(Table::AppliedChanges, &view_key(conversation))?
      .is_some();
    if has_view || self.has_strict_state(conversation)? {
      self.write_strict_view(conversation)?;
    }
    Ok(())
  }

  /// Writes the view when it changed, so the UI redraws only then.
  fn write_strict_view(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let view = self.make_strict_view(conversation)?;
    let maybe_stored: Option<StrictView> =
      records::get_json(&self.store, Table::AppliedChanges, &view_key(conversation))?;
    if maybe_stored.as_ref() != Some(&view) {
      records::put_json(&self.store, Table::AppliedChanges, &view_key(conversation), &view)?;
    }
    Ok(())
  }

  fn make_strict_view(&mut self, conversation: ConversationId) -> Result<StrictView, CoreError> {
    let has_group = self.has_group(conversation)?;
    let workspace = self.workspace()?;
    let (mut workspace_signers, _) = self.fresh_admins(&workspace, self.now_ms());
    workspace_signers.sort();
    workspace_signers.dedup();
    let maybe_stop = self.strict_stop(conversation)?.map(|stop| StrictViewStop {
      at_ms: stop.at_ms,
      maybe_signer: stop.maybe_signer,
    });
    let maybe_rejoin = self
      .strict_rejoin_prompt(conversation)?
      .map(|prompt| StrictViewRejoin {
        sender: prompt.hold.sender,
        joined: prompt.hold.joined,
        gone: prompt.hold.gone,
        is_due: prompt.is_due,
        is_accepted: prompt.hold.is_accepted,
      });
    if !has_group {
      return Ok(StrictView {
        workspace_signers,
        maybe_rejoin,
        ..StrictView::default()
      });
    }
    let maybe_roles = self.strict_roles(conversation)?;
    let is_strict = maybe_roles.is_some();
    let roles = match maybe_roles {
      Some(roles) => {
        let mut list = Vec::with_capacity(roles.roles().len());
        for role in roles.roles() {
          list.push(StrictViewRole {
            user: convert::user_from_crypto(role.user_id)?,
            is_admin: role.role == StrictRoleKind::Admin,
          });
        }
        list
      },
      None => self.proposed_strict_roles(conversation)?,
    };
    let maybe_readiness = if is_strict {
      None
    } else {
      let readiness = self.strict_readiness(conversation)?;
      Some(StrictViewReadiness {
        maybe_asked_by: readiness.maybe_ask.map(|ask| ask.asker),
        waiting: readiness.waiting,
        refreshing: readiness.refreshing,
        is_ready: readiness.is_ready,
      })
    };
    Ok(StrictView {
      has_group,
      is_strict,
      roles,
      workspace_signers,
      maybe_readiness,
      maybe_stop,
      maybe_rejoin,
    })
  }

  /// The roles turning the channel strict signs: its signed channel admins and moderators (§4.6.1)
  /// who are in the group, so each has a leaf (§4.6.2, "A role counts only with a leaf").
  pub(super) fn proposed_strict_roles(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<StrictViewRole>, CoreError> {
    let signed = self.channel_role_state(conversation)?;
    let mut users = self.group_users(conversation)?;
    users.sort();
    users.dedup();
    Ok(
      users
        .into_iter()
        .filter_map(|user| match signed.role_of(user) {
          ChannelRole::Admin => Some(StrictViewRole { user, is_admin: true }),
          ChannelRole::Moderator => Some(StrictViewRole {
            user,
            is_admin: false,
          }),
          _ => None,
        })
        .collect(),
    )
  }

  /// Whether the conversation has anything strict to show without the UI asking.
  fn has_strict_state(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    let has_hold = self
      .store
      .get(
        Table::AppliedChanges,
        &super::strict_channels::hold_key(conversation),
      )?
      .is_some();
    Ok(has_hold || self.strict_stop(conversation)?.is_some() || self.is_strict(conversation)?)
  }

  fn is_private_channel(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let maybe_response: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    Ok(maybe_response.is_some_and(|response| {
      response.kind == ConversationKind::Channel && response.visibility == ConversationVisibility::Private
    }))
  }
}

/// The web build without the `strict-channels` feature: no view is ever made.
#[cfg(not(any(not(target_arch = "wasm32"), feature = "strict-channels")))]
impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub fn refresh_strict_view(&mut self, _conversation: ConversationId) -> Result<(), CoreError> {
    Ok(())
  }

  pub async fn run_strict_step(
    &mut self,
    conversation: ConversationId,
    _step: StrictStep,
  ) -> Result<(), CoreError> {
    Err(CoreError::StrictNotNow(
      conversation,
      "this app doesn't implement strict channels",
    ))
  }
}

fn view_key(conversation: ConversationId) -> String {
  format!("strict-view:{conversation}")
}
