//! A channel's anchor on the phone (e2ee.md §4.6.5, issue #1842), as desktop and web show it
//! (`teamcraft-ui`'s `anchor_marks`): the name rules a channel's name field checks, the "Name not
//! confirmed" mark, what a deletion confirmation says about who last named the channel, the fork a
//! workspace admin's look at the roles shows, and whether the channel is unreachable on this device.
//!
//! What to say is decided here; the apps only pick the string for each case.

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::model::{
  AnchorFork, AnchorNameRole, ChannelAnchorMarks, ConversationId, MembershipIssue, UserId,
};
use teamcraft_proto::channel_name::{ChannelNameError, checked_channel_name};

use crate::TeamCraft;
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// Why a channel name breaks the rules; each has its own text at the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ChannelNameProblem {
  /// Nothing left after trimming: the field's "enter a name".
  Empty,
  /// `channel-name-error-bidi`
  BidiControl,
  /// `channel-name-error-control`
  ControlCharacter,
  /// `channel-name-error-invisible`
  InvisibleCharacter,
}

impl From<&ChannelNameError> for ChannelNameProblem {
  fn from(error: &ChannelNameError) -> Self {
    match error {
      ChannelNameError::Empty => Self::Empty,
      ChannelNameError::BidiControl => Self::BidiControl,
      ChannelNameError::ControlCharacter => Self::ControlCharacter,
      ChannelNameError::InvisibleCharacter => Self::InvisibleCharacter,
    }
  }
}

/// A channel name checked against the rules every app and the server keep.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelNameCheck {
  /// Keeps the rules: the name as it is sent and signed (trimmed, NFC).
  Valid { name: String },
  /// Breaks them: shown at the name field.
  Refused { problem: ChannelNameProblem },
}

/// Checks a channel name as it is typed (trimmed, NFC; no invisible, control or direction-changing
/// characters): the same rule the core applies before it creates or renames a channel.
#[uniffi::export]
pub fn check_channel_name(name: String) -> ChannelNameCheck {
  match checked_channel_name(&name) {
    Ok(name) => ChannelNameCheck::Valid { name },
    Err(error) => ChannelNameCheck::Refused {
      problem: (&error).into(),
    },
  }
}

/// What a channel's deletion confirmation adds about the name. `maybe_name` is the person's name
/// from the directory; `None` where nobody can name them ("Unknown person").
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelDeleteNameNote {
  /// `channel-delete-name-stale`: the name shown isn't the one members' apps hold, so their devices
  /// keep the messages for 30 days.
  NameStale,
  /// `channel-delete-named-by-founder`
  NamedByFounder { maybe_name: Option<String> },
  /// `channel-delete-renamed-by-workspace`
  RenamedByWorkspace { maybe_name: Option<String> },
  /// `channel-delete-renamed-by-channel-admin`
  RenamedByChannelAdmin { maybe_name: Option<String> },
  /// `channel-delete-renamed-by-member`: members' devices keep the messages for 30 days.
  RenamedByMember { maybe_name: Option<String> },
  /// `channel-delete-renamed-by`: a role a newer app can name.
  RenamedBy { maybe_name: Option<String> },
}

/// What the user's devices saw two of in the channel's anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ChannelAnchorForkItem {
  /// `channel-anchor-forked-founding`
  Founding,
  /// `channel-anchor-forked-properties`
  Properties,
  /// `channel-anchor-forked`
  Unknown,
}

impl From<AnchorFork> for ChannelAnchorForkItem {
  fn from(fork: AnchorFork) -> Self {
    match fork {
      AnchorFork::Founding => Self::Founding,
      AnchorFork::Properties => Self::Properties,
      AnchorFork::Unknown => Self::Unknown,
    }
  }
}

/// What a channel shows of its anchor.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelAnchorMarksItem {
  /// "Name not confirmed" under the channel's name (`channel-name-stale`, its hint
  /// `channel-name-stale-hint`).
  pub is_name_stale: bool,
  /// The note of the deletion confirmation; `None` when this device holds nothing verified.
  pub maybe_delete_note: Option<ChannelDeleteNameNote>,
  /// For a workspace admin's look at the roles only: the anchor forked and grants nothing.
  pub maybe_fork: Option<ChannelAnchorForkItem>,
}

#[uniffi::export]
impl TeamCraft {
  /// What the channel shows of its anchor; `None` when this device holds no anchor for it (a DM, a
  /// channel from before the anchor). Read it again on a change.
  pub async fn channel_anchor_marks(
    &self,
    conversation_id: String,
  ) -> Result<Option<ChannelAnchorMarksItem>, TeamCraftError> {
    self
      .read(move |reads| reads.channel_anchor_marks(&conversation_id))
      .await
  }

  /// Verifies the channel's anchor again, at most once in 5 minutes per channel: call it when the
  /// channel or its settings open and before the deletion confirmation, as desktop and web do. The
  /// marks change with a store update.
  pub async fn refresh_channel_anchor(&self, conversation_id: String) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| {
        reads
          .backend()?
          .refresh_channel_anchor(parse_id(&conversation_id)?);
        Ok(())
      })
      .await
  }

  /// Whether the server offered this device a group the user's devices didn't pin for this channel
  /// (`anchor-refused`): the channel is unreachable here, and new messages stay unreadable.
  pub async fn is_channel_unreachable(&self, conversation_id: String) -> Result<bool, TeamCraftError> {
    self
      .read(move |reads| {
        let conversation: ConversationId = parse_id(&conversation_id)?;
        Ok(is_unreachable(&reads.backend()?.membership_issues(conversation)))
      })
      .await
  }
}

impl Reads {
  fn channel_anchor_marks(
    &self,
    conversation_id: &str,
  ) -> Result<Option<ChannelAnchorMarksItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    Ok(backend.channel_anchor_marks(conversation).map(|marks| {
      marks_item(&marks, |user| {
        backend
          .user(user)
          .map(|found| found.display_name)
          .filter(|name| !name.is_empty())
      })
    }))
  }
}

fn is_unreachable(issues: &[MembershipIssue]) -> bool {
  issues
    .iter()
    .any(|issue| matches!(issue, MembershipIssue::AnchorRefused))
}

/// The marks as the apps show them; `name_of` names a person from the directory.
fn marks_item(
  marks: &ChannelAnchorMarks,
  name_of: impl Fn(UserId) -> Option<String>,
) -> ChannelAnchorMarksItem {
  ChannelAnchorMarksItem {
    is_name_stale: marks.is_name_stale,
    maybe_delete_note: delete_note(marks, name_of),
    maybe_fork: marks.maybe_fork.map(Into::into),
  }
}

/// As desktop and web: a stale name says so instead of naming whoever set the anchor's name, which
/// says nothing about the one shown.
fn delete_note(
  marks: &ChannelAnchorMarks,
  name_of: impl Fn(UserId) -> Option<String>,
) -> Option<ChannelDeleteNameNote> {
  if marks.is_name_stale {
    return Some(ChannelDeleteNameNote::NameStale);
  }
  let setter = marks.maybe_name_set_by?;
  let maybe_name = name_of(setter.user);
  Some(match setter.role {
    AnchorNameRole::Founder => ChannelDeleteNameNote::NamedByFounder { maybe_name },
    AnchorNameRole::ChannelAdmin => ChannelDeleteNameNote::RenamedByChannelAdmin { maybe_name },
    AnchorNameRole::Workspace => ChannelDeleteNameNote::RenamedByWorkspace { maybe_name },
    AnchorNameRole::Member => ChannelDeleteNameNote::RenamedByMember { maybe_name },
    AnchorNameRole::Unknown => ChannelDeleteNameNote::RenamedBy { maybe_name },
  })
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::{AnchorNameSetter, id_from_raw};

  use super::*;

  fn named_by(role: AnchorNameRole) -> ChannelAnchorMarks {
    ChannelAnchorMarks {
      maybe_name_set_by: Some(AnchorNameSetter {
        user: id_from_raw(7),
        role,
      }),
      ..ChannelAnchorMarks::default()
    }
  }

  fn anna(user: UserId) -> Option<String> {
    (user == id_from_raw(7)).then(|| "Anna".to_owned())
  }

  #[test]
  fn the_confirmation_names_who_last_renamed_in_which_role() {
    let note = |role| marks_item(&named_by(role), anna).maybe_delete_note;
    let name = Some("Anna".to_owned());
    assert_eq!(
      note(AnchorNameRole::Member),
      Some(ChannelDeleteNameNote::RenamedByMember {
        maybe_name: name.clone()
      })
    );
    assert_eq!(
      note(AnchorNameRole::Founder),
      Some(ChannelDeleteNameNote::NamedByFounder {
        maybe_name: name.clone()
      })
    );
    assert_eq!(
      note(AnchorNameRole::ChannelAdmin),
      Some(ChannelDeleteNameNote::RenamedByChannelAdmin {
        maybe_name: name.clone()
      })
    );
    assert_eq!(
      note(AnchorNameRole::Workspace),
      Some(ChannelDeleteNameNote::RenamedByWorkspace {
        maybe_name: name.clone()
      })
    );
    assert_eq!(
      note(AnchorNameRole::Unknown),
      Some(ChannelDeleteNameNote::RenamedBy { maybe_name: name })
    );
  }

  #[test]
  fn somebody_the_directory_lacks_is_unnamed() {
    let item = marks_item(&named_by(AnchorNameRole::Member), |_| None);
    assert_eq!(
      item.maybe_delete_note,
      Some(ChannelDeleteNameNote::RenamedByMember { maybe_name: None })
    );
  }

  #[test]
  fn a_stale_name_says_so_instead_of_naming_the_anchors_setter() {
    let marks = ChannelAnchorMarks {
      is_name_stale: true,
      ..named_by(AnchorNameRole::Workspace)
    };
    let item = marks_item(&marks, anna);
    assert!(item.is_name_stale);
    assert_eq!(item.maybe_delete_note, Some(ChannelDeleteNameNote::NameStale));
  }

  #[test]
  fn nothing_verified_adds_nothing() {
    assert_eq!(
      marks_item(&ChannelAnchorMarks::default(), anna),
      ChannelAnchorMarksItem {
        is_name_stale: false,
        maybe_delete_note: None,
        maybe_fork: None,
      }
    );
  }

  #[test]
  fn a_fork_is_named_by_its_side() {
    let fork = |fork| {
      marks_item(
        &ChannelAnchorMarks {
          maybe_fork: Some(fork),
          ..ChannelAnchorMarks::default()
        },
        anna,
      )
      .maybe_fork
    };
    assert_eq!(fork(AnchorFork::Founding), Some(ChannelAnchorForkItem::Founding));
    assert_eq!(
      fork(AnchorFork::Properties),
      Some(ChannelAnchorForkItem::Properties)
    );
    assert_eq!(fork(AnchorFork::Unknown), Some(ChannelAnchorForkItem::Unknown));
  }

  #[test]
  fn only_a_refused_anchor_makes_the_channel_unreachable() {
    assert!(is_unreachable(&[
      MembershipIssue::GroupRefused,
      MembershipIssue::AnchorRefused
    ]));
    assert!(!is_unreachable(&[MembershipIssue::GroupRefused]));
    assert!(!is_unreachable(&[]));
  }

  #[test]
  fn the_name_rules_normalize_or_name_the_problem() {
    assert_eq!(
      check_channel_name("  Plans ".into()),
      ChannelNameCheck::Valid { name: "Plans".into() }
    );
    // "e" and a combining acute become one character (NFC).
    assert_eq!(
      check_channel_name("Cafe\u{301}".into()),
      ChannelNameCheck::Valid {
        name: "Caf\u{e9}".into()
      }
    );
    let problem = |name: &str| match check_channel_name(name.into()) {
      ChannelNameCheck::Refused { problem } => Some(problem),
      ChannelNameCheck::Valid { .. } => None,
    };
    assert_eq!(problem("   "), Some(ChannelNameProblem::Empty));
    assert_eq!(problem("a\u{202e}b"), Some(ChannelNameProblem::BidiControl));
    assert_eq!(problem("a\nb"), Some(ChannelNameProblem::ControlCharacter));
    assert_eq!(
      problem("a\u{200b}b"),
      Some(ChannelNameProblem::InvisibleCharacter)
    );
  }
}
