//! Managing spaces and channels through the core: creating and editing them, the channel browser
//! and joining, categories, invitations, and announcing changes as encrypted system messages.

use teamcraft_crypto::membership::{HistoryShare, MembershipAction};
use teamcraft_proto::{
  CategoryId, ConversationId, ConversationResponse, ConversationVisibility, CreateCategoryRequest,
  CreateChannelRequest, CreateInviteRequest, CreateSpaceRequest, HistoryPolicy, ListCategoriesResponse,
  ListConversationsResponse, SpaceId, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest,
  UpdateConversationRequest, UpdateSpaceRequest,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, Table};

/// A new channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOptions {
  pub name: String,
  pub is_private: bool,
  pub maybe_topic: Option<String>,
  pub maybe_category: Option<CategoryId>,
  pub maybe_history: Option<HistoryPolicy>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub async fn create_space(&mut self, request: &CreateSpaceRequest) -> Result<SpaceResponse, CoreError> {
    let space = self.api.create_space(request).await?;
    self.refresh_spaces().await?;
    Ok(space)
  }

  pub async fn update_space(
    &mut self,
    space: SpaceId,
    request: &UpdateSpaceRequest,
  ) -> Result<SpaceResponse, CoreError> {
    let updated = self.api.update_space(space, request).await?;
    self.refresh_spaces().await?;
    Ok(updated)
  }

  /// Founds a channel (and its encryption group) with the options of the "new channel" dialog.
  pub async fn create_channel_with(
    &mut self,
    space: SpaceId,
    options: ChannelOptions,
  ) -> Result<ConversationId, CoreError> {
    let request = CreateChannelRequest {
      name: options.name,
      visibility: if options.is_private {
        ConversationVisibility::Private
      } else {
        ConversationVisibility::Public
      },
      maybe_category_id: options.maybe_category,
      maybe_history_policy: options.maybe_history,
      maybe_topic: options.maybe_topic,
      maybe_description: None,
      is_integration: false,
    };
    self.create_channel_from(space, &request).await
  }

  /// Renames, converts, archives or recategorizes a channel and announces what changed.
  pub async fn update_channel(
    &mut self,
    conversation: ConversationId,
    request: &UpdateConversationRequest,
  ) -> Result<(), CoreError> {
    let maybe_before: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    let after = self.api.update_conversation(conversation, request).await?;
    records::put_json(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
      &after,
    )?;
    let Some(before) = maybe_before else {
      return Ok(());
    };
    for event in changes(&before, &after) {
      self.announce(conversation, event).await?;
    }
    Ok(())
  }

  /// Public channels of a space the user hasn't joined, kept for the channel browser.
  pub async fn refresh_channel_browser(
    &mut self,
    space: SpaceId,
  ) -> Result<ListConversationsResponse, CoreError> {
    let joinable = self.api.list_joinable(space).await?;
    records::put_json(&self.store, Table::ChannelBrowser, &space.to_string(), &joinable)?;
    Ok(joinable)
  }

  /// Joins a public channel: the server lets the user in, and a member adds this user's devices to
  /// the encryption group from the signed self-add this device posts (e2ee.md §4.6).
  pub async fn join_channel(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.api.join_conversation(conversation).await?;
    let response = self.api.conversation(conversation).await?;
    records::put_json(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
      &response,
    )?;
    // Nobody chose to show the past to somebody joining on their own: history from the join on
    // (e2ee.md §4.7). Members' clients refuse a self-add that says otherwise.
    let history = HistoryShare::FromJoin;
    self
      .post_admin_change(
        conversation,
        MembershipAction::AddUser {
          user_id: convert::user(self.account.user_id),
          history,
        },
      )
      .await?;
    // Sent once this device has the conversation's keys.
    self.enqueue(
      conversation,
      None,
      MessageContent::Event {
        event: ConversationEvent::Joined,
      },
    )?;
    if let Some(space) = response.maybe_space_id {
      self.refresh_channel_browser(space).await?;
    }
    Ok(())
  }

  /// Says goodbye, then leaves; the remaining members remove this user's devices.
  pub async fn leave_channel(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.announce(conversation, ConversationEvent::Left).await?;
    self.api.remove_member(conversation, self.account.user_id).await?;
    Ok(())
  }

  pub async fn refresh_categories(&mut self, space: SpaceId) -> Result<ListCategoriesResponse, CoreError> {
    let categories = self.api.list_categories(space).await?;
    records::put_json(&self.store, Table::Categories, &space.to_string(), &categories)?;
    Ok(categories)
  }

  pub async fn create_category(&mut self, space: SpaceId, name: &str) -> Result<(), CoreError> {
    self
      .api
      .create_category(
        space,
        &CreateCategoryRequest {
          name: name.to_string(),
          maybe_position: None,
        },
      )
      .await?;
    self.refresh_categories(space).await.map(|_| ())
  }

  pub async fn rename_category(
    &mut self,
    space: SpaceId,
    category: CategoryId,
    name: &str,
  ) -> Result<(), CoreError> {
    let request = UpdateCategoryRequest {
      maybe_name: Some(name.to_string()),
      maybe_position: None,
    };
    self.api.update_category(space, category, &request).await?;
    self.refresh_categories(space).await.map(|_| ())
  }

  pub async fn delete_category(&mut self, space: SpaceId, category: CategoryId) -> Result<(), CoreError> {
    self.api.delete_category(space, category).await?;
    self.refresh_categories(space).await.map(|_| ())
  }

  /// Invites someone by email to a space, as member or guest, optionally into some channels.
  pub async fn invite_to_space(
    &mut self,
    space: SpaceId,
    email: &str,
    is_guest: bool,
    channels: Vec<ConversationId>,
    maybe_expires_at: Option<i64>,
  ) -> Result<(), CoreError> {
    let request = CreateInviteRequest {
      email: email.to_string(),
      role: if is_guest {
        SpaceRole::Guest
      } else {
        SpaceRole::Member
      },
      channel_ids: channels,
      maybe_expires_at: maybe_expires_at.map(UnixMillis),
    };
    self.api.create_invite(space, &request).await?;
    Ok(())
  }

  /// Sends a system message about a change, encrypted like any message.
  pub(crate) async fn announce(
    &mut self,
    conversation: ConversationId,
    event: ConversationEvent,
  ) -> Result<(), CoreError> {
    self.enqueue(conversation, None, MessageContent::Event { event })?;
    self.flush_outbox().await.map(|_| ())
  }
}

/// The announcements a channel update calls for.
fn changes(before: &ConversationResponse, after: &ConversationResponse) -> Vec<ConversationEvent> {
  let mut events = Vec::new();
  if before.maybe_name != after.maybe_name
    && let Some(name) = &after.maybe_name
  {
    events.push(ConversationEvent::Renamed { name: name.clone() });
  }
  if before.maybe_topic != after.maybe_topic {
    events.push(ConversationEvent::TopicChanged {
      topic: after.maybe_topic.clone().unwrap_or_default(),
    });
  }
  if before.is_archived != after.is_archived {
    events.push(if after.is_archived {
      ConversationEvent::Archived
    } else {
      ConversationEvent::Unarchived
    });
  }
  if before.visibility != after.visibility {
    events.push(match after.visibility {
      ConversationVisibility::Public => ConversationEvent::MadePublic,
      _ => ConversationEvent::MadePrivate,
    });
  }
  events
}
