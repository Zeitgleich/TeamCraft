//! Who counts as a member of a meeting chat on this device (e2ee.md §4.6, "Who is a guest, and
//! until when", #1926): one rule for every decision a device makes in a meeting chat. The member
//! directory that judges commits, the membership rule, what the organizer's device signs (admitters,
//! invitees, hosts), whether this device leaves others' changes to the members, and the "Guest"
//! marks the apps show all read it.
//!
//! A meeting chat is where people from outside meet the workspace, so the server's word that
//! somebody is a member counts for nothing there: a member is who the owners' signed roster vouches
//! for ([`WorkspaceState::vouches_for_member`]), or a host by the host lists this device verified.
//! An admitter the organizer signed lets guests in besides. Everybody else is a guest, whatever the
//! server's user kind says.

use std::collections::BTreeSet;

use teamcraft_proto::UserId;

use crate::workspace::WorkspaceState;

/// What a user is in a meeting chat on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeetingStanding {
  /// Manages the chat's membership like a member of any group: on the signed roster, or the
  /// organizer or a co-host by the verified host lists.
  Member,
  /// Not vouched for as a member, but named by the organizer's signed admitters in force: lets
  /// people in and takes guests out, nothing else (e2ee.md §4.6, "Meeting guests").
  Admitter,
  /// Manages nobody's membership but their own.
  Guest,
}

/// What a device knows that vouches for people in one meeting chat: the verified roster, the
/// hosts of the verified host lists, and the admitters of the organizer's statement in force.
#[derive(Clone, Debug)]
pub(crate) struct MeetingVouchers {
  workspace: WorkspaceState,
  /// The roster is younger than `workspace_max_age` at the time judged: only then does a noted
  /// account vouch for a member (a server that withholds a removal only buys that long).
  is_roster_fresh: bool,
  hosts: BTreeSet<UserId>,
  admitters: BTreeSet<UserId>,
}

/// When a meeting chat is judged: the server's time of the commit (or now, for what this device
/// does or shows), and how old the verified roster may be then (`workspace_max_age`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MeetingTime {
  pub(crate) at_ms: i64,
  pub(crate) roster_max_age_ms: i64,
}

impl MeetingVouchers {
  pub(crate) fn new(
    workspace: WorkspaceState,
    time: MeetingTime,
    hosts: impl IntoIterator<Item = UserId>,
    admitters: impl IntoIterator<Item = UserId>,
  ) -> Self {
    Self {
      is_roster_fresh: workspace.is_roster_fresh(time.at_ms, time.roster_max_age_ms),
      workspace,
      hosts: hosts.into_iter().collect(),
      admitters: admitters.into_iter().collect(),
    }
  }

  /// Whether nothing here vouches for anybody: no verified roster and no host. A device then
  /// applies the rule of other conversations, as the membership rule of meeting chats did before
  /// #1926 (an instance has a roster once its first owner set it up, and a server can't take a
  /// verified roster away from a device).
  pub(crate) fn vouches_for_nobody(&self) -> bool {
    self.workspace.roster.members.is_empty() && self.hosts.is_empty()
  }

  /// What `user` is in the meeting chat. `is_known_guest`: the server calls them a guest or this
  /// device let them in as one ([`crate::client::known_guests`]); that makes a guest of anybody
  /// but an owner, an admin or a host, whom a hostile server can't demote this way. Without a
  /// verified roster, only the hosts and the admitters vouch for anybody; a noted account vouches
  /// only while the roster is fresh (and where there is no
  /// host either, callers apply the rule of other conversations, [`Self::vouches_for_nobody`]).
  pub(crate) fn standing(&self, user: UserId, is_known_guest: bool) -> MeetingStanding {
    if self
      .workspace
      .roster
      .members
      .iter()
      .any(|member| member.user_id == user)
      || self.hosts.contains(&user)
    {
      return MeetingStanding::Member;
    }
    if is_known_guest {
      MeetingStanding::Guest
    } else if self.is_roster_fresh && self.workspace.vouches_for_member(user) {
      MeetingStanding::Member
    } else if self.admitters.contains(&user) {
      MeetingStanding::Admitter
    } else {
      MeetingStanding::Guest
    }
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;

  use teamcraft_proto::{Base64Bytes, Id, Snowflake};

  use super::*;
  use crate::role_signing::{HandRole, ProviderMark};
  use crate::workspace::{RosterMember, RosterRole};

  const OWNER: i64 = 1;
  const ADMIN: i64 = 2;
  const NOTED: i64 = 3;
  const REMOVED_BY_HAND: i64 = 4;
  const UNNOTED: i64 = 5;
  const HOST: i64 = 6;
  const ADMITTER: i64 = 7;
  const NOTED_ADMITTER: i64 = 8;
  const SIGNED_AT_MS: i64 = 1_800_000_000_000;
  const MAX_AGE_MS: i64 = 30 * 24 * 60 * 60_000;

  /// Every role a user can have in a meeting chat, vouched for or not, with the server calling
  /// them a member (`false`) and a guest (`true`): a hostile server's label moves nobody out of
  /// "Guest", and moves no owner, admin or host into it.
  #[test]
  fn every_role_by_the_signed_roster_whatever_the_server_says() {
    let vouchers = vouchers();
    let cases = [
      (OWNER, MeetingStanding::Member, MeetingStanding::Member),
      (ADMIN, MeetingStanding::Member, MeetingStanding::Member),
      (HOST, MeetingStanding::Member, MeetingStanding::Member),
      (NOTED, MeetingStanding::Member, MeetingStanding::Guest),
      (NOTED_ADMITTER, MeetingStanding::Member, MeetingStanding::Guest),
      (ADMITTER, MeetingStanding::Admitter, MeetingStanding::Guest),
      (REMOVED_BY_HAND, MeetingStanding::Guest, MeetingStanding::Guest),
      (UNNOTED, MeetingStanding::Guest, MeetingStanding::Guest),
    ];
    for (user, called_member, called_guest) in cases {
      assert_eq!(
        vouchers.standing(id(user), false),
        called_member,
        "user {user}, the server calls them a member"
      );
      assert_eq!(
        vouchers.standing(id(user), true),
        called_guest,
        "user {user}, the server calls them a guest"
      );
    }
  }

  #[test]
  fn admitters_out_of_force_and_hosts_of_unverified_lists_vouch_for_nobody() {
    let vouchers = MeetingVouchers::new(workspace(), now(), [], []);
    assert_eq!(vouchers.standing(id(HOST), false), MeetingStanding::Guest);
    assert_eq!(vouchers.standing(id(ADMITTER), false), MeetingStanding::Guest);
  }

  #[test]
  fn without_a_verified_roster_only_hosts_and_admitters_vouch() {
    let vouchers = MeetingVouchers::new(WorkspaceState::default(), now(), [id(HOST)], [id(ADMITTER)]);
    assert_eq!(vouchers.standing(id(HOST), true), MeetingStanding::Member);
    assert_eq!(vouchers.standing(id(ADMITTER), false), MeetingStanding::Admitter);
    assert_eq!(vouchers.standing(id(UNNOTED), false), MeetingStanding::Guest);
    assert!(!vouchers.vouches_for_nobody());
    assert!(MeetingVouchers::new(WorkspaceState::default(), now(), [], [id(ADMITTER)]).vouches_for_nobody());
  }

  /// A roster too old at the time judged vouches by the noted accounts for nobody: a server that
  /// withholds the roster that removes somebody buys only `workspace_max_age`. Owners, admins and
  /// hosts stay members.
  #[test]
  fn a_stale_roster_vouches_by_noted_accounts_for_nobody() {
    let mut workspace = workspace();
    workspace.roster.maybe_signed_at_ms = Some(SIGNED_AT_MS);
    workspace.heads.maybe_roster_signed_at_ms = Some(SIGNED_AT_MS);
    let fresh = MeetingVouchers::new(workspace.clone(), now(), [id(HOST)], []);
    assert_eq!(fresh.standing(id(NOTED), false), MeetingStanding::Member);

    let late = MeetingTime {
      at_ms: SIGNED_AT_MS + MAX_AGE_MS + 1,
      roster_max_age_ms: MAX_AGE_MS,
    };
    let stale = MeetingVouchers::new(workspace, late, [id(HOST)], []);
    assert_eq!(stale.standing(id(NOTED), false), MeetingStanding::Guest);
    assert_eq!(stale.standing(id(ADMIN), false), MeetingStanding::Member);
    assert_eq!(stale.standing(id(HOST), false), MeetingStanding::Member);
  }

  fn now() -> MeetingTime {
    MeetingTime {
      at_ms: SIGNED_AT_MS,
      roster_max_age_ms: MAX_AGE_MS,
    }
  }

  fn vouchers() -> MeetingVouchers {
    MeetingVouchers::new(workspace(), now(), [id(HOST)], [id(ADMITTER), id(NOTED_ADMITTER)])
  }

  fn workspace() -> WorkspaceState {
    let mut workspace = WorkspaceState::default();
    workspace.roster.members = vec![member(OWNER, RosterRole::Owner), member(ADMIN, RosterRole::Admin)];
    workspace.roster.provider_marks = BTreeMap::from([
      (id(NOTED), mark(None)),
      (id(NOTED_ADMITTER), mark(None)),
      (id(REMOVED_BY_HAND), mark(Some(HandRole::Removed))),
    ]);
    workspace
  }

  fn member(user: i64, role: RosterRole) -> RosterMember {
    RosterMember {
      user_id: id(user),
      role,
      identity_key: Base64Bytes::new(vec![0; 32]),
    }
  }

  fn mark(maybe_by_hand: Option<HandRole>) -> ProviderMark {
    ProviderMark {
      maybe_subject: Some("subject".into()),
      issued_at: 0,
      maybe_by_hand,
    }
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
