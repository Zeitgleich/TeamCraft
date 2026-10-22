//! Full state sync over REST, key package top-up and key bag upload.

use teamcraft_crypto::mls::{create_key_package, create_last_resort_key_package};
use teamcraft_proto::{Base64Bytes, ConversationId, UploadKeyPackagesRequest, UserSummary};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Brings the store up to date after a fresh gateway session (or at start): spaces,
  /// conversations and members, welcomes, MLS backlogs, new messages, then the outbox, key
  /// packages and the key bag.
  pub async fn full_sync(&mut self) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    self.refresh_spaces().await?;
    let conversations = self.api.list_conversations().await?.conversations;
    let known: Vec<ConversationId> = self.cached_conversations()?.into_keys().collect();
    for stale in known
      .iter()
      .filter(|id| !conversations.iter().any(|conversation| conversation.id == **id))
    {
      self.store.delete(Table::Conversations, &stale.to_string())?;
    }
    for conversation in &conversations {
      records::put_json(
        &self.store,
        Table::Conversations,
        &conversation.id.to_string(),
        conversation,
      )?;
      self.refresh_members(conversation.id).await?;
    }
    self.sync_welcomes().await?;
    for conversation in &conversations {
      if self.has_group(conversation.id)? {
        self.sync_mls(conversation.id).await?;
      }
      self.catch_up_messages(conversation.id).await?;
      self.retry_undecrypted(conversation.id)?;
    }
    self.apply_pending_changes().await?;
    self.flush_outbox().await?;
    self.top_up_key_packages().await?;
    if self.sync.is_key_bag_dirty {
      self.upload_key_bag().await?;
    }
    Ok(())
  }

  /// Keeps enough key packages on the server so others can add this device (e2ee.md §4.3), plus a
  /// last-resort one for when they run out.
  pub async fn top_up_key_packages(&mut self) -> Result<(), CoreError> {
    let status = self.api.key_package_status(self.account.device_id).await?;
    let missing = if status.available < self.settings.key_package_low_water {
      self.settings.key_package_target.saturating_sub(status.available)
    } else {
      0
    };
    if missing == 0 && status.has_last_resort {
      return Ok(());
    }
    let mut key_packages = Vec::new();
    for _ in 0..missing {
      key_packages.push(Base64Bytes::new(create_key_package(
        &self.provider,
        &self.account.device,
        &self.account.certificate,
      )?));
    }
    let maybe_last_resort = if status.has_last_resort {
      None
    } else {
      Some(Base64Bytes::new(create_last_resort_key_package(
        &self.provider,
        &self.account.device,
        &self.account.certificate,
      )?))
    };
    // The private parts must be stored before the public parts become claimable.
    records::save_mls_storage(&self.store, &self.provider)?;
    self
      .api
      .upload_key_packages(
        self.account.device_id,
        &UploadKeyPackagesRequest {
          key_packages,
          maybe_last_resort,
        },
      )
      .await?;
    Ok(())
  }

  pub(crate) async fn refresh_spaces(&mut self) -> Result<(), CoreError> {
    for space in self.api.list_spaces().await?.spaces {
      records::put_json(&self.store, Table::Spaces, &space.id.to_string(), &space)?;
    }
    Ok(())
  }

  pub(crate) async fn refresh_conversation(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    match self.api.conversation(conversation).await {
      Ok(response) => {
        records::put_json(
          &self.store,
          Table::Conversations,
          &conversation.to_string(),
          &response,
        )?;
        self.refresh_members(conversation).await
      },
      Err(error) if error.is_not_found() => {
        self
          .store
          .delete(Table::Conversations, &conversation.to_string())?;
        Ok(())
      },
      Err(error) => Err(error.into()),
    }
  }

  async fn refresh_members(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let members = self.api.list_members(conversation).await?;
    for member in &members.members {
      self.remember_user(&member.user)?;
    }
    records::put_json(&self.store, Table::Members, &conversation.to_string(), &members)
  }

  fn remember_user(&self, user: &UserSummary) -> Result<(), CoreError> {
    records::put_json(&self.store, Table::Users, &user.id.to_string(), user)
  }
}
