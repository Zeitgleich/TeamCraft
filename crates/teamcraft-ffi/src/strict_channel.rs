//! Strict channels on the phone (e2ee.md §4.6.2, issue #1897), as desktop and web show them
//! (`teamcraft-ui`'s `strict_channel`): the setting in a private channel's settings, who loses what,
//! the devices that block turning it on, and the notices when this device stopped at a refused
//! change or holds a welcome back.
//!
//! What to show and which actions to offer is decided here
//! ([`teamcraft_client_api::strict_channel`]); the apps only render it. Nothing is returned while
//! this build doesn't implement strict channels, so the apps claim no protection then.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::model::{ConversationId, DeviceId, UserId};
use teamcraft_client_api::strict_channel::{
  StrictActor, StrictChannel, StrictGates, StrictLossKind, StrictRole, strict_losses_in,
};
use teamcraft_core::client::StrictStep;
use teamcraft_proto::{Permission, PermissionScheme};

use crate::TeamCraft;
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// A private channel's strict mode as the phone shows it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictChannelItem {
  /// `channel-strict-on` / `channel-strict-off`.
  pub is_strict: bool,
  /// The context's roles while strict; otherwise the roles turning it on signs.
  pub roles: Vec<StrictPersonItem>,
  /// Who loses what when it turns on (`channel-strict-losses`); empty unless the user may turn it
  /// on.
  pub losses: Vec<StrictLossItem>,
  /// At least one admin is in the roles; otherwise `channel-strict-needs-admin` instead of the
  /// steps.
  pub has_admin: bool,
  /// Steps 2 and 3; `None` once strict.
  pub maybe_readiness: Option<StrictReadinessItem>,
  /// This device stopped at a refused change (`channel-strict-stopped`).
  pub maybe_stop: Option<StrictStopItem>,
  /// A welcome back that wasn't opened.
  pub maybe_rejoin: Option<StrictRejoinItem>,
  /// "Check devices" / "Turn on" (D3); without it `channel-strict-ask-workspace-admin`.
  pub can_turn_on: bool,
  pub can_turn_off: bool,
  /// "Remove device" next to a waiting device (D6).
  pub can_remove_blocker: bool,
  /// "Remove {name} and continue" / "Continue" in the stop notice (D4).
  pub can_continue: bool,
}

/// A person with a role in a strict channel.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictPersonItem {
  pub user_id: String,
  /// From the directory; `None`: `channel-strict-someone`.
  pub maybe_name: Option<String>,
  /// `channel-strict-role-admin`, otherwise `channel-strict-role-moderator`.
  pub is_admin: bool,
}

/// What somebody loses when the channel turns strict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum StrictLossKindItem {
  /// `channel-strict-loses-changing`
  ChangingMembers,
  /// `channel-strict-loses-adding`
  AddingMembers,
}

/// Somebody who loses something when the channel turns strict.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictLossItem {
  pub user_id: String,
  pub maybe_name: Option<String>,
  pub loss: StrictLossKindItem,
}

/// Steps 2 and 3 of turning strict on.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictReadinessItem {
  /// Somebody asked: show `waiting` and `refreshing_count`; otherwise "Check devices".
  pub has_asked: bool,
  /// `channel-strict-waiting` with their count, one row each.
  pub waiting: Vec<StrictDeviceItem>,
  /// `channel-strict-refreshing`.
  pub refreshing_count: u32,
  /// "Turn on" is offered.
  pub is_ready: bool,
}

/// A device that blocks turning strict on.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictDeviceItem {
  pub user_id: String,
  pub device_id: String,
  pub maybe_name: Option<String>,
}

/// Where this device stopped.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictStopItem {
  /// Whom "Remove {name} and continue" names; `None`: only "Continue". Never the user.
  pub maybe_signer_id: Option<String>,
  pub maybe_signer_name: Option<String>,
}

/// A held welcome back into the channel.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StrictRejoinItem {
  pub maybe_sender_name: Option<String>,
  /// `channel-strict-rejoin-joined`, names joined with ", ".
  pub joined_names: Vec<String>,
  /// `channel-strict-rejoin-gone`.
  pub gone_names: Vec<String>,
  /// `channel-strict-rejoin-due` with "Join"; otherwise `channel-strict-rejoin-waiting`.
  pub is_due: bool,
  /// `channel-strict-rejoin-accepted`.
  pub is_accepted: bool,
}

/// A step the user takes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StrictStepItem {
  AskReady,
  TurnOn,
  TurnOff,
  RemoveBlocker { user_id: String, device_id: String },
  Continue { maybe_remove_user_id: Option<String> },
  AcceptRejoin,
}

#[uniffi::export]
impl TeamCraft {
  /// The channel's strict mode; `None` while this app doesn't implement strict channels, for
  /// anything but a private channel and before the core looked. Read it again on a change.
  pub async fn strict_channel(
    &self,
    conversation_id: String,
  ) -> Result<Option<StrictChannelItem>, TeamCraftError> {
    self
      .read(move |reads| reads.strict_channel(&conversation_id))
      .await
  }

  /// Reads the channel's strict mode afresh, at most every few seconds per channel: call it when
  /// the channel's settings open, as desktop and web do.
  pub async fn refresh_strict_channel(&self, conversation_id: String) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| {
        reads
          .backend()?
          .refresh_strict_channel(parse_id(&conversation_id)?);
        Ok(())
      })
      .await
  }

  /// Runs a step and waits for it, so the button can show the failure where it was pressed. The
  /// core decides again; the view changes with a store update either way.
  pub async fn strict_step(
    &self,
    conversation_id: String,
    step: StrictStepItem,
  ) -> Result<(), TeamCraftError> {
    let conversation: ConversationId = parse_id(&conversation_id)?;
    let step = core_step(step)?;
    self
      .call(move |engine| {
        Box::pin(async move {
          engine
            .with_client(move |client| {
              Box::pin(async move { client.run_strict_step(conversation, step).await })
            })
            .await
        })
      })
      .await
  }
}

impl Reads {
  fn strict_channel(&self, conversation_id: &str) -> Result<Option<StrictChannelItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    let Some(channel) = backend.strict_channel(conversation) else {
      return Ok(None);
    };
    let me = backend.me().id;
    let granted = backend
      .conversation(conversation)
      .and_then(|info| info.maybe_permissions)
      .unwrap_or_default();
    // Without the server's answer (an older server) nothing is offered: the server would decide.
    let actor = StrictActor {
      user: me,
      can_manage_members: granted.contains(&Permission::ManageMembers),
      can_manage_channel_roles: granted.contains(&Permission::ManageChannelRoles),
    };
    let gates = StrictGates::new(&channel, actor);
    let name_of = |user: UserId| {
      backend
        .user(user)
        .map(|found| found.display_name)
        .filter(|name| !name.is_empty())
    };
    // The phone reads no space's own scheme: the built-in one, which the server's answer refines
    // on desktop and web.
    let losses = if gates.can_turn_on {
      strict_losses_in(backend, conversation, &channel, &PermissionScheme::default())
    } else {
      Vec::new()
    };
    Ok(Some(item(&channel, gates, me, &name_of, losses)))
  }
}

fn item(
  channel: &StrictChannel,
  gates: StrictGates,
  me: UserId,
  name_of: &impl Fn(UserId) -> Option<String>,
  losses: Vec<teamcraft_client_api::strict_channel::StrictLoss>,
) -> StrictChannelItem {
  StrictChannelItem {
    is_strict: channel.is_strict,
    roles: channel
      .roles
      .iter()
      .map(|holder| StrictPersonItem {
        user_id: holder.user.to_string(),
        maybe_name: name_of(holder.user),
        is_admin: holder.role == StrictRole::Admin,
      })
      .collect(),
    losses: losses
      .into_iter()
      .map(|loss| StrictLossItem {
        user_id: loss.user.to_string(),
        maybe_name: name_of(loss.user),
        loss: match loss.loss {
          StrictLossKind::ChangingMembers => StrictLossKindItem::ChangingMembers,
          StrictLossKind::AddingMembers => StrictLossKindItem::AddingMembers,
        },
      })
      .collect(),
    has_admin: channel
      .roles
      .iter()
      .any(|holder| holder.role == StrictRole::Admin),
    maybe_readiness: channel
      .maybe_readiness
      .as_ref()
      .filter(|_| !channel.is_strict)
      .map(|readiness| StrictReadinessItem {
        has_asked: readiness.maybe_asked_by.is_some(),
        waiting: readiness
          .waiting
          .iter()
          .map(|device| StrictDeviceItem {
            user_id: device.user.to_string(),
            device_id: device.device.to_string(),
            maybe_name: name_of(device.user),
          })
          .collect(),
        refreshing_count: u32::try_from(readiness.refreshing.len()).unwrap_or(u32::MAX),
        is_ready: readiness.is_ready,
      }),
    maybe_stop: channel.maybe_stop.map(|stop| {
      // Never "Remove yourself and continue".
      let maybe_signer = stop.maybe_signer.filter(|signer| *signer != me);
      StrictStopItem {
        maybe_signer_id: maybe_signer.map(|signer| signer.to_string()),
        maybe_signer_name: maybe_signer.and_then(name_of),
      }
    }),
    maybe_rejoin: channel.maybe_rejoin.as_ref().map(|rejoin| StrictRejoinItem {
      maybe_sender_name: name_of(rejoin.sender),
      joined_names: rejoin.joined.iter().filter_map(|user| name_of(*user)).collect(),
      gone_names: rejoin.gone.iter().filter_map(|user| name_of(*user)).collect(),
      is_due: rejoin.is_due,
      is_accepted: rejoin.is_accepted,
    }),
    can_turn_on: gates.can_turn_on,
    can_turn_off: gates.can_turn_off,
    can_remove_blocker: gates.can_remove_blocker,
    can_continue: gates.can_continue,
  }
}

fn core_step(step: StrictStepItem) -> Result<StrictStep, TeamCraftError> {
  Ok(match step {
    StrictStepItem::AskReady => StrictStep::AskReady,
    StrictStepItem::TurnOn => StrictStep::TurnOn,
    StrictStepItem::TurnOff => StrictStep::TurnOff,
    StrictStepItem::RemoveBlocker { user_id, device_id } => StrictStep::RemoveBlocker {
      user: parse_id::<UserId>(&user_id)?,
      device: parse_id::<DeviceId>(&device_id)?,
    },
    StrictStepItem::Continue { maybe_remove_user_id } => StrictStep::Continue {
      maybe_remove: maybe_remove_user_id
        .map(|user| parse_id::<UserId>(&user))
        .transpose()?,
    },
    StrictStepItem::AcceptRejoin => StrictStep::AcceptRejoin,
  })
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::strict_channel::{StrictRoleHolder, StrictStop};
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn the_stop_never_offers_removing_the_user() {
    let me: UserId = id(1);
    let channel = StrictChannel {
      has_group: true,
      is_strict: true,
      roles: vec![StrictRoleHolder {
        user: me,
        role: StrictRole::Admin,
      }],
      maybe_stop: Some(StrictStop {
        at_ms: 1,
        maybe_signer: Some(me),
      }),
      ..StrictChannel::default()
    };
    let gates = StrictGates::new(
      &channel,
      StrictActor {
        user: me,
        can_manage_members: true,
        can_manage_channel_roles: true,
      },
    );
    let shown = item(&channel, gates, me, &|_| Some("Alice".into()), Vec::new());
    let stop = shown.maybe_stop.unwrap();
    assert_eq!(stop.maybe_signer_id, None);
    assert!(shown.can_continue && shown.has_admin);
  }

  #[test]
  fn steps_parse_their_ids_and_refuse_garbage() {
    assert!(
      core_step(StrictStepItem::Continue {
        maybe_remove_user_id: Some("nope".into())
      })
      .is_err()
    );
    assert_eq!(
      core_step(StrictStepItem::Continue {
        maybe_remove_user_id: None
      })
      .unwrap(),
      StrictStep::Continue { maybe_remove: None }
    );
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
