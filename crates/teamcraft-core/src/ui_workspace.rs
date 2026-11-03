//! Spaces and channels in the UI adapter: the dialogs' requests become engine commands; the channel
//! browser and categories are read from the store and refreshed on first look.

use teamcraft_client_api::actions::{
  BrowsableChannel, BrowseJoin, CategoryChoice, CategoryInfo, ChannelChanges, NewChannel, NewSpace,
  SpaceChanges,
};
use teamcraft_client_api::model::{ConversationId, SpaceId};
use teamcraft_proto::{
  AdminSelfJoin, ConversationKind, ConversationResponse, ConversationVisibility, CreateSpaceRequest,
  JoinPolicy, ListCategoriesResponse, ListConversationsResponse, MoveChannelRequest,
  UpdateConversationRequest, UpdateSpaceRequest,
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
        maybe_idempotency_key: space.maybe_idempotency_key,
      }),
    });
  }

  pub(crate) fn ui_update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.command(CoreCommand::UpdateSpace {
      space,
      request: Box::new(UpdateSpaceRequest {
        maybe_name: changes.maybe_name,
        maybe_icon: None,
        maybe_color: changes.maybe_color,
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
        is_integration: channel.is_integration,
        maybe_idempotency_key: channel.maybe_idempotency_key,
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
        maybe_is_integration: changes.maybe_is_integration,
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
    // Private channels come only to admins (the server's word); whether and how they may join
    // follows the policy this device verified (admin.md §2.4).
    let workspace: crate::workspace::WorkspaceState = self.read(
      records::get_json(self.store_ref(), Table::Workspace, crate::client::WORKSPACE_KEY)
        .map(Option::unwrap_or_default),
    );
    let self_join = workspace.policy.admin_self_join;
    let asked = self.my_join_requests();
    let joinable = maybe_joinable
      .map(|list| list.conversations)
      .unwrap_or_default()
      .into_iter()
      .filter_map(move |conversation| {
        let join = match browse_join(conversation.visibility.clone(), &self_join)? {
          BrowseJoin::NeedsApproval if asked.contains(&conversation.id) => BrowseJoin::Requested,
          join => join,
        };
        Some(BrowsableChannel {
          join,
          ..browsable(&conversation, space, false)
        })
      });
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
    maybe_category: conversation.maybe_category_id,
    join: BrowseJoin::Open,
  }
}

/// How a channel the user isn't in can be joined; `None` hides it (a private channel the policy
/// keeps admins out of).
fn browse_join(visibility: ConversationVisibility, self_join: &AdminSelfJoin) -> Option<BrowseJoin> {
  match visibility {
    ConversationVisibility::Public => Some(BrowseJoin::Open),
    ConversationVisibility::Private | ConversationVisibility::Unknown(_) => match self_join {
      AdminSelfJoin::Visible => Some(BrowseJoin::AsAdmin),
      AdminSelfJoin::OwnerApproval => Some(BrowseJoin::NeedsApproval),
      AdminSelfJoin::Disabled | AdminSelfJoin::Unknown(_) => None,
    },
  }
}

fn join_policy(is_open: bool) -> JoinPolicy {
  if is_open {
    JoinPolicy::Open
  } else {
    JoinPolicy::InviteOnly
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// admin.md §2.4: private channels in the browser follow the verified `admin_self_join`.
  #[test]
  fn private_channels_are_offered_as_the_signed_policy_says() {
    let private = || ConversationVisibility::Private;
    assert_eq!(
      browse_join(private(), &AdminSelfJoin::Visible),
      Some(BrowseJoin::AsAdmin)
    );
    assert_eq!(
      browse_join(private(), &AdminSelfJoin::OwnerApproval),
      Some(BrowseJoin::NeedsApproval)
    );
    assert_eq!(browse_join(private(), &AdminSelfJoin::Disabled), None);
    assert_eq!(
      browse_join(private(), &AdminSelfJoin::Unknown("later".into())),
      None
    );
    for policy in [AdminSelfJoin::Visible, AdminSelfJoin::Disabled] {
      assert_eq!(
        browse_join(ConversationVisibility::Public, &policy),
        Some(BrowseJoin::Open)
      );
    }
  }
}
