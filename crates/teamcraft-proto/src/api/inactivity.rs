//! Inactive channels (PLAN.md §12, issue #212): the owner hears that a channel has gone quiet, the
//! channel is archived after some more days, and an archived channel can be deleted later. The server
//! is content-blind, so quiet means: no message has been stored in the channel for a while (the time
//! comes from the message IDs, never from the content). Off unless the signed policy turns it on.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::UnixMillis;

/// Quiet for this long, the owners are told.
pub const DEFAULT_INACTIVITY_NOTICE_DAYS: u32 = 180;
/// Days between the notice and the archiving; the channel can be kept open in between.
pub const DEFAULT_INACTIVITY_ARCHIVE_DAYS: u32 = 30;
/// What a snooze defers by unless the request says otherwise.
pub const DEFAULT_INACTIVITY_SNOOZE_DAYS: u32 = 90;
/// Most channels one run notices, archives and deletes each: a safety limit, so that switching the
/// policy on never archives a whole instance at once.
pub const DEFAULT_INACTIVITY_MAX_PER_RUN: u32 = 10;
/// How long before a deletion the owners are told again. Fixed: the deletion can't be undone.
pub const INACTIVITY_DELETE_NOTICE_DAYS: u32 = 7;
/// The shortest period offered between archiving and deleting.
pub const MIN_INACTIVITY_DELETE_DAYS: u32 = 14;
/// The longest any of the periods may be, in days (ten years).
pub const MAX_INACTIVITY_DAYS: u32 = 3650;
/// The longest a single snooze may defer, in days.
pub const MAX_INACTIVITY_SNOOZE_DAYS: u32 = 365;
/// The most channels one run may handle per kind of action.
pub const MAX_INACTIVITY_PER_RUN: u32 = 100;

/// What happens to channels nobody writes in (admin.md §3.1.3). Part of the signed workspace policy:
/// the default is off, and an instance that doesn't name it behaves exactly as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InactivityPolicy {
  /// Master switch. Nothing is noticed, archived or deleted while it is off.
  #[serde(default)]
  pub is_enabled: bool,
  /// A channel with no message for this many days gets an owner notice.
  #[serde(default = "default_notice_days")]
  pub notice_after_days: u32,
  /// Days after the notice that the channel is archived if nothing was written and nobody kept it
  /// open; `0`: notices only, nothing is archived.
  #[serde(default = "default_archive_days")]
  pub archive_after_more_days: u32,
  /// Days a snooze defers by (the owners' "keep it open").
  #[serde(default = "default_snooze_days")]
  pub snooze_days: u32,
  /// Safety limit: most channels one hourly run notices, archives and deletes, each.
  #[serde(default = "default_max_per_run")]
  pub max_per_run: u32,
  /// Delete a channel this many days after the policy archived it, with its messages. Absent (the
  /// default): never. Only channels the policy archived itself, and that nobody restored; an owner
  /// notice goes out [`INACTIVITY_DELETE_NOTICE_DAYS`] before. The only way this policy deletes
  /// content.
  #[serde(
    rename = "delete_after_archived_days",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_delete_after_archived_days: Option<u32>,
}

fn default_notice_days() -> u32 {
  DEFAULT_INACTIVITY_NOTICE_DAYS
}

fn default_archive_days() -> u32 {
  DEFAULT_INACTIVITY_ARCHIVE_DAYS
}

fn default_snooze_days() -> u32 {
  DEFAULT_INACTIVITY_SNOOZE_DAYS
}

fn default_max_per_run() -> u32 {
  DEFAULT_INACTIVITY_MAX_PER_RUN
}

impl Default for InactivityPolicy {
  fn default() -> Self {
    Self {
      is_enabled: false,
      notice_after_days: DEFAULT_INACTIVITY_NOTICE_DAYS,
      archive_after_more_days: DEFAULT_INACTIVITY_ARCHIVE_DAYS,
      snooze_days: DEFAULT_INACTIVITY_SNOOZE_DAYS,
      max_per_run: DEFAULT_INACTIVITY_MAX_PER_RUN,
      maybe_delete_after_archived_days: None,
    }
  }
}

impl InactivityPolicy {
  /// Whether every period and the limit are in their allowed ranges.
  pub fn is_valid(&self) -> bool {
    (1..=MAX_INACTIVITY_DAYS).contains(&self.notice_after_days)
      && self.archive_after_more_days <= MAX_INACTIVITY_DAYS
      && (1..=MAX_INACTIVITY_SNOOZE_DAYS).contains(&self.snooze_days)
      && (1..=MAX_INACTIVITY_PER_RUN).contains(&self.max_per_run)
      && self
        .maybe_delete_after_archived_days
        .is_none_or(|days| (MIN_INACTIVITY_DELETE_DAYS..=MAX_INACTIVITY_DAYS).contains(&days))
  }

  /// Whether channels are archived at all, not only noticed.
  pub fn archives(&self) -> bool {
    self.is_enabled && self.archive_after_more_days > 0
  }
}

/// Body of `POST /api/v1/conversations/{conversation_id}/inactivity/snooze`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SnoozeInactivityRequest {
  /// Days to defer by; absent: the policy's `snooze_days`. At most [`MAX_INACTIVITY_SNOOZE_DAYS`].
  #[serde(rename = "days", default, skip_serializing_if = "Option::is_none")]
  pub maybe_days: Option<u32>,
}

/// Where a channel stands in the inactivity policy, as the admin console lists it. Absent while
/// there is nothing to say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelInactivityStatus {
  /// When the owners were told that the channel is quiet. For an archived channel: that it will be
  /// deleted.
  #[serde(rename = "notified_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_notified_at: Option<UnixMillis>,
  /// When the channel is archived unless somebody writes in it or keeps it open.
  #[serde(rename = "archive_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_archive_at: Option<UnixMillis>,
  /// Nothing is noticed, archived or deleted before this.
  #[serde(rename = "snoozed_until", default, skip_serializing_if = "Option::is_none")]
  pub maybe_snoozed_until: Option<UnixMillis>,
  /// The policy archived the channel (rather than somebody by hand).
  #[serde(default)]
  pub is_archived_by_policy: bool,
  /// When an archived channel is deleted, if the policy deletes.
  #[serde(rename = "delete_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_delete_at: Option<UnixMillis>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_policy_is_off_when_a_document_does_not_name_it() {
    let policy: InactivityPolicy = serde_json::from_str("{}").unwrap();
    assert!(!policy.is_enabled);
    assert_eq!(policy, InactivityPolicy::default());
    assert!(policy.is_valid());
    assert!(!policy.archives());
    assert_eq!(
      policy.maybe_delete_after_archived_days, None,
      "never deletes by default"
    );
  }

  #[test]
  fn periods_are_checked() {
    let valid = InactivityPolicy::default();
    for broken in [
      InactivityPolicy {
        notice_after_days: 0,
        ..valid.clone()
      },
      InactivityPolicy {
        snooze_days: 0,
        ..valid.clone()
      },
      InactivityPolicy {
        max_per_run: 0,
        ..valid.clone()
      },
      InactivityPolicy {
        max_per_run: 101,
        ..valid.clone()
      },
      InactivityPolicy {
        maybe_delete_after_archived_days: Some(1),
        ..valid.clone()
      },
    ] {
      assert!(!broken.is_valid(), "{broken:?}");
    }
    let notices_only = InactivityPolicy {
      archive_after_more_days: 0,
      is_enabled: true,
      ..valid
    };
    assert!(notices_only.is_valid());
    assert!(!notices_only.archives());
  }
}
