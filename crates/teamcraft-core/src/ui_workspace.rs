//! Spaces and channels in the UI adapter: the dialogs' requests become engine commands; the channel
//! browser and categories are read from the store and refreshed on first look.

use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryChoice, CategoryInfo, ChannelChanges, NewChannel, NewSpace, SpaceChanges,
};
use teamcraft_client_api::model::{ConversationId, SpaceId};
use teamcraft_proto::{
  ConversationKind, ConversationResponse, ConversationVisibility, CreateSpaceRequest, JoinPolicy,
  ListCategoriesResponse, ListConversationsResponse, MoveChannelRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};

use crate::client::workspace_admin::ChannelOptions;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};

impl<S: LocalStore> CoreChatBackend<S> {
  pub(crate) fn ui_create_space(&self, space: NewSpace) {
    self.command(CoreCommand::CreateSpace {
      request: Box::new(CreateSpaceRequest {
        name: space.name,
        maybe_icon: None,
        maybe_color: None,
        maybe_description: space.maybe_description,
        join_policy: join_policy(space.is_open),
      }),
    });
  }

  pub(crate) fn ui_update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.command(CoreCommand::UpdateSpace {
      space,
      request: Box::new(UpdateSpaceRequest {
        maybe_name: changes.maybe_name,
        maybe_icon: None,
        maybe_color: None,
        maybe_description: changes.maybe_description,
        maybe_join_policy: changes.maybe_is_open.map(join_policy),
        maybe_is_archived: changes.maybe_is_archived,
        maybe_brand_id: changes.maybe_brand_id,
      }),
    });
  }

  pub(crate) fn ui_create_channel_with(&self, channel: NewChannel) {
    self.command(CoreCommand::CreateChannelWith {
      space: channel.space,
      options: Box::new(ChannelOptions {
        name: channel.name,
        is_private: channel.is_private,
        maybe_topic: channel.maybe_topic,
        maybe_category: channel.maybe_category,
        maybe_history: channel.maybe_history,
      }),
    });
  }

  pub(crate) fn ui_update_channel(&self, conversation: ConversationId, changes: ChannelChanges) {
    if let Some(space) = changes.maybe_space {
      let maybe_category_id = match changes.maybe_category {
        Some(CategoryChoice::Category { category }) => Some(category),
        Some(CategoryChoice::Uncategorized) | None => None,
      };
      self.command(CoreCommand::MoveChannel {
        conversation,
        request: MoveChannelRequest {
          space_id: space,
          maybe_category_id,
        },
      });
      return;
    }
    let (maybe_category_id, clear_category) = match changes.maybe_category {
      Some(CategoryChoice::Category { category }) => (Some(category), false),
      Some(CategoryChoice::Uncategorized) => (None, true),
      None => (None, false),
    };
    self.command(CoreCommand::UpdateChannel {
      conversation,
      request: Box::new(UpdateConversationRequest {
        maybe_name: changes.maybe_name,
        maybe_topic: changes.maybe_topic,
        maybe_description: changes.maybe_description,
        maybe_visibility: changes.maybe_is_private.map(|is_private| {
          if is_private {
            ConversationVisibility::Private
          } else {
            ConversationVisibility::Public
          }
        }),
        maybe_is_archived: changes.maybe_is_archived,
        maybe_category_id,
        clear_category,
        ..UpdateConversationRequest::default()
      }),
    });
  }

  /// Channels of the space the user is in, then the public ones they could join.
  pub(crate) fn ui_browse_channels(&self, space: SpaceId) -> Vec<BrowsableChannel> {
    let maybe_joinable: Option<ListConversationsResponse> = self.read(records::get_json(
      self.store_ref(),
      Table::ChannelBrowser,
      &space.to_string(),
    ));
    if maybe_joinable.is_none() {
      self.command(CoreCommand::RefreshChannelBrowser { space });
    }
    let joined = self
      .conversations_raw()
      .into_iter()
      .filter(|conversation| {
        conversation.maybe_space_id == Some(space) && conversation.kind == ConversationKind::Channel
      })
      .map(|conversation| browsable(&conversation, space, true));
    let joinable = maybe_joinable
      .map(|list| list.conversations)
      .unwrap_or_default()
      .into_iter()
      .map(|conversation| browsable(&conversation, space, false));
    let mut channels: Vec<BrowsableChannel> = joined.chain(joinable).collect();
    channels.sort_by_key(|channel| channel.name.to_lowercase());
    channels.dedup_by_key(|channel| channel.conversation);
    channels
  }

  pub(crate) fn ui_categories(&self, space: SpaceId) -> Vec<CategoryInfo> {
    let maybe_categories: Option<ListCategoriesResponse> = self.read(records::get_json(
      self.store_ref(),
      Table::Categories,
      &space.to_string(),
    ));
    match maybe_categories {
      Some(list) => list
        .categories
        .into_iter()
        .map(|category| CategoryInfo {
          id: category.id,
          space: category.space_id,
          name: category.name,
          position: category.position,
        })
        .collect(),
      None => {
        self.command(CoreCommand::RefreshCategories { space });
        Vec::new()
      },
    }
  }
}

fn browsable(conversation: &ConversationResponse, space: SpaceId, is_member: bool) -> BrowsableChannel {
  BrowsableChannel {
    conversation: conversation.id,
    space,
    name: conversation.maybe_name.clone().unwrap_or_default(),
    topic: conversation.maybe_topic.clone().unwrap_or_default(),
    is_member,
    is_archived: conversation.is_archived,
    has_external_people: conversation.contains_guests,
  }
}

fn join_policy(is_open: bool) -> JoinPolicy {
  if is_open {
    JoinPolicy::Open
  } else {
    JoinPolicy::InviteOnly
  }
}
