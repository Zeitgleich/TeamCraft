//! Managing spaces and channels through the core: creating and editing them, the channel browser
//! and joining, categories, invitations, and announcing changes as encrypted system messages.

use teamcraft_crypto::membership::{HistoryShare, MembershipAction};
use teamcraft_proto::{
  CategoryId, ConversationId, ConversationKind, ConversationResponse, ConversationVisibility,
  CreateCategoryRequest, CreateChannelRequest, CreateInviteRequest, CreateSpaceRequest, HistoryPolicy,
  IdempotencyKey, ListCategoriesResponse, ListConversationsResponse, MoveChannelRequest, SpaceId,
  SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest, UpdateConversationRequest, UpdateSpaceRequest,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::client::groups::CREATE_ATTEMPTS;
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
  /// Makes a retry of the creation return the same channel.
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  pub async fn create_space(&mut self, request: &CreateSpaceRequest) -> Result<SpaceResponse, CoreError> {
    let mut attempt = 1;
    // Same retry as for channels: the request keeps its idempotency key.
    let space = loop {
      match self.api.create_space(request).await {
        Err(ApiError::Network(_)) if request.maybe_idempotency_key.is_some() && attempt < CREATE_ATTEMPTS => {
          attempt += 1;
        },
        other => break other?,
      }
    };
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
      maybe_idempotency_key: options.maybe_idempotency_key,
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

  /// Moves a channel to another space and announces it. The encryption group stays as it is.
  pub async fn move_channel(
    &mut self,
    conversation: ConversationId,
    request: &MoveChannelRequest,
  ) -> Result<(), CoreError> {
    let after = self.api.move_conversation(conversation, request).await?;
    records::put_json(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
      &after,
    )?;
    let maybe_space: Option<SpaceResponse> =
      records::get_json(&self.store, Table::Spaces, &request.space_id.to_string())?;
    self
      .announce(
        conversation,
        ConversationEvent::Moved {
          space: maybe_space.map(|space| space.name).unwrap_or_default(),
        },
      )
      .await
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
  /// Joins an open space; the server may also add the user to its default channels.
  pub async fn join_space(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let response = self.api.join_space(space).await?;
    records::put_json(&self.store, Table::Spaces, &space.to_string(), &response)?;
    // Picks up the space's default channels and their encryption groups.
    self.full_sync().await
  }

  pub async fn join_channel(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.api.join_conversation(conversation).await?;
    let response = self.api.conversation(conversation).await?;
    records::put_json(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
      &response,
    )?;
    self.post_self_add(&response).await
  }

  /// Channels the server put this user into (the instance's default channels, an invitation) where
  /// no member has added this device to the encryption group: posts the same signed self-add as
  /// [`Self::join_channel`], once per channel. Returns how many were requested.
  pub async fn join_missing_groups(&mut self) -> Result<usize, CoreError> {
    let mut requested = 0;
    let mut maybe_first_error = None;
    for response in self.cached_conversations()?.into_values() {
      if response.maybe_my_membership.is_some()
        && !self.has_group(response.id)?
        && self.has_requested_rejoin(response.id)?
      {
        // A welcome this device opened but couldn't join: it asked to be added back, and asks
        // again at a growing interval (also when the first request never went out). One
        // conversation failing doesn't keep the others from asking.
        if let Err(error) = self.request_rejoin(response.id, 0).await {
          tracing::warn!(conversation_id = %response.id, %error, "could not ask to be added back");
          maybe_first_error.get_or_insert(error);
        }
        continue;
      }
      let is_channel = response.maybe_space_id.is_some() && response.kind == ConversationKind::Channel;
      let key = format!("self-add/{}", response.id);
      if !is_channel
        || response.maybe_my_membership.is_none()
        || response.is_archived
        || self.has_group(response.id)?
        || records::get_json::<bool>(&self.store, Table::SyncState, &key)?.is_some()
      {
        continue;
      }
      // A further device of a user who is in the group already joins as that, unannounced
      // (e2ee.md §4.6); only a user who isn't signs a self-add.
      match self.join_as_member_device(response.id).await {
        Ok(super::member_device::MemberDeviceJoin::NotAMember) => {},
        Ok(_) => continue,
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(conversation_id = %response.id, %error, "could not join as a further device");
          continue;
        },
      }
      // Marked first: a failed attempt is not repeated on every sync, and a member's own add wins.
      records::put_json(&self.store, Table::SyncState, &key, &true)?;
      self.post_self_add(&response).await?;
      requested += 1;
    }
    maybe_first_error.map_or(Ok(requested), Err)
  }

  async fn post_self_add(&mut self, response: &ConversationResponse) -> Result<(), CoreError> {
    let conversation = response.id;
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
