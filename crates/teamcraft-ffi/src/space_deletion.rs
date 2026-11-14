//! A space's deletion for the mobile apps' confirmation (e2ee.md §4.6.5, D7, #1875): the
//! channels it takes, each with who last set its name as the channel's anchor on this device says.
//! The core signs exactly the channels listed here, so the apps show this list before the user
//! confirms; without it the deletion is unsigned. Desktop and web show the same list
//! (`teamcraft-ui`, `space_deletion_list.rs`).

use teamcraft_client_api::model::{
  RenamerRole as RenamerRoleModel, SpaceDeletionPreview as SpaceDeletionPreviewModel,
};
use teamcraft_proto::UserId;

/// What deleting a space takes, as this device prepared it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SpaceDeletionPreview {
  /// The channels that go with the space, in the server's order; empty for a space without any.
  pub channels: Vec<SpaceDeletionChannel>,
  /// Only a workspace owner's or admin's deletion is signed (and can purge at once).
  pub is_signed: bool,
}

/// One channel a space's deletion takes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SpaceDeletionChannel {
  pub conversation_id: String,
  /// The name the server shows; what the signature binds.
  pub name: String,
  /// Who set the current name; `None` where this device holds no verified anchor for the channel
  /// (one it isn't in, or one from before anchors).
  pub maybe_renamed_by: Option<ChannelRenamer>,
  /// What the confirmation says of the channel's history on members' devices.
  pub history_note: SpaceDeletionHistoryNote,
}

/// Whether members' devices keep a channel's history for a while after a signed deletion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SpaceDeletionHistoryNote {
  /// Nothing to say: the deletion purges it at once, or it is unsigned (every channel is kept for
  /// the retention then, and the confirmation doesn't single any out).
  None,
  /// A member chose the name, so members' devices keep its history for a while.
  MemberNameKept,
  /// This device holds no verified anchor for the channel, so it can't tell whether members'
  /// devices will purge it at once: they may keep its history for a while.
  MayBeKept,
}

/// Who set a channel's current name, and in which role.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelRenamer {
  pub user_id: String,
  /// Empty while the person isn't known on this device yet.
  pub display_name: String,
  pub role: ChannelRenamerRole,
}

/// The role in which a channel's name was set (the anchor's `name_set_by`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelRenamerRole {
  /// The channel's founder named it at creation: "Named by X, who created it".
  Founder,
  /// "Last renamed by X, an admin".
  ChannelAdmin,
  /// "Last renamed by X, an admin" (a workspace owner or admin).
  WorkspaceAdmin,
  /// "Last renamed by X, a member"; such a name purges nothing at once.
  Member,
}

/// The preview as the apps show it, with each renamer's display name from `display_name`.
pub(crate) fn space_deletion_preview(
  preview: SpaceDeletionPreviewModel,
  display_name: impl Fn(UserId) -> Option<String>,
) -> SpaceDeletionPreview {
  let is_signed = preview.is_signed;
  SpaceDeletionPreview {
    is_signed,
    channels: preview
      .channels
      .into_iter()
      .map(|channel| {
        let maybe_renamed_by = channel.maybe_renamed_by.map(|renamer| ChannelRenamer {
          user_id: renamer.user.to_string(),
          display_name: display_name(renamer.user).unwrap_or_default(),
          role: renamer.role.into(),
        });
        let history_note = history_note(is_signed, maybe_renamed_by.as_ref().map(|renamer| renamer.role));
        SpaceDeletionChannel {
          conversation_id: channel.conversation.to_string(),
          name: channel.name,
          maybe_renamed_by,
          history_note,
        }
      })
      .collect(),
  }
}

fn history_note(is_signed: bool, maybe_role: Option<ChannelRenamerRole>) -> SpaceDeletionHistoryNote {
  match (is_signed, maybe_role) {
    (false, _) => SpaceDeletionHistoryNote::None,
    (true, None) => SpaceDeletionHistoryNote::MayBeKept,
    (true, Some(ChannelRenamerRole::Member)) => SpaceDeletionHistoryNote::MemberNameKept,
    (true, Some(_)) => SpaceDeletionHistoryNote::None,
  }
}

impl From<RenamerRoleModel> for ChannelRenamerRole {
  fn from(role: RenamerRoleModel) -> Self {
    match role {
      RenamerRoleModel::Founder => Self::Founder,
      RenamerRoleModel::ChannelAdmin => Self::ChannelAdmin,
      RenamerRoleModel::WorkspaceAdmin => Self::WorkspaceAdmin,
      RenamerRoleModel::Member => Self::Member,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::{ChannelRenamer as ChannelRenamerModel, SpaceDeletionPreviewChannel};
  use teamcraft_proto::{ConversationId, SpaceId};

  use super::*;

  const FOUNDER: &str = "11";
  const MEMBER: &str = "12";
  const UNKNOWN: &str = "13";

  #[test]
  fn each_role_is_named_with_the_renamers_name() {
    let preview = space_deletion_preview(
      model(
        true,
        vec![
          channel("1", "general", Some((FOUNDER, RenamerRoleModel::Founder))),
          channel("2", "ops", Some((FOUNDER, RenamerRoleModel::ChannelAdmin))),
          channel("3", "news", Some((FOUNDER, RenamerRoleModel::WorkspaceAdmin))),
          channel("4", "old-test", Some((MEMBER, RenamerRoleModel::Member))),
        ],
      ),
      known_users,
    );

    assert!(preview.is_signed);
    let roles: Vec<_> = preview
      .channels
      .iter()
      .map(|channel| channel.maybe_renamed_by.as_ref().unwrap().role)
      .collect();
    assert_eq!(
      roles,
      [
        ChannelRenamerRole::Founder,
        ChannelRenamerRole::ChannelAdmin,
        ChannelRenamerRole::WorkspaceAdmin,
        ChannelRenamerRole::Member,
      ]
    );
    let member = &preview.channels[3];
    assert_eq!(member.name, "old-test");
    assert_eq!(member.conversation_id, "4");
    assert_eq!(member.maybe_renamed_by.as_ref().unwrap().display_name, "Member B");
    assert_eq!(
      member.history_note,
      SpaceDeletionHistoryNote::MemberNameKept,
      "a member's name keeps the history a while"
    );
    assert!(
      preview.channels[..3]
        .iter()
        .all(|channel| channel.history_note == SpaceDeletionHistoryNote::None)
    );
  }

  #[test]
  fn an_unsigned_deletion_keeps_every_history_anyway_so_no_note() {
    let preview = space_deletion_preview(
      model(
        false,
        vec![
          channel("4", "old-test", Some((MEMBER, RenamerRoleModel::Member))),
          channel("5", "general", None),
        ],
      ),
      known_users,
    );

    assert!(!preview.is_signed);
    assert!(
      preview
        .channels
        .iter()
        .all(|channel| channel.history_note == SpaceDeletionHistoryNote::None)
    );
  }

  #[test]
  fn a_channel_without_an_anchor_names_nobody_and_may_be_kept() {
    let preview = space_deletion_preview(model(true, vec![channel("1", "general", None)]), known_users);

    assert_eq!(preview.channels[0].maybe_renamed_by, None);
    assert_eq!(
      preview.channels[0].history_note,
      SpaceDeletionHistoryNote::MayBeKept,
      "without an anchor here, members' devices may keep it"
    );
  }

  #[test]
  fn an_unknown_renamer_has_an_empty_name() {
    let preview = space_deletion_preview(
      model(
        true,
        vec![channel(
          "1",
          "general",
          Some((UNKNOWN, RenamerRoleModel::Founder)),
        )],
      ),
      known_users,
    );

    let renamer = preview.channels[0].maybe_renamed_by.as_ref().unwrap();
    assert_eq!(renamer.user_id, UNKNOWN);
    assert_eq!(renamer.display_name, "");
  }

  #[test]
  fn an_empty_space_lists_nothing() {
    let preview = space_deletion_preview(model(true, Vec::new()), known_users);

    assert!(preview.channels.is_empty());
    assert!(preview.is_signed);
  }

  fn model(is_signed: bool, channels: Vec<SpaceDeletionPreviewChannel>) -> SpaceDeletionPreviewModel {
    SpaceDeletionPreviewModel {
      space: "1".parse::<SpaceId>().unwrap(),
      channels,
      is_signed,
    }
  }

  fn channel(
    id: &str,
    name: &str,
    maybe_renamer: Option<(&str, RenamerRoleModel)>,
  ) -> SpaceDeletionPreviewChannel {
    SpaceDeletionPreviewChannel {
      conversation: id.parse::<ConversationId>().unwrap(),
      name: name.to_string(),
      maybe_renamed_by: maybe_renamer.map(|(user, role)| ChannelRenamerModel {
        user: user.parse::<UserId>().unwrap(),
        role,
      }),
    }
  }

  fn known_users(id: UserId) -> Option<String> {
    match id.to_string().as_str() {
      FOUNDER => Some("Founder A".to_string()),
      MEMBER => Some("Member B".to_string()),
      _ => None,
    }
  }
}
