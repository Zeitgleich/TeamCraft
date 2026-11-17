//! What the owners of a quiet channel are told in the channel itself (issue #212, admin.md §3.1.3):
//! the policy noted that nobody wrote in it for a while, will archive it unless somebody keeps it
//! open, or archived it and will delete it. One decision for every shell: the web and desktop UI
//! and the phone apps only draw what [`InactivityNotice`] says.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ChannelInactivityStatus, UnixMillis};

use crate::model::Conversation;

/// One thing the owners see about the channel, for the notice's text and the "keep it open"
/// action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InactivityNotice {
  /// Nobody wrote in the channel for a while and the owners were told. It is archived at `archive_at`
  /// unless somebody keeps it open (absent: the policy only notices, it never archives).
  Quiet {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_at: Option<UnixMillis>,
  },
  /// The policy archived the channel; it is deleted at `delete_at` unless somebody postpones that
  /// (absent: it is not deleted).
  ArchivedByPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delete_at: Option<UnixMillis>,
  },
  /// Somebody kept the channel open; the policy leaves it alone until then.
  KeptOpen { until: UnixMillis },
}

impl InactivityNotice {
  /// Whether the notice comes with "keep it open" (for an archived channel: "postpone the
  /// deletion"). A channel that is already kept open, or that nothing threatens, offers nothing.
  pub fn offers_keep_open(&self) -> bool {
    matches!(
      self,
      Self::Quiet { .. } | Self::ArchivedByPolicy { delete_at: Some(_) }
    )
  }
}

/// What the owners are told at `now_ms` (milliseconds since the Unix epoch) about a channel's place
/// in the policy; `None` when there is nothing to say. A snooze that ended says nothing, and an
/// outstanding notice wins over a snooze that is still running only when the server forgot it
/// (it does not: a snooze forgets the notice).
pub fn notice_of(status: &ChannelInactivityStatus, now_ms: i64) -> Option<InactivityNotice> {
  if status.is_archived_by_policy {
    return Some(InactivityNotice::ArchivedByPolicy {
      delete_at: status.maybe_delete_at,
    });
  }
  if status.maybe_notified_at.is_some() {
    return Some(InactivityNotice::Quiet {
      archive_at: status.maybe_archive_at,
    });
  }
  status
    .maybe_snoozed_until
    .filter(|until| until.0 > now_ms)
    .map(|until| InactivityNotice::KeptOpen { until })
}

impl Conversation {
  /// The inactivity notice for the channel's owners: only for who may manage the channel (the
  /// server sends the status only to them, and a client without that word shows nothing), and
  /// never in direct messages.
  pub fn inactivity_notice(&self, now_ms: i64) -> Option<InactivityNotice> {
    if !self.can_manage_channel() {
      return None;
    }
    self
      .maybe_inactivity
      .as_ref()
      .and_then(|status| notice_of(status, now_ms))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{Conversation, ConversationKind, SidebarCategory};
  use teamcraft_proto::Permission;

  const NOW: i64 = 1_000;

  #[test]
  fn a_noticed_channel_is_quiet_and_offers_keeping_it_open() {
    let status = ChannelInactivityStatus {
      maybe_notified_at: Some(UnixMillis(10)),
      maybe_archive_at: Some(UnixMillis(5_000)),
      ..ChannelInactivityStatus::default()
    };
    let notice = notice_of(&status, NOW).unwrap();
    assert_eq!(
      notice,
      InactivityNotice::Quiet {
        archive_at: Some(UnixMillis(5_000))
      }
    );
    assert!(notice.offers_keep_open());
  }

  #[test]
  fn a_policy_that_only_notices_still_offers_keeping_it_open() {
    let status = ChannelInactivityStatus {
      maybe_notified_at: Some(UnixMillis(10)),
      ..ChannelInactivityStatus::default()
    };
    let notice = notice_of(&status, NOW).unwrap();
    assert_eq!(notice, InactivityNotice::Quiet { archive_at: None });
    assert!(notice.offers_keep_open());
  }

  #[test]
  fn an_archived_channel_offers_a_postponement_only_while_it_will_be_deleted() {
    let mut status = ChannelInactivityStatus {
      is_archived_by_policy: true,
      ..ChannelInactivityStatus::default()
    };
    assert!(!notice_of(&status, NOW).unwrap().offers_keep_open());
    status.maybe_delete_at = Some(UnixMillis(9_000));
    let notice = notice_of(&status, NOW).unwrap();
    assert_eq!(
      notice,
      InactivityNotice::ArchivedByPolicy {
        delete_at: Some(UnixMillis(9_000))
      }
    );
    assert!(notice.offers_keep_open());
  }

  #[test]
  fn a_running_snooze_is_shown_and_a_finished_one_is_not() {
    let status = ChannelInactivityStatus {
      maybe_snoozed_until: Some(UnixMillis(2_000)),
      ..ChannelInactivityStatus::default()
    };
    let notice = notice_of(&status, NOW).unwrap();
    assert_eq!(
      notice,
      InactivityNotice::KeptOpen {
        until: UnixMillis(2_000)
      }
    );
    assert!(!notice.offers_keep_open());
    assert_eq!(notice_of(&status, 2_000), None);
    assert_eq!(notice_of(&ChannelInactivityStatus::default(), NOW), None);
  }

  #[test]
  fn only_who_may_manage_the_channel_sees_the_notice() {
    let status = ChannelInactivityStatus {
      maybe_notified_at: Some(UnixMillis(10)),
      ..ChannelInactivityStatus::default()
    };
    let with = |permissions: Vec<Permission>| {
      let mut channel = channel();
      channel.maybe_inactivity = Some(status.clone());
      channel.maybe_permissions = Some(permissions);
      channel
    };
    assert!(
      with(vec![Permission::ManageChannelProperties])
        .inactivity_notice(NOW)
        .is_some()
    );
    assert_eq!(
      with(vec![Permission::CreatePost]).inactivity_notice(NOW),
      None,
      "a member who may not manage the channel is told nothing"
    );
    let mut direct = with(vec![Permission::ManageChannelProperties]);
    direct.kind = ConversationKind::Direct;
    assert_eq!(direct.inactivity_notice(NOW), None);
  }

  fn channel() -> Conversation {
    Conversation {
      id: crate::model::id_from_raw(1),
      maybe_space: None,
      kind: ConversationKind::Public,
      category: SidebarCategory::Channels,
      name: "general".to_owned(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      is_archived: false,
      is_imported: false,
      bookmarks: Vec::new(),
      is_catching_up: false,
      maybe_image_version: None,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_draft_preview: None,
      maybe_inactivity: None,
    }
  }
}
