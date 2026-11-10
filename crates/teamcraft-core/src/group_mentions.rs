//! Group mentions (`@accounting`) on the sender's device (issue #206): the server can't read a
//! message, so the device turns `@handle` into the people to notify. It uses its copy of the
//! groups, which the sealed store keeps and the server's roster refreshes; only members of the
//! conversation are ever named, and only with `use_group_mentions` there (docs/e2ee.md §4.4).

#[cfg(feature = "ui")]
use std::collections::BTreeSet;

#[cfg(feature = "ui")]
use teamcraft_client_api::model::{MentionableGroup, MentionableGroupKind};
#[cfg(feature = "ui")]
use teamcraft_client_api::notify::{MAX_GROUP_MENTIONS_PER_MESSAGE, is_group_handle};
use teamcraft_proto::ListUserGroupRostersResponse;
#[cfg(feature = "ui")]
use teamcraft_proto::{ADMINS_GROUP_HANDLE, ConversationResponse, Permission, UserId};

use crate::error::CoreError;
use crate::records;
#[cfg(feature = "ui")]
use crate::store::MessageContent;
use crate::store::{LocalStore, Table};

const ROSTER_RECORD: &str = "user_group_rosters";

/// The name the built-in `@admins` has where a UI shows no translated one of its own.
#[cfg(feature = "ui")]
const ADMINS_GROUP_NAME: &str = "Admins";

/// The device's copy of the groups (all of them, with members, and the built-in `@admins`); empty
/// before the first read.
#[cfg(feature = "ui")]
pub(crate) fn stored_rosters(store: &impl LocalStore) -> Result<ListUserGroupRostersResponse, CoreError> {
  let stored: Option<ListUserGroupRostersResponse> =
    records::get_json(store, Table::Workspace, ROSTER_RECORD)?;
  Ok(stored.unwrap_or_default())
}

/// Replaces the copy; whether it changed.
pub(crate) fn store_rosters(
  store: &impl LocalStore,
  rosters: &ListUserGroupRostersResponse,
) -> Result<bool, CoreError> {
  let known: Option<ListUserGroupRostersResponse> =
    records::get_json(store, Table::Workspace, ROSTER_RECORD)?;
  if known.as_ref() == Some(rosters) {
    return Ok(false);
  }
  records::put_json(store, Table::Workspace, ROSTER_RECORD, rosters)?;
  Ok(true)
}

/// Whether the user may mention groups in this conversation: the server's word
/// (`my_permissions`), and without one (an older server) yes. A device with no copy of the groups
/// has nothing to expand anyway, which is where guests, who may not read the groups, end up.
#[cfg(feature = "ui")]
pub(crate) fn may_mention_groups(response: &ConversationResponse) -> bool {
  response
    .maybe_my_permissions
    .as_ref()
    .is_none_or(|granted| granted.contains(&Permission::UseGroupMentions))
}

/// What the composer offers in a conversation, and what a send of `@handle` expands through:
/// `members` are the conversation's. Nothing without the permission (no suggestion, no expansion).
/// The built-in `@admins` comes last, while the server announces it (notifications.md, "Group
/// mentions"); a stored group can't take its handle.
#[cfg(feature = "ui")]
pub(crate) fn mentionable_groups(
  rosters: &ListUserGroupRostersResponse,
  members: &BTreeSet<UserId>,
  me: UserId,
  may_mention: bool,
) -> Vec<MentionableGroup> {
  if !may_mention {
    return Vec::new();
  }
  let stored = rosters
    .groups
    .iter()
    .filter(|roster| roster.handle != ADMINS_GROUP_HANDLE)
    .map(|roster| {
      mentionable(
        &roster.handle,
        &roster.name,
        MentionableGroupKind::Custom,
        &roster.member_ids,
        members,
        me,
      )
    });
  let admins = (!rosters.admin_ids.is_empty()).then(|| {
    mentionable(
      ADMINS_GROUP_HANDLE,
      ADMINS_GROUP_NAME,
      MentionableGroupKind::Admins,
      &rosters.admin_ids,
      members,
      me,
    )
  });
  stored.chain(admins).collect()
}

/// One group as offered: who of `group_members` a mention notifies here, and how many it can't.
#[cfg(feature = "ui")]
fn mentionable(
  handle: &str,
  name: &str,
  kind: MentionableGroupKind,
  group_members: &[UserId],
  members: &BTreeSet<UserId>,
  me: UserId,
) -> MentionableGroup {
  let notified: Vec<UserId> = group_members
    .iter()
    .copied()
    .filter(|user| *user != me && members.contains(user))
    .collect();
  let outside = group_members
    .iter()
    .filter(|user| !members.contains(user))
    .count();
  MentionableGroup {
    handle: handle.to_owned(),
    name: name.to_owned(),
    kind,
    notified,
    outside_count: u32::try_from(outside).unwrap_or(u32::MAX),
  }
}

/// The handles a message claims to mention, for showing them: from the ciphertext, so bounded and
/// shaped like a handle whatever the sender wrote.
#[cfg(feature = "ui")]
pub(crate) fn shown_group_mentions(content: &MessageContent) -> Vec<String> {
  let MessageContent::Text { meta, .. } = content else {
    return Vec::new();
  };
  meta
    .group_mentions
    .iter()
    .take(MAX_GROUP_MENTIONS_PER_MESSAGE)
    .filter(|handle| is_group_handle(handle))
    .cloned()
    .collect()
}
