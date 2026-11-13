//! What the apps show of a channel's anchor (e2ee.md §4.6.5, issue #1749): which note a channel's
//! deletion confirmation adds about its name, and how a workspace admin's look at the roles names
//! a fork. The views only pick the texts from here.

use crate::model::{AnchorFork, AnchorNameRole, ChannelAnchorMarks, UserId};

/// The note a channel's deletion confirmation adds about the name: its i18n key, and whose name it
/// takes (`$name`) if it names somebody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeleteNameNote {
  pub key: &'static str,
  pub maybe_user: Option<UserId>,
}

/// What the confirmation says about the name: that the name shown isn't the anchor's, or who last
/// named the channel, and that a member's name makes the deletion take the retention on members'
/// devices (D5). `None` when this device holds nothing verified to say.
pub fn delete_name_note(marks: &ChannelAnchorMarks) -> Option<DeleteNameNote> {
  // The anchor's name isn't the one shown: who set the anchor's says nothing about this one.
  if marks.is_name_stale {
    return Some(DeleteNameNote {
      key: "channel-delete-name-stale",
      maybe_user: None,
    });
  }
  let setter = marks.maybe_name_set_by?;
  let key = match setter.role {
    AnchorNameRole::Founder => "channel-delete-named-by-founder",
    AnchorNameRole::ChannelAdmin => "channel-delete-renamed-by-channel-admin",
    AnchorNameRole::Workspace => "channel-delete-renamed-by-workspace",
    AnchorNameRole::Member => "channel-delete-renamed-by-member",
    AnchorNameRole::Unknown => "channel-delete-renamed-by",
  };
  Some(DeleteNameNote {
    key,
    maybe_user: Some(setter.user),
  })
}

/// The i18n key of a workspace admin's note on a forked anchor, `None` without a fork.
pub fn fork_note_key(marks: &ChannelAnchorMarks) -> Option<&'static str> {
  Some(match marks.maybe_fork? {
    AnchorFork::Founding => "channel-anchor-forked-founding",
    AnchorFork::Properties => "channel-anchor-forked-properties",
    AnchorFork::Unknown => "channel-anchor-forked",
  })
}

/// Whether the name shown for the channel gets the "not confirmed" mark.
pub fn is_name_stale(maybe_marks: Option<&ChannelAnchorMarks>) -> bool {
  maybe_marks.is_some_and(|marks| marks.is_name_stale)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{AnchorNameSetter, id_from_raw};

  fn named_by(role: AnchorNameRole) -> ChannelAnchorMarks {
    ChannelAnchorMarks {
      maybe_name_set_by: Some(AnchorNameSetter {
        user: id_from_raw(7),
        role,
      }),
      ..ChannelAnchorMarks::default()
    }
  }

  #[test]
  fn the_confirmation_names_who_last_renamed_and_a_members_rename_takes_the_retention() {
    let note = delete_name_note(&named_by(AnchorNameRole::Member));
    assert_eq!(
      note,
      Some(DeleteNameNote {
        key: "channel-delete-renamed-by-member",
        maybe_user: Some(id_from_raw(7)),
      })
    );
    assert_eq!(
      delete_name_note(&named_by(AnchorNameRole::Founder)).map(|note| note.key),
      Some("channel-delete-named-by-founder")
    );
    assert_eq!(
      delete_name_note(&named_by(AnchorNameRole::Workspace)).map(|note| note.key),
      Some("channel-delete-renamed-by-workspace")
    );
    assert_eq!(
      delete_name_note(&named_by(AnchorNameRole::Unknown)).map(|note| note.key),
      Some("channel-delete-renamed-by")
    );
  }

  #[test]
  fn a_stale_name_says_so_instead_of_naming_the_anchors_setter() {
    let marks = ChannelAnchorMarks {
      is_name_stale: true,
      ..named_by(AnchorNameRole::Workspace)
    };
    assert_eq!(
      delete_name_note(&marks),
      Some(DeleteNameNote {
        key: "channel-delete-name-stale",
        maybe_user: None,
      })
    );
    assert!(is_name_stale(Some(&marks)));
    assert!(!is_name_stale(None));
  }

  #[test]
  fn nothing_verified_adds_no_note() {
    assert_eq!(delete_name_note(&ChannelAnchorMarks::default()), None);
    assert_eq!(fork_note_key(&ChannelAnchorMarks::default()), None);
  }

  #[test]
  fn a_fork_is_named_by_its_side() {
    let fork = |fork| ChannelAnchorMarks {
      maybe_fork: Some(fork),
      ..ChannelAnchorMarks::default()
    };
    assert_eq!(
      fork_note_key(&fork(AnchorFork::Founding)),
      Some("channel-anchor-forked-founding")
    );
    assert_eq!(
      fork_note_key(&fork(AnchorFork::Properties)),
      Some("channel-anchor-forked-properties")
    );
    assert_eq!(
      fork_note_key(&fork(AnchorFork::Unknown)),
      Some("channel-anchor-forked")
    );
  }
}
