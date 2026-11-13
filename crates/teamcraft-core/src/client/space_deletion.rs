//! Deleting a space on the deleter's device (e2ee.md §4.6.5, D7, issue #1727). Before the user
//! confirms, the device lists the space's channels as the server shows them, each with who last
//! set its name as the channel's anchor on this device says ([`Client::prepare_space_deletion`]);
//! the confirmation names them. A workspace owner or admin then signs exactly those channels with
//! their epochs and name hashes; the members' devices, the deleter's own included, purge a channel
//! at once only where its anchor places it in the space (`crate::signed_deletion`). Anybody else
//! (a space admin, a custom scheme) deletes unsigned, and every channel stays a claim.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::channel_anchor::{NameAuthority, name_hash};
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, DeleteSpaceRequest, ErrorCode, SignedSpaceDeletion,
  SpaceDeletionChannel, SpaceId, UserId,
};

use super::Client;
use super::channel_anchor::HeldAnchor;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::signed_deletion::{is_told_epoch_plausible, sign_space_deletion};
use crate::store::{LocalStore, Table};
use crate::workspace::RosterRole;

const PREVIEW_PREFIX: &str = "space_deletion_preview/";

/// What deleting a space takes, as this device prepared it: the channels its confirmation names,
/// each with who set its current name, and whether the deletion will be signed (only a workspace
/// owner's or admin's is). The device signs exactly these channels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDeletionPreview {
  pub space: SpaceId,
  pub channels: Vec<SpaceDeletionPreviewChannel>,
  pub is_signed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDeletionPreviewChannel {
  pub conversation: ConversationId,
  /// The name the server shows; what the signature binds.
  pub name: String,
  /// From the channel's verified anchor here; `None` without one.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_renamed_by: Option<ChannelRenamer>,
}

/// Who set a channel's current name, and in which role (the anchor's `name_set_by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRenamer {
  pub user: UserId,
  pub role: RenamerRole,
}

/// A member's name purges nothing at once (D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenamerRole {
  Founder,
  ChannelAdmin,
  WorkspaceAdmin,
  Member,
}
/// Tries with fresh epochs: a commit in one of the channels may come in between.
const DELETION_ATTEMPTS: usize = 3;
/// How long a prepared list stands for what the user was shown.
const PREVIEW_MAX_AGE_MS: i64 = 60 * 60 * 1000;

/// What the device prepared: the preview the UI shows, and per channel the epoch to sign, `None`
/// where the server's epoch is far from this device's own group (that channel isn't signed).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PreparedSpaceDeletion {
  preview: SpaceDeletionPreview,
  epochs: Vec<(ConversationId, Option<u64>)>,
  prepared_at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Lists what deleting `space` takes for its confirmation, kept for the UI
  /// ([`space_deletion_preview`]) and for the signature.
  pub async fn prepare_space_deletion(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let prepared = self.make_space_deletion(space).await?;
    records::put_json(&self.store, Table::SyncState, &preview_key(space), &prepared)
  }

  /// Deletes a space with its channels for everybody (e2ee.md §4.6). Done also when it is gone
  /// already. Signed if this user is a workspace owner or admin, over the channels the confirmation
  /// named (or, without one, the server's list now); each channel then goes on this device as on
  /// every member's ([`Self::note_space_deletion`], #1162).
  pub async fn delete_space(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let now_ms = self.now_ms();
    let maybe_shown: Option<PreparedSpaceDeletion> =
      records::get_json(&self.store, Table::SyncState, &preview_key(space))?.filter(
        |prepared: &PreparedSpaceDeletion| {
          now_ms.saturating_sub(prepared.prepared_at_ms) <= PREVIEW_MAX_AGE_MS
        },
      );
    let mut prepared = match &maybe_shown {
      Some(shown) => shown.clone(),
      None => self.make_space_deletion(space).await?,
    };
    let mut attempts = 1;
    let maybe_used = loop {
      let maybe_signed = self.signed_space_deletion_if_allowed(space, &prepared);
      let request = DeleteSpaceRequest {
        maybe_signed_deletion: maybe_signed.clone(),
      };
      match self.api.delete_space(space, &request).await {
        Ok(()) => break maybe_signed,
        Err(error) if error.error_code() == Some(&ErrorCode::SpaceNotFound) => break None,
        Err(error) if error.is_epoch_conflict() && attempts < DELETION_ATTEMPTS => {
          let fresh = self.make_space_deletion(space).await?;
          prepared = match &maybe_shown {
            Some(shown) => only_shown(fresh, shown),
            None => fresh,
          };
          attempts += 1;
        },
        Err(error) => return Err(error.into()),
      }
    };
    self.note_space_deletion(space, maybe_used.as_ref()).await?;
    self.store.delete(Table::SyncState, &preview_key(space))?;
    self.forget_space(&space.to_string())
  }

  async fn make_space_deletion(&mut self, space: SpaceId) -> Result<PreparedSpaceDeletion, CoreError> {
    if let Err(error) = self.sync_workspace().await {
      tracing::debug!(space_id = %space, %error, "preparing a space's deletion on the roster this device has");
    }
    let is_signed = self.is_workspace_signer();
    let listed: Vec<(ConversationId, String, u64)> = match self.api.space_deletion_preview(space).await {
      Ok(response) => response
        .channels
        .into_iter()
        .map(|channel| (channel.conversation_id, channel.name, channel.epoch))
        .collect(),
      Err(error) => {
        // An older server: the channels this device lists, with its own groups' epochs.
        tracing::debug!(space_id = %space, %error, "no deletion preview from the server; listing the channels known here");
        let mut known = Vec::new();
        for (id, response) in self.cached_conversations()? {
          if response.maybe_space_id == Some(space) && response.kind == ConversationKind::Channel {
            let epoch = self.group_epoch(id)?.unwrap_or_default();
            known.push((id, response.maybe_name.unwrap_or_default(), epoch));
          }
        }
        known
      },
    };
    let mut channels = Vec::with_capacity(listed.len());
    let mut epochs = Vec::with_capacity(listed.len());
    for (conversation, name, told) in listed {
      let maybe_own = self.group_epoch(conversation)?;
      // A member signs nothing far from its own group: the signature would be good until the
      // group got there. That channel stays a claim.
      let maybe_epoch = match maybe_own {
        Some(own) if !is_told_epoch_plausible(own, told) => {
          tracing::warn!(conversation_id = %conversation, told, "the server names an epoch far from this device's group; the channel isn't signed");
          None
        },
        _ => Some(told),
      };
      let maybe_renamed_by = if maybe_own.is_some() {
        self.renamer_of(conversation).await
      } else {
        None
      };
      channels.push(SpaceDeletionPreviewChannel {
        conversation,
        name,
        maybe_renamed_by,
      });
      epochs.push((conversation, maybe_epoch));
    }
    Ok(PreparedSpaceDeletion {
      preview: SpaceDeletionPreview {
        space,
        channels,
        is_signed,
      },
      epochs,
      prepared_at_ms: self.now_ms(),
    })
  }

  /// Who set the channel's current name, from its verified anchor on this device.
  async fn renamer_of(&mut self, conversation: ConversationId) -> Option<ChannelRenamer> {
    match Box::pin(self.held_anchor(conversation)).await {
      Ok(HeldAnchor::Anchored { anchor, .. }) => {
        let setter = anchor.properties().name_set_by;
        Some(ChannelRenamer {
          user: convert::user_from_crypto(setter.user_id).ok()?,
          role: match setter.authority {
            NameAuthority::Founder => RenamerRole::Founder,
            NameAuthority::ChannelAdmin => RenamerRole::ChannelAdmin,
            NameAuthority::Workspace => RenamerRole::WorkspaceAdmin,
            NameAuthority::Member => RenamerRole::Member,
          },
        })
      },
      Ok(HeldAnchor::Unanchored | HeldAnchor::Unverifiable) => None,
      Err(error) => {
        tracing::debug!(conversation_id = %conversation, %error, "could not read the channel's anchor for the confirmation");
        None
      },
    }
  }

  /// Only an owner or admin of the verified roster signs: nobody else's signature counts (D7).
  fn is_workspace_signer(&self) -> bool {
    let me = self.user_id();
    self.workspace().is_ok_and(|workspace| {
      workspace
        .roster
        .members
        .iter()
        .any(|member| member.user_id == me && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    })
  }

  fn signed_space_deletion_if_allowed(
    &self,
    space: SpaceId,
    prepared: &PreparedSpaceDeletion,
  ) -> Option<SignedSpaceDeletion> {
    if !prepared.preview.is_signed || !self.is_workspace_signer() {
      return None;
    }
    let roster_version = self.workspace().ok()?.roster.version;
    let channels = prepared
      .preview
      .channels
      .iter()
      .filter_map(|channel| {
        let epoch = prepared
          .epochs
          .iter()
          .find(|(id, _)| *id == channel.conversation)
          .and_then(|(_, maybe_epoch)| *maybe_epoch)?;
        Some(SpaceDeletionChannel {
          conversation_id: channel.conversation,
          epoch,
          maybe_name_hash: Some(Base64Bytes::new(name_hash(&channel.name).as_bytes().to_vec())),
        })
      })
      .collect();
    match sign_space_deletion(
      &self.account.device,
      &self.account.certificate,
      space,
      roster_version,
      channels,
      self.now_ms(),
    ) {
      Ok(signed) => Some(signed),
      Err(error) => {
        tracing::warn!(space_id = %space, %error, "could not sign a space's deletion; it stays unsigned");
        None
      },
    }
  }
}

/// What deleting a space takes, once [`Client::prepare_space_deletion`] listed it.
pub fn space_deletion_preview<S: LocalStore>(store: &S, space: SpaceId) -> Option<SpaceDeletionPreview> {
  records::get_json::<PreparedSpaceDeletion>(store, Table::SyncState, &preview_key(space))
    .ok()
    .flatten()
    .map(|prepared| prepared.preview)
}

/// A fresh listing cut to the channels the user was shown, under the names shown: the signature
/// binds what the confirmation said, never a name or channel the server put in since.
fn only_shown(fresh: PreparedSpaceDeletion, shown: &PreparedSpaceDeletion) -> PreparedSpaceDeletion {
  let channels: Vec<SpaceDeletionPreviewChannel> = shown
    .preview
    .channels
    .iter()
    .filter(|channel| {
      fresh
        .preview
        .channels
        .iter()
        .any(|now| now.conversation == channel.conversation)
    })
    .cloned()
    .collect();
  let epochs = fresh
    .epochs
    .into_iter()
    .filter(|(id, _)| channels.iter().any(|channel| channel.conversation == *id))
    .collect();
  PreparedSpaceDeletion {
    preview: SpaceDeletionPreview {
      space: shown.preview.space,
      channels,
      is_signed: fresh.preview.is_signed,
    },
    epochs,
    prepared_at_ms: fresh.prepared_at_ms,
  }
}

fn preview_key(space: SpaceId) -> String {
  format!("{PREVIEW_PREFIX}{space}")
}
