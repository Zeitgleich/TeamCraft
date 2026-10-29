//! The notetaker's requests (e2ee.md §8.2): a member asks the notetaker into a call, or sends it
//! away, with a hidden, encrypted control message in the call's conversation
//! ([`MessageContent::Notetaker`](crate::store::MessageContent)). Every member device that reads one,
//! the notetaker's own included, keeps it in a [`NotetakerLedger`] per conversation.
//!
//! Only signed data decides who may do what: the notetaker group of the verified workspace policy
//! and the admins of the verified, fresh roster (admin.md §2.3). The server's account kinds only
//! take rights away (guests and bots ask nobody in). Requests from anybody else never reach the
//! ledger, so a member can't make the notetaker listen against the policy, and nobody can fill
//! the ledger of others.
//!
//! - **Join** ([`NotetakerRequest::Join`]): only from a sender the policy's `inviters` allow, while
//!   the policy is on.
//! - **Stop** ([`NotetakerRequest::Stop`]): from whoever asked the notetaker in, anybody allowed to
//!   ask it in, a roster admin, or anybody in the conversation when `can_anyone_stop` is on.
//!
//! The ledger is bounded ([`MAX_REQUESTS`]) and ordered by server time, so history that arrives
//! newest first reads the same as live messages.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ConversationId, MessageId, NotetakerInviters, NotetakerPolicy, NotetakerRequest, NotetakerScope, UserId,
  UserKind, UserSummary,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::WorkspaceState;

/// Requests a device remembers per conversation; the oldest are dropped beyond this.
pub const MAX_REQUESTS: usize = 64;
/// How long a [`NotetakerScope::ThisCall`] request waits for a call to start.
pub const THIS_CALL_WINDOW_MS: i64 = 10 * 60 * 1000;

/// The requests of one conversation that their senders were allowed to make, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotetakerLedger {
  requests: Vec<NotetakerRequestRecord>,
}

/// One request as it was taken in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotetakerRequestRecord {
  pub by: UserId,
  pub request: NotetakerRequest,
  /// The message's server time (or this device's clock for an own request not sent yet).
  pub at_ms: i64,
  /// `None` while the own request isn't sent yet.
  #[serde(rename = "server_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_server_id: Option<MessageId>,
}

/// Who may ask the notetaker in and send it away, as this device's signed data says.
#[derive(Clone, Debug)]
pub struct NotetakerAuthority {
  policy: NotetakerPolicy,
  /// Admins of the verified, fresh workspace roster.
  admins: HashSet<UserId>,
  /// Guests and bots (by the server's word, which only takes rights away).
  guests_and_bots: HashSet<UserId>,
}

/// A request that asks the notetaker into a call now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
  pub by: UserId,
  pub scope: NotetakerScope,
  pub at_ms: i64,
}

impl NotetakerLedger {
  /// The ledger of `conversation` as stored (empty if nothing was learned yet).
  pub fn load(store: &impl LocalStore, conversation: ConversationId) -> Result<Self, CoreError> {
    Ok(records::get_json(store, Table::Account, &ledger_key(conversation))?.unwrap_or_default())
  }

  /// Takes in a request `record.by` wrote in the conversation, if `authority` allows it. Returns
  /// whether the ledger changed.
  pub fn apply(&mut self, record: NotetakerRequestRecord, authority: &NotetakerAuthority) -> bool {
    let is_allowed = match &record.request {
      NotetakerRequest::Join { scope } => {
        !matches!(scope, NotetakerScope::Unknown(_)) && authority.may_invite(record.by)
      },
      NotetakerRequest::Stop => authority.may_stop(record.by, self.requester_at(record.at_ms)),
      NotetakerRequest::Unknown => false,
    };
    if !is_allowed {
      return false;
    }
    if let Some(existing) = self.requests.iter_mut().find(|known| known.is_same(&record)) {
      // An own request that was sent meanwhile: it now has its server ID and time.
      if existing.maybe_server_id.is_none() && record.maybe_server_id.is_some() {
        *existing = record;
        self.sort();
        return true;
      }
      return false;
    }
    self.requests.push(record);
    self.sort();
    if self.requests.len() > MAX_REQUESTS {
      let excess = self.requests.len() - MAX_REQUESTS;
      self.requests.drain(..excess);
    }
    true
  }

  /// The newest join in force at `now_ms`: a standing [`NotetakerScope::EveryCall`], or a
  /// [`NotetakerScope::ThisCall`] younger than [`THIS_CALL_WINDOW_MS`], whichever is newer, without
  /// a stop after it. `maybe_since_ms`: joins at or before this time were already served.
  pub fn join_in_force(&self, now_ms: i64, maybe_since_ms: Option<i64>) -> Option<JoinRequest> {
    let last_stop_ms = self.last_stop_ms();
    self
      .requests
      .iter()
      .rev()
      .filter(|record| last_stop_ms.is_none_or(|stop| record.at_ms > stop))
      .find_map(|record| match &record.request {
        NotetakerRequest::Join {
          scope: NotetakerScope::EveryCall,
        } => Some(JoinRequest {
          by: record.by,
          scope: NotetakerScope::EveryCall,
          at_ms: record.at_ms,
        }),
        NotetakerRequest::Join {
          scope: NotetakerScope::ThisCall,
        } if now_ms.saturating_sub(record.at_ms) <= THIS_CALL_WINDOW_MS
          && maybe_since_ms.is_none_or(|since| record.at_ms > since) =>
        {
          Some(JoinRequest {
            by: record.by,
            scope: NotetakerScope::ThisCall,
            at_ms: record.at_ms,
          })
        },
        _ => None,
      })
  }

  /// When the newest stop was written; `None` without one.
  pub fn last_stop_ms(&self) -> Option<i64> {
    self
      .requests
      .iter()
      .rev()
      .find(|record| record.request == NotetakerRequest::Stop)
      .map(|record| record.at_ms)
  }

  /// Whether a stop was written after `at_ms`.
  pub fn is_stopped_after(&self, at_ms: i64) -> bool {
    self.last_stop_ms().is_some_and(|stop| stop > at_ms)
  }

  pub fn requests(&self) -> &[NotetakerRequestRecord] {
    &self.requests
  }

  /// Who asked the notetaker in by the join in force at `at_ms` (any age), for stops.
  fn requester_at(&self, at_ms: i64) -> Option<UserId> {
    self
      .requests
      .iter()
      .rev()
      .filter(|record| record.at_ms <= at_ms)
      .take_while(|record| record.request != NotetakerRequest::Stop)
      .find(|record| matches!(record.request, NotetakerRequest::Join { .. }))
      .map(|record| record.by)
  }

  fn sort(&mut self) {
    self
      .requests
      .sort_by_key(|record| (record.at_ms, record.maybe_server_id));
  }
}

impl NotetakerRequestRecord {
  /// The same message: the same server ID, or the own request (same sender, request and time)
  /// before and after it was sent.
  fn is_same(&self, other: &Self) -> bool {
    match (self.maybe_server_id, other.maybe_server_id) {
      (Some(mine), Some(theirs)) => mine == theirs,
      _ => self.by == other.by && self.request == other.request && self.at_ms == other.at_ms,
    }
  }
}

impl NotetakerAuthority {
  /// The authority as this device's store knows it at `now_ms`: the verified policy's notetaker
  /// group and the admins of the roster while it is fresh. Of `users` (those it will be asked
  /// about), the ones the server calls guests or bots lose their rights.
  pub fn from_store(
    store: &impl LocalStore,
    now_ms: i64,
    users: impl IntoIterator<Item = UserId>,
  ) -> Result<Self, CoreError> {
    let workspace: WorkspaceState =
      records::get_json(store, Table::Workspace, crate::client::WORKSPACE_KEY)?.unwrap_or_default();
    let max_age_ms =
      i64::try_from(crate::ClientSettings::default().workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    let admins: HashSet<UserId> = if workspace.is_roster_fresh(now_ms, max_age_ms) {
      workspace.admins().collect()
    } else {
      HashSet::new()
    };
    let mut guests_and_bots = HashSet::new();
    for user in users.into_iter().chain(admins.iter().copied()) {
      if is_guest_or_bot(store, user)? {
        guests_and_bots.insert(user);
      }
    }
    Ok(Self::new(workspace.policy.notetaker, admins, guests_and_bots))
  }

  pub fn new(policy: NotetakerPolicy, admins: HashSet<UserId>, guests_and_bots: HashSet<UserId>) -> Self {
    Self {
      policy,
      admins,
      guests_and_bots,
    }
  }

  pub fn policy(&self) -> &NotetakerPolicy {
    &self.policy
  }

  /// Whether `user` may ask the notetaker in. The conversation's encryption makes sure `user` is
  /// a member of it.
  pub fn may_invite(&self, user: UserId) -> bool {
    if self.policy.active_bot().is_none() || self.guests_and_bots.contains(&user) {
      return false;
    }
    match self.policy.inviters {
      NotetakerInviters::Members => true,
      NotetakerInviters::Admins => self.admins.contains(&user),
      NotetakerInviters::Unknown(_) => false,
    }
  }

  /// Whether `user` may send the notetaker away; `maybe_requester` asked it in.
  pub fn may_stop(&self, user: UserId, maybe_requester: Option<UserId>) -> bool {
    if self.policy.is_notetaker(user) {
      // The notetaker leaves on its own; it never writes requests.
      return false;
    }
    self.policy.can_anyone_stop
      || maybe_requester == Some(user)
      || self.may_invite(user)
      || (self.admins.contains(&user) && !self.guests_and_bots.contains(&user))
  }
}

/// The notetaker group of the verified policy; `None` before a workspace was verified or when the
/// state can't be read.
pub fn notetaker_policy(store: &impl LocalStore) -> Option<NotetakerPolicy> {
  records::get_json::<WorkspaceState>(store, Table::Workspace, crate::client::WORKSPACE_KEY)
    .ok()
    .flatten()
    .map(|state| state.policy.notetaker)
}

/// Applies a received or own request to the stored ledger of `conversation`.
pub(crate) fn apply_request(
  store: &impl LocalStore,
  conversation: ConversationId,
  record: NotetakerRequestRecord,
) -> Result<(), CoreError> {
  let mut ledger = NotetakerLedger::load(store, conversation)?;
  let authority = NotetakerAuthority::from_store(store, record.at_ms, [record.by])?;
  if ledger.apply(record, &authority) {
    records::put_json(store, Table::Account, &ledger_key(conversation), &ledger)?;
  }
  Ok(())
}

fn is_guest_or_bot(store: &impl LocalStore, user: UserId) -> Result<bool, CoreError> {
  let maybe_summary: Option<UserSummary> = records::get_json(store, Table::Users, &user.to_string())?;
  Ok(maybe_summary.is_some_and(|summary| matches!(summary.kind, UserKind::Guest | UserKind::Bot)))
}

fn ledger_key(conversation: ConversationId) -> String {
  format!("notetaker/{conversation}")
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const NOW: i64 = 1_800_000_000_000;

  mod joins {
    use super::*;

    #[test]
    fn only_allowed_senders_ask_the_notetaker_in() {
      let mut ledger = NotetakerLedger::default();
      let members = authority(NotetakerInviters::Members);
      assert!(!ledger.apply(join(guest(), NotetakerScope::ThisCall, NOW), &members));
      assert!(ledger.apply(join(alice(), NotetakerScope::ThisCall, NOW), &members));
      let admins = authority(NotetakerInviters::Admins);
      let mut strict = NotetakerLedger::default();
      assert!(!strict.apply(join(alice(), NotetakerScope::ThisCall, NOW), &admins));
      assert!(strict.apply(join(admin(), NotetakerScope::ThisCall, NOW), &admins));
    }

    #[test]
    fn nobody_asks_a_disabled_notetaker_in() {
      let mut off = authority(NotetakerInviters::Members);
      off.policy.is_enabled = false;
      let mut ledger = NotetakerLedger::default();
      assert!(!ledger.apply(join(admin(), NotetakerScope::EveryCall, NOW), &off));
      assert!(!ledger.apply(join(alice(), NotetakerScope::Unknown("later".into()), NOW), &off));
    }

    #[test]
    fn a_this_call_request_expires_and_is_served_once() {
      let mut ledger = NotetakerLedger::default();
      ledger.apply(
        join(alice(), NotetakerScope::ThisCall, NOW),
        &authority(NotetakerInviters::Members),
      );
      let in_force = ledger.join_in_force(NOW + 1000, None).unwrap();
      assert_eq!((in_force.by, in_force.scope), (alice(), NotetakerScope::ThisCall));
      assert_eq!(ledger.join_in_force(NOW + THIS_CALL_WINDOW_MS + 1, None), None);
      assert_eq!(ledger.join_in_force(NOW + 1000, Some(NOW)), None);
    }

    #[test]
    fn a_standing_request_holds_until_a_stop() {
      let members = authority(NotetakerInviters::Members);
      let mut ledger = NotetakerLedger::default();
      ledger.apply(join(alice(), NotetakerScope::EveryCall, NOW), &members);
      let much_later = NOW + 30 * 24 * 3600 * 1000;
      assert!(ledger.join_in_force(much_later, Some(much_later)).is_some());
      ledger.apply(stop(bob(), NOW + 5), &members);
      assert_eq!(ledger.join_in_force(much_later, None), None);
      assert!(ledger.is_stopped_after(NOW));
    }
  }

  mod stops {
    use super::*;

    #[test]
    fn with_can_anyone_stop_a_guest_can_object() {
      let mut ledger = NotetakerLedger::default();
      let members = authority(NotetakerInviters::Members);
      ledger.apply(join(alice(), NotetakerScope::ThisCall, NOW), &members);
      assert!(ledger.apply(stop(guest(), NOW + 1), &members));
    }

    #[test]
    fn without_it_only_the_requester_inviters_and_admins_stop() {
      let mut strict = authority(NotetakerInviters::Admins);
      strict.policy.can_anyone_stop = false;
      let mut ledger = NotetakerLedger::default();
      ledger.apply(join(admin(), NotetakerScope::EveryCall, NOW), &strict);
      assert!(!ledger.apply(stop(guest(), NOW + 1), &strict));
      assert!(!ledger.apply(stop(bob(), NOW + 2), &strict));
      assert!(ledger.apply(stop(admin(), NOW + 3), &strict));
    }

    #[test]
    fn whoever_asked_it_in_can_send_it_away() {
      let mut members = authority(NotetakerInviters::Members);
      members.policy.can_anyone_stop = false;
      let mut ledger = NotetakerLedger::default();
      ledger.apply(join(alice(), NotetakerScope::ThisCall, NOW), &members);
      // Alice may ask it in anyway; a member who may not, but asked before the policy changed,
      // still may send it away.
      members.policy.inviters = NotetakerInviters::Admins;
      assert!(ledger.apply(stop(alice(), NOW + 1), &members));
    }

    #[test]
    fn the_notetaker_writes_no_requests() {
      let members = authority(NotetakerInviters::Members);
      let mut ledger = NotetakerLedger::default();
      assert!(!ledger.apply(stop(bot(), NOW), &members));
      assert!(!ledger.apply(join(bot(), NotetakerScope::ThisCall, NOW), &members));
    }
  }

  mod ledger {
    use super::*;

    #[test]
    fn history_in_any_order_reads_the_same() {
      let members = authority(NotetakerInviters::Members);
      let records = [
        sent(join(alice(), NotetakerScope::EveryCall, NOW), 1),
        sent(stop(bob(), NOW + 10), 2),
        sent(join(bob(), NotetakerScope::ThisCall, NOW + 20), 3),
      ];
      let mut forward = NotetakerLedger::default();
      let mut backward = NotetakerLedger::default();
      for record in &records {
        forward.apply(record.clone(), &members);
      }
      for record in records.iter().rev() {
        backward.apply(record.clone(), &members);
      }
      assert_eq!(forward, backward);
      assert_eq!(forward.join_in_force(NOW + 30, None).unwrap().by, bob());
    }

    #[test]
    fn an_own_request_is_kept_once_after_it_was_sent() {
      let members = authority(NotetakerInviters::Members);
      let mut ledger = NotetakerLedger::default();
      let own = join(alice(), NotetakerScope::ThisCall, NOW);
      assert!(ledger.apply(own.clone(), &members));
      assert!(ledger.apply(sent(own.clone(), 9), &members));
      assert!(!ledger.apply(sent(own, 9), &members));
      assert_eq!(ledger.requests().len(), 1);
    }

    #[test]
    fn the_ledger_is_bounded() {
      let members = authority(NotetakerInviters::Members);
      let mut ledger = NotetakerLedger::default();
      for index in 0..(MAX_REQUESTS as i64 + 10) {
        ledger.apply(
          sent(join(alice(), NotetakerScope::ThisCall, NOW + index), index + 1),
          &members,
        );
      }
      assert_eq!(ledger.requests().len(), MAX_REQUESTS);
      assert_eq!(ledger.requests()[0].at_ms, NOW + 10);
    }
  }

  fn alice() -> UserId {
    id(11)
  }

  fn bob() -> UserId {
    id(12)
  }

  fn admin() -> UserId {
    id(13)
  }

  fn guest() -> UserId {
    id(14)
  }

  fn bot() -> UserId {
    id(15)
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn authority(inviters: NotetakerInviters) -> NotetakerAuthority {
    let policy = NotetakerPolicy {
      is_enabled: true,
      maybe_bot: Some(bot()),
      inviters,
      ..NotetakerPolicy::default()
    };
    NotetakerAuthority::new(policy, HashSet::from([admin()]), HashSet::from([guest(), bot()]))
  }

  fn join(by: UserId, scope: NotetakerScope, at_ms: i64) -> NotetakerRequestRecord {
    NotetakerRequestRecord {
      by,
      request: NotetakerRequest::Join { scope },
      at_ms,
      maybe_server_id: None,
    }
  }

  fn stop(by: UserId, at_ms: i64) -> NotetakerRequestRecord {
    NotetakerRequestRecord {
      by,
      request: NotetakerRequest::Stop,
      at_ms,
      maybe_server_id: None,
    }
  }

  fn sent(record: NotetakerRequestRecord, server_id: i64) -> NotetakerRequestRecord {
    NotetakerRequestRecord {
      maybe_server_id: Some(MessageId::from_snowflake(Snowflake::new(server_id).unwrap())),
      ..record
    }
  }
}
