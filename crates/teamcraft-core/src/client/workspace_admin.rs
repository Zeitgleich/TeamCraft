//! Managing spaces and channels through the core: creating and editing them, the channel browser
//! and joining, categories, invitations, and announcing changes as encrypted system messages.

use teamcraft_proto::{
  AddSpaceMemberRequest, CategoryId, ConversationId, ConversationKind, ConversationResponse,
  ConversationVisibility, CreateCategoryRequest, CreateChannelRequest, CreateInviteRequest,
  CreateSpaceRequest, HistoryPolicy, IdempotencyKey, InviteId, ListCategoriesResponse,
  ListConversationsResponse, ListInvitesResponse, ListSpaceMembersResponse, MoveChannelRequest, SpaceId,
  SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceMemberRequest, UpdateSpaceRequest, UserId,
};

use super::Client;
use super::member_device::GroupInfoOutcome;
use super::self_add::SelfAddState;
use super::server_changes::{Asked, Observed};
use crate::api::{ApiError, ServerApi};
use crate::client::groups::CREATE_ATTEMPTS;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, Table};

/// A space's member list as the store keeps it (`Table::SpaceRosters`).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", content = "roster", rename_all = "snake_case")]
pub(crate) enum StoredRoster {
  Visible(ListSpaceMembersResponse),
  /// The server doesn't show this user the members.
  Hidden,
}

/// A new channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelOptions {
  pub name: String,
  pub is_private: bool,
  pub maybe_topic: Option<String>,
  pub maybe_category: Option<CategoryId>,
  pub maybe_history: Option<HistoryPolicy>,
  /// An integration (log) channel: log view, log retention, push only for mentions (docs/bridge.md).
  pub is_integration: bool,
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
      is_integration: options.is_integration,
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
    // The lines for archiving and converting (issue #818) come from the server's answer; the
    // announcements are still sent, for older versions and to name who made the change.
    let observed = Observed::OwnChange {
      asked: Asked::update(request),
    };
    self.store_conversation_from(maybe_before.as_ref(), &after, observed)?;
    let Some(before) = maybe_before else {
      return Ok(());
    };
    for event in changes(&before, &after, request) {
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
    self.store_conversation(&after, Observed::OwnChange { asked: Asked::MOVE })?;
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
    self.store_conversation(&response, Observed::FromServer { maybe_head: None })?;
    self.post_self_add(&response).await
  }

  /// Channels the server put this user into (the instance's default channels, an invitation) where
  /// no member has added this device to the encryption group: posts the same signed self-add as
  /// [`Self::join_channel`], once per channel. Returns how many were requested.
  pub async fn join_missing_groups(&mut self) -> Result<usize, CoreError> {
    let mut requested = 0;
    // DMs this device created whose group setup was cut short (issue #228).
    let mut maybe_first_error = self.repair_dm_setups().await.err();
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
      let is_direct = matches!(
        response.kind,
        ConversationKind::Dm | ConversationKind::Group | ConversationKind::Meeting
      );
      if is_direct {
        // A further device joins the user's DMs, group DMs and meeting chats on its own too
        // (#179); one this device set up is finished by its setup (`repair_dm_setups`), and
        // nobody self-adds.
        if response.kind == ConversationKind::Meeting
          && response.maybe_my_membership.is_some()
          && !response.is_archived
          && self.mls_epoch(response.id)? == Some(0)
          && !self.has_published_meeting_group_info(response.id)?
        {
          // A group of one (an instant meeting) carries no commit with its information yet; the
          // organizer's other devices need it to join. Asked for once: after the server has it,
          // a meeting nobody joins costs no request per sync (#494).
          match self.publish_group_info_if_missing(response.id).await {
            Ok(GroupInfoOutcome::OnServer) => self.note_meeting_group_info_published(response.id)?,
            Ok(GroupInfoOutcome::Superseded) => {
              // Only the organizer's setup founds anew if that group can't be joined.
              if let Err(error) = self.rejoin_superseded_group(response.id).await {
                tracing::debug!(conversation_id = %response.id, %error, "could not join the meeting chat's first group");
              }
            },
            Ok(GroupInfoOutcome::NotPublished) => {},
            Err(error) => {
              tracing::debug!(conversation_id = %response.id, %error, "could not publish a meeting chat's group information");
            },
          }
        }
        if response.maybe_my_membership.is_none()
          || response.is_archived
          || self.has_group(response.id)?
          || self.has_dm_setup(response.id)?
        {
          continue;
        }
        match self.join_as_member_device(response.id).await {
          Err(error) if error.is_transient() => return Err(error),
          Err(error) => {
            tracing::warn!(conversation_id = %response.id, %error, "could not join a DM as a further device");
          },
          Ok(_) => {},
        }
        continue;
      }
      let is_channel = response.maybe_space_id.is_some() && response.kind == ConversationKind::Channel;
      if !is_channel || response.maybe_my_membership.is_none() || response.is_archived {
        continue;
      }
      if self.has_group(response.id)? {
        self.forget_self_add(response.id)?;
        continue;
      }
      match self.self_add_state(response.id)? {
        // `retry_due_self_adds` posts it again when no member let this device in.
        SelfAddState::Noted => continue,
        // An earlier version posted it once and never again (issue #907): due now, without a
        // second "joined" line.
        SelfAddState::LegacyMark => {
          self.post_self_add_change(response.id).await?;
          requested += 1;
          continue;
        },
        SelfAddState::None => {},
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
      self.post_self_add(&response).await?;
      requested += 1;
    }
    // Self-adds no member answered (refused, or never committed) are posted again when due.
    match self.retry_due_self_adds().await {
      Ok(posted) => requested += posted,
      Err(error) => {
        tracing::warn!(%error, "could not post a self-add again");
        maybe_first_error.get_or_insert(error);
      },
    }
    maybe_first_error.map_or(Ok(requested), Err)
  }

  async fn post_self_add(&mut self, response: &ConversationResponse) -> Result<(), CoreError> {
    let conversation = response.id;
    self.post_self_add_change(conversation).await?;
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
    maybe_message: Option<String>,
  ) -> Result<(), CoreError> {
    let request = CreateInviteRequest {
      email: email.to_string(),
      role: space_role(is_guest),
      channel_ids: channels,
      maybe_expires_at: maybe_expires_at.map(UnixMillis),
      maybe_message,
    };
    self.api.create_invite(space, &request).await?;
    self.refresh_space_invites(space).await
  }

  /// The space's members with their roles, kept for the space settings. Guests and people outside
  /// the space may not list them: that is kept as hidden, not an error.
  pub async fn refresh_space_roster(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let stored = match self.api.list_space_members(space).await {
      Ok(roster) => StoredRoster::Visible(roster),
      Err(ApiError::Status {
        status: 403 | 404, ..
      }) => StoredRoster::Hidden,
      Err(error) => return Err(error.into()),
    };
    records::put_json(&self.store, Table::SpaceRosters, &space.to_string(), &stored)?;
    Ok(())
  }

  /// The space's pending invitations. Only member managers may list them; for everybody else the
  /// list is empty rather than an error.
  pub async fn refresh_space_invites(&mut self, space: SpaceId) -> Result<(), CoreError> {
    let invites = match self.api.list_invites(space).await {
      Ok(invites) => invites,
      Err(ApiError::Status {
        status: 403 | 404, ..
      }) => ListInvitesResponse { invites: Vec::new() },
      Err(error) => return Err(error.into()),
    };
    records::put_json(&self.store, Table::SpaceInvites, &space.to_string(), &invites)?;
    Ok(())
  }

  /// Changes a member's role in the space (member, guest, space admin).
  pub async fn set_space_role(
    &mut self,
    space: SpaceId,
    user: UserId,
    role: SpaceRole,
  ) -> Result<(), CoreError> {
    let request = UpdateSpaceMemberRequest {
      role,
      maybe_expires_at: None,
      clears_expiry: false,
    };
    self.api.update_space_member(space, user, &request).await?;
    self.refresh_space_roster(space).await
  }

  /// Gives a channel member another role (admin.md §2.3). The server checks the permission and
  /// the invariants (a channel keeps an admin, guests stay members) first; then this device signs
  /// the channel's new roles, so members don't take the server's word for them (e2ee.md §4.6.1).
  /// Somebody who can't sign in a way members accept (a role that is the server's word only, a
  /// space admin) still changes the server's record: members see that role as not signed.
  pub async fn set_channel_role(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_proto::ChannelRole,
  ) -> Result<crate::client::RoleSigning, CoreError> {
    let request = teamcraft_proto::UpdateConversationMemberRequest {
      maybe_role: Some(role.clone()),
      ..Default::default()
    };
    self.api.update_member(conversation, user, &request).await?;
    self.refresh_members(conversation).await?;
    self.sign_server_role(conversation, user, role).await
  }

  /// Signs the role the server lists for `user` (an admin confirming a role that is the server's
  /// word only, e.g. from before signed roles). Nothing changes on the server.
  pub async fn confirm_channel_role(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<crate::client::RoleSigning, CoreError> {
    self.refresh_members(conversation).await?;
    let role = self
      .server_channel_role(conversation, user)?
      .ok_or_else(|| CoreError::InvalidRequest(format!("user {user} is not a member")))?;
    self.sign_server_role(conversation, user, role).await
  }

  /// Signs `user`'s new role. A channel nothing was signed for yet is founded by an admin the
  /// server lists (its first admin who changes a role); its other roles stay unsigned until an
  /// admin confirms them, so nobody vouches for the server's list unseen.
  async fn sign_server_role(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_proto::ChannelRole,
  ) -> Result<crate::client::RoleSigning, CoreError> {
    let me = self.account.user_id;
    let may_found = self.server_channel_role(conversation, me)? == Some(teamcraft_proto::ChannelRole::Admin);
    let signing = self
      .sign_channel_roles(conversation, &[(user, role)], may_found)
      .await?;
    if signing == crate::client::RoleSigning::Unsigned {
      tracing::info!(conversation_id = %conversation, "the role change is the server's word only: this user can't sign it");
    }
    Ok(signing)
  }

  /// The role the stored member list (the server's word) gives `user`.
  fn server_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<Option<teamcraft_proto::ChannelRole>, CoreError> {
    let stored: Option<teamcraft_proto::ListConversationMembersResponse> =
      records::get_json(&self.store, Table::Members, &conversation.to_string())?;
    Ok(
      stored
        .into_iter()
        .flat_map(|list| list.members)
        .find(|member| member.user.id == user)
        .map(|member| member.role),
    )
  }

  /// Removes someone from the space; the server also takes them out of its channels, whose
  /// members' clients then remove their devices from the encryption groups (e2ee.md §4.6).
  pub async fn remove_space_member(&mut self, space: SpaceId, user: UserId) -> Result<(), CoreError> {
    self.api.remove_space_member(space, user).await?;
    self.refresh_space_roster(space).await?;
    self.refresh_spaces().await
  }

  /// Withdraws a pending invitation.
  pub async fn revoke_space_invite(&mut self, space: SpaceId, invite: InviteId) -> Result<(), CoreError> {
    self.api.delete_invite(space, invite).await?;
    self.refresh_space_invites(space).await
  }

  /// Adds people from the directory to a space, as members or guests. The server's add is
  /// idempotent and never changes the role of someone already in the space (it answers 409), so
  /// people already in it count as added. Space membership is directory data; the space's channels
  /// are joined one by one through their encryption groups (e2ee.md §4.6).
  pub async fn add_space_members(
    &mut self,
    space: SpaceId,
    users: &[UserId],
    is_guest: bool,
  ) -> Result<(), CoreError> {
    let (_, maybe_first_error) = self.add_to_space(space, users, is_guest).await;
    // The member count, the roster and the people the user can find changed.
    self.refresh_spaces().await?;
    self.refresh_space_roster(space).await?;
    maybe_first_error.map_or(Ok(()), |error| Err(error.into()))
  }

  /// Adds people to a channel through its encryption group, first to the channel's space when
  /// `joins_space` (members as members, guest accounts as guests). Someone whose space add failed
  /// is not added to the channel: the server would refuse it anyway. Returns the first error.
  pub async fn add_people_to_channel(
    &mut self,
    conversation: ConversationId,
    people: &[(UserId, bool)],
    history: HistoryPolicy,
    joins_space: bool,
  ) -> Result<(), CoreError> {
    let mut ready: Vec<UserId> = people.iter().map(|(user, _)| *user).collect();
    let mut maybe_first_error: Option<CoreError> = None;
    let maybe_space = self
      .cached_conversations()?
      .get(&conversation)
      .and_then(|c| c.maybe_space_id);
    if let Some(space) = maybe_space.filter(|_| joins_space) {
      ready.clear();
      for is_guest in [false, true] {
        let users: Vec<UserId> = people
          .iter()
          .filter(|(_, guest)| *guest == is_guest)
          .map(|(user, _)| *user)
          .collect();
        let (added, maybe_error) = self.add_to_space(space, &users, is_guest).await;
        ready.extend(added);
        if let Some(error) = maybe_error {
          maybe_first_error.get_or_insert(error.into());
        }
      }
      self.refresh_spaces().await?;
    }
    for user in ready {
      if let Err(error) = self
        .add_member_with_history(conversation, user, Some(history.clone()))
        .await
      {
        tracing::warn!(%error, conversation_id = %conversation, "adding a person to the channel failed");
        maybe_first_error.get_or_insert(error);
      }
    }
    maybe_first_error.map_or(Ok(()), Err)
  }

  /// Adds each user once; returns who is in the space now and the first refusal. A 409 (already
  /// in the space with another role) counts as in.
  async fn add_to_space(
    &mut self,
    space: SpaceId,
    users: &[UserId],
    is_guest: bool,
  ) -> (Vec<UserId>, Option<ApiError>) {
    let mut added = Vec::new();
    let mut maybe_first_error = None;
    for user in users_to_add(users, std::iter::empty()) {
      let request = AddSpaceMemberRequest {
        user_id: user,
        role: space_role(is_guest),
        maybe_expires_at: None,
      };
      match self.api.add_space_member(space, &request).await {
        Ok(_) | Err(ApiError::Status { status: 409, .. }) => added.push(user),
        // One refusal (a deactivated account, a guest as member) doesn't stop the others.
        Err(error) => {
          tracing::warn!(%error, space_id = %space, "adding a person to the space failed");
          maybe_first_error.get_or_insert(error);
        },
      }
    }
    (added, maybe_first_error)
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

fn space_role(is_guest: bool) -> SpaceRole {
  if is_guest {
    SpaceRole::Guest
  } else {
    SpaceRole::Member
  }
}

/// The picked people who aren't in the space yet, each once, in the order picked.
fn users_to_add(picked: &[UserId], members: impl IntoIterator<Item = UserId>) -> Vec<UserId> {
  let mut skipped: std::collections::HashSet<UserId> = members.into_iter().collect();
  picked
    .iter()
    .copied()
    .filter(|user| skipped.insert(*user))
    .collect()
}

/// The announcements a channel update calls for: what `request` changed. A difference it didn't
/// ask for (somebody else's change this device hadn't seen yet) is theirs to announce, so this
/// user isn't named for it (issue #818).
fn changes(
  before: &ConversationResponse,
  after: &ConversationResponse,
  request: &UpdateConversationRequest,
) -> Vec<ConversationEvent> {
  let mut events = Vec::new();
  if request.maybe_name.is_some()
    && before.maybe_name != after.maybe_name
    && let Some(name) = &after.maybe_name
  {
    events.push(ConversationEvent::Renamed { name: name.clone() });
  }
  if request.maybe_topic.is_some() && before.maybe_topic != after.maybe_topic {
    events.push(ConversationEvent::TopicChanged {
      topic: after.maybe_topic.clone().unwrap_or_default(),
    });
  }
  if request.maybe_is_archived == Some(after.is_archived) && before.is_archived != after.is_archived {
    events.push(if after.is_archived {
      ConversationEvent::Archived
    } else {
      ConversationEvent::Unarchived
    });
  }
  if request.maybe_visibility.as_ref() == Some(&after.visibility) && before.visibility != after.visibility {
    events.push(match after.visibility {
      ConversationVisibility::Public => ConversationEvent::MadePublic,
      _ => ConversationEvent::MadePrivate,
    });
  }
  events
}
