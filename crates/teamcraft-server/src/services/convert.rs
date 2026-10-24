//! Database records to API responses.

use std::collections::{HashMap, HashSet};

use teamcraft_db::{
  Category, ConversationView, Device, Invite, Membership, SpaceView, StoredMessage, StoredSpaceNotifications,
  User,
};
use teamcraft_proto::{
  Base64Bytes, CategoryResponse, ConversationId, ConversationResponse, DeviceResponse, DirectoryPolicy,
  DisplayNameFormat, InviteResponse, MessageResponse, MyMembership, PushProvider, SpaceImage,
  SpaceNotificationSettings, SpaceResponse, UnixMillis, UserSummary,
};
use time::OffsetDateTime;

use crate::state::AppState;

/// What others see of a user, shaped by the directory policy (display name format and full name
/// privacy). E-mail addresses are added only by the directory lookups.
pub fn user_summary(state: &AppState, user: &User) -> UserSummary {
  let policy = state.policy.get();
  UserSummary {
    id: user.id,
    maybe_username: user.maybe_username.clone(),
    display_name: presented_name(&policy.directory, user),
    kind: user.kind.clone(),
    maybe_email: None,
    maybe_organization: user.maybe_organization.clone(),
    maybe_has_devices: None,
  }
}

/// The display name as the directory policy wants it shown.
pub fn presented_name(policy: &DirectoryPolicy, user: &User) -> String {
  let username = user.maybe_username.as_deref();
  if !policy.shows_full_name || policy.display_name_format == DisplayNameFormat::Username {
    return username.unwrap_or(&user.display_name).to_owned();
  }
  match policy.display_name_format {
    DisplayNameFormat::FirstLast => split_name(&user.display_name).map_or_else(
      || user.display_name.clone(),
      |(first, last)| format!("{first} {last}"),
    ),
    DisplayNameFormat::LastFirst => split_name(&user.display_name).map_or_else(
      || user.display_name.clone(),
      |(first, last)| format!("{last}, {first}"),
    ),
    _ => user.display_name.clone(),
  }
}

/// (first names, last name) of "Ada Lovelace" or "Lovelace, Ada"; `None` for a single word.
fn split_name(name: &str) -> Option<(String, String)> {
  let name = name.trim();
  if let Some((last, first)) = name.split_once(',') {
    let (last, first) = (last.trim(), first.trim());
    return (!last.is_empty() && !first.is_empty()).then(|| (first.to_owned(), last.to_owned()));
  }
  let (first, last) = name.rsplit_once(char::is_whitespace)?;
  let (first, last) = (first.trim(), last.trim());
  (!first.is_empty() && !last.is_empty()).then(|| (first.to_owned(), last.to_owned()))
}

pub fn unix_millis(time: OffsetDateTime) -> UnixMillis {
  UnixMillis(i64::try_from(time.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX))
}

/// `None` if the timestamp is outside the representable range.
pub fn offset_date_time(millis: UnixMillis) -> Option<OffsetDateTime> {
  OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis.0) * 1_000_000).ok()
}

pub fn device_response(device: &Device) -> DeviceResponse {
  DeviceResponse {
    id: device.id,
    name: device.name.clone(),
    public_key: Base64Bytes::new(device.public_key.clone()),
    cross_signature: Base64Bytes::new(device.cross_signature.clone()),
    maybe_push_provider: device.maybe_push_provider.clone().map(PushProvider::from),
    created_at: unix_millis(device.created_at),
  }
}

pub fn space_response(view: &SpaceView) -> SpaceResponse {
  let space = &view.space;
  SpaceResponse {
    id: space.id,
    name: space.name.clone(),
    maybe_icon: space.maybe_icon.clone(),
    maybe_color: space.maybe_color.clone(),
    maybe_description: space.maybe_description.clone(),
    maybe_image: space.maybe_image.as_ref().map(|image| SpaceImage {
      version: image.version.clone(),
      size_bytes: image.size_bytes,
    }),
    join_policy: space.join_policy.clone(),
    is_archived: space.is_archived,
    maybe_brand_id: space.maybe_brand_id.clone(),
    maybe_my_role: view.maybe_role.clone(),
    maybe_my_notifications: view
      .maybe_role
      .as_ref()
      .map(|_| space_notification_settings(&view.notifications)),
    member_count: view.member_count,
    // The caller's abilities need the permission scheme: services::spaces fills them in.
    can_add_members: false,
    can_invite_guests: false,
    can_manage_space: false,
  }
}

pub fn space_notification_settings(stored: &StoredSpaceNotifications) -> SpaceNotificationSettings {
  SpaceNotificationSettings {
    maybe_level: stored.maybe_level.clone(),
    maybe_muted_until: stored.maybe_muted_until.map(unix_millis),
  }
}

pub fn category_response(category: &Category) -> CategoryResponse {
  CategoryResponse {
    id: category.id,
    space_id: category.space_id,
    name: category.name.clone(),
    position: category.position,
  }
}

pub fn invite_response(invite: &Invite) -> InviteResponse {
  InviteResponse {
    id: invite.id,
    space_id: invite.space_id,
    email: invite.email.clone(),
    role: invite.role.clone(),
    channel_ids: invite.channel_ids.clone(),
    sponsor_user_id: invite.sponsor_user_id,
    maybe_expires_at: invite.maybe_expires_at.map(unix_millis),
  }
}

pub fn my_membership(membership: &Membership) -> MyMembership {
  MyMembership {
    role: membership.role.clone(),
    notification_level: membership.notification_level.clone(),
    is_favorite: membership.is_favorite,
    maybe_last_read_message_id: membership.maybe_last_read_message_id,
    maybe_muted_until: membership.maybe_muted_until.map(unix_millis),
    is_notification_inherited: membership.is_notification_inherited,
  }
}

/// Lookup tables the conversation response needs besides the conversation itself.
#[derive(Debug, Default)]
pub struct ConversationExtras {
  pub participants: HashMap<ConversationId, Vec<User>>,
  pub with_guests: HashSet<ConversationId>,
}

pub fn conversation_response(
  state: &AppState,
  view: &ConversationView,
  extras: &ConversationExtras,
) -> ConversationResponse {
  let conversation = &view.conversation;
  ConversationResponse {
    id: conversation.id,
    maybe_space_id: conversation.maybe_space_id,
    kind: conversation.kind.clone(),
    maybe_name: conversation.maybe_name.clone(),
    maybe_topic: conversation.maybe_topic.clone(),
    maybe_description: conversation.maybe_description.clone(),
    visibility: conversation.visibility.clone(),
    history_policy: conversation.history_policy.clone(),
    maybe_category_id: conversation.maybe_category_id,
    is_archived: conversation.is_archived,
    moderation: conversation.moderation.clone(),
    is_integration: conversation.is_integration,
    contains_guests: extras.with_guests.contains(&conversation.id),
    maybe_my_membership: view.maybe_membership.as_ref().map(my_membership),
    participants: extras
      .participants
      .get(&conversation.id)
      .map(|users| users.iter().map(|user| user_summary(state, user)).collect())
      .unwrap_or_default(),
  }
}

pub fn message_response(message: &StoredMessage) -> MessageResponse {
  MessageResponse {
    id: message.id,
    client_message_id: message.client_message_id,
    conversation_id: message.conversation_id,
    sender_user_id: message.sender_user_id,
    sender_device_id: message.sender_device_id,
    maybe_thread_id: message.maybe_thread_id,
    mentioned_user_ids: message.mentioned_user_ids.clone(),
    is_urgent: message.is_urgent,
    content_key_id: Base64Bytes::new(message.content_key_id.clone()),
    ciphertext: Base64Bytes::new(message.ciphertext.clone()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn timestamps_round_trip_through_milliseconds() {
    let millis = UnixMillis(1_767_225_600_123);
    let time = offset_date_time(millis).unwrap();
    assert_eq!(unix_millis(time), millis);
  }

  #[test]
  fn absurd_timestamps_are_rejected() {
    assert!(offset_date_time(UnixMillis(i64::MAX)).is_none());
  }
}
