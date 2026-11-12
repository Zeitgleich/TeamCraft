//! What the admin roster should say, given what the identity provider signed (e2ee.md §3.1.3, issue
//! #1276). Everything here is a pure decision: [`plan_roster`] takes verified statements and the
//! verified roster and returns the next roster. Fetching and verifying are in
//! `client/role_signing.rs`.
//!
//! The rules that matter:
//!
//! - Only a token puts anybody on the roster or gives them a higher role. A token also takes a role
//!   away: an ID token of the person, or an access token the server saw (`role_losses`), which needs no
//!   cooperation from them and exists also when they have no role left.
//! - Tokens are ordered by the provider's own `iat`, per person, and a person's mark only moves
//!   forward. A replayed older token after a demotion is not newer than the mark and changes nothing.
//!   No device's clock is read: "how old" is judged against the newest token this device itself got
//!   from the provider.
//! - An ID token without the role claim says nothing: the realm may just not map roles into ID tokens.
//! - A person's account at the provider (`sub`) is noted the first time a token counts, and only a token
//!   naming the key the roster already has for them, or this device pinned, can note it, and it can't
//!   lower their role while doing so. A token of another account can't speak for them, so nobody can
//!   demote somebody with a token of their own that names the other person's user ID and key, both of
//!   which are public.
//! - The server's word alone never takes an owner off and never puts anybody on. It takes an admin off
//!   only after standing for a while, one at a time, with a note to the owners, and a newer token beats
//!   it (e2ee.md §3.1.3).
//! - An owner's edit by hand stands until the provider agrees with it once; after that the provider
//!   leads again.
//! - After a move to another provider that the owners signed (`provider_moves` in the policy), the
//!   accounts noted at the old one are forgotten, edits by hand included, and the new provider's are
//!   noted as for anybody's first token. Only the signed policy starts this.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  Base64Bytes, DEFAULT_ADMIN_ROLE, DEFAULT_OWNER_ROLE, RoleSigningPolicy, UserId, WithdrawnReason,
  WithdrawnRole,
};

use crate::workspace::{RosterDocument, RosterMember, RosterRole};

/// The newest thing the identity provider said about a person that an owner's app acted on. Part of
/// the signed roster, so every owner's app applies the same floor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMark {
  /// The person's account at the provider, once a token of theirs counted.
  #[serde(rename = "sub", default, skip_serializing_if = "Option::is_none")]
  pub maybe_subject: Option<String>,
  /// The provider's time (seconds): only a token issued later changes anything for this person.
  pub issued_at: i64,
  /// An owner set this by hand, against or without the provider. It stands until a token says the same.
  #[serde(rename = "by_hand", default, skip_serializing_if = "Option::is_none")]
  pub maybe_by_hand: Option<HandRole>,
}

/// What an owner set by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandRole {
  Owner,
  Admin,
  Removed,
  #[serde(other)]
  Unknown,
}

/// One thing an owner's app changed in the roster by itself, for the audit log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomaticChange {
  pub user_id: UserId,
  pub change: AutomaticChangeKind,
  pub basis: AutomaticBasis,
  /// The provider's time of the token this rests on (seconds).
  #[serde(rename = "issued_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_issued_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticChangeKind {
  Owner,
  Admin,
  Removed,
  NewKey,
  /// Nothing about the role changed: the person's account at the provider was noted.
  Noted,
  #[serde(other)]
  Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticBasis {
  /// A token signed by the identity provider.
  ProviderToken,
  /// The server reported the role or the account gone, for long enough, and nothing newer said
  /// otherwise. Only ever takes an admin off.
  ServerReport,
  #[serde(other)]
  Unknown,
}

/// The provider whose tokens count, where they name the roles, and which roles mean what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleProviderAnchor {
  pub issuer: String,
  pub client_id: String,
  pub owner_role: String,
  pub admin_role: String,
  /// The signed policy's `roles_claim`: empty means the client roles (issue #1742).
  pub roles_claim: Vec<String>,
}

impl RoleProviderAnchor {
  /// The provider the signed policy names, if it names one.
  pub fn signed(maybe_policy: Option<&RoleSigningPolicy>) -> Option<Self> {
    let policy = maybe_policy?;
    let provider = policy.maybe_provider.as_ref()?;
    Some(Self {
      issuer: provider.issuer.clone(),
      client_id: provider.client_id.clone(),
      owner_role: policy.owner_role.clone(),
      admin_role: policy.admin_role.clone(),
      roles_claim: policy.roles_claim.clone(),
    })
  }

  /// While the signed policy names no provider: the one this device got its own token from, read with
  /// the role names and claim the policy gives, else the ones everybody starts with. Never the server's
  /// word.
  pub fn of_own_token(issuer: &str, client_id: &str, maybe_policy: Option<&RoleSigningPolicy>) -> Self {
    Self {
      issuer: issuer.to_owned(),
      client_id: client_id.to_owned(),
      owner_role: maybe_policy.map_or_else(
        || DEFAULT_OWNER_ROLE.to_owned(),
        |policy| policy.owner_role.clone(),
      ),
      admin_role: maybe_policy.map_or_else(
        || DEFAULT_ADMIN_ROLE.to_owned(),
        |policy| policy.admin_role.clone(),
      ),
      roles_claim: maybe_policy.map_or_else(Vec::new, |policy| policy.roles_claim.clone()),
    }
  }
}

/// What a token's roles mean in this workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderRole {
  Owner,
  Admin,
  /// The token lists roles, and neither of these two.
  Neither,
  /// The token has no role claim: no statement either way.
  NotSaid,
}

/// A token of a person that passed every check, read with the workspace's role names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderStatement {
  pub user_id: UserId,
  /// The identity key the token names.
  pub identity_key: Base64Bytes,
  pub subject: String,
  pub issued_at: i64,
  pub role: ProviderRole,
}

/// The inputs of [`plan_roster`].
#[derive(Clone, Copy, Debug)]
pub struct RosterPlanInput<'a> {
  pub roster: &'a RosterDocument,
  pub marks: &'a BTreeMap<UserId, ProviderMark>,
  pub statements: &'a [ProviderStatement],
  pub withdrawn: &'a [WithdrawnRole],
  /// The signing owner: the server's word never takes them off.
  pub me: UserId,
  /// The newest `iat` this device got from the provider itself: the only clock used here.
  pub provider_now: i64,
  /// A token older than this (seconds, by `provider_now`) puts nobody on the roster.
  pub max_evidence_age_secs: i64,
  /// The keys this device pinned: nobody's account is noted under another key.
  pub pinned: &'a BTreeMap<UserId, [u8; 32]>,
  /// Since when (`provider_now`) this device has seen the server report each person.
  pub reports: &'a BTreeMap<UserId, i64>,
  /// When this device last took somebody off on a report.
  pub maybe_last_report_removal: Option<i64>,
  /// How long a report has to stand before it counts (seconds, provider's clock).
  pub report_grace_secs: i64,
  /// How long after one removal on a report the next can follow (seconds, provider's clock).
  pub report_interval_secs: i64,
}

/// What [`plan_roster`] decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RosterPlan {
  pub members: Vec<RosterMember>,
  pub marks: BTreeMap<UserId, ProviderMark>,
  pub changes: Vec<AutomaticChange>,
  /// People whose evidence contradicts what is signed: another account speaks for them, or their
  /// removal would leave the roster without an owner. A person has to look at these.
  pub contradicted: Vec<UserId>,
  /// Admins the server reports gone, waiting for their report to count.
  pub reported: Vec<UserId>,
  /// The reports still standing, for the next round.
  pub reports: BTreeMap<UserId, i64>,
  pub maybe_last_report_removal: Option<i64>,
}

impl RosterPlan {
  /// Whether there is anything to sign.
  pub fn has_changes(&self) -> bool {
    !self.changes.is_empty()
  }
}

/// The role in `maybe_roles`, by the role names of `anchor`. The higher role wins.
pub fn provider_role(maybe_roles: Option<&[String]>, anchor: &RoleProviderAnchor) -> ProviderRole {
  let Some(roles) = maybe_roles else {
    return ProviderRole::NotSaid;
  };
  let has = |wanted: &str| roles.iter().any(|role| role == wanted);
  if has(&anchor.owner_role) {
    ProviderRole::Owner
  } else if has(&anchor.admin_role) {
    ProviderRole::Admin
  } else {
    ProviderRole::Neither
  }
}

/// An expired access token the server kept, as a statement about somebody on the roster, if it shows a
/// lower role than they hold and comes from the account noted for them. It names no key and the server
/// says whose it is, so it can never be what first ties an account to a person.
pub fn role_loss_statement(
  member: &RosterMember,
  maybe_mark: Option<&ProviderMark>,
  subject: String,
  issued_at: i64,
  role: ProviderRole,
) -> Option<ProviderStatement> {
  let is_lower = match role {
    ProviderRole::Neither => true,
    ProviderRole::Admin => member.role == RosterRole::Owner,
    ProviderRole::Owner | ProviderRole::NotSaid => false,
  };
  let is_the_noted_account = maybe_mark
    .and_then(|mark| mark.maybe_subject.as_deref())
    .is_some_and(|noted| noted == subject);
  (is_lower && is_the_noted_account).then(|| ProviderStatement {
    user_id: member.user_id,
    identity_key: member.identity_key.clone(),
    subject,
    issued_at,
    role,
  })
}

/// The provider move the signed policy names, if the roster's marks don't follow it yet: their
/// accounts were noted at the provider before (`settled` is the move they belong to). Only once the
/// policy names the new provider, so they are noted again at the one the owners' apps pinned. An older
/// policy, or one an older app signed without the count, says nothing.
pub fn unsettled_provider_move(maybe_policy: Option<&RoleSigningPolicy>, settled: u64) -> Option<u64> {
  let policy = maybe_policy?;
  (policy.maybe_provider.is_some() && policy.provider_moves > settled).then_some(policy.provider_moves)
}

/// `marks` without the accounts they noted, for a roster that follows a provider move: what an owner
/// set by hand, and each person's floor, stay.
pub fn forget_accounts(marks: &BTreeMap<UserId, ProviderMark>) -> BTreeMap<UserId, ProviderMark> {
  marks
    .iter()
    .map(|(user, mark)| {
      let forgotten = ProviderMark {
        maybe_subject: None,
        ..mark.clone()
      };
      (*user, forgotten)
    })
    .collect()
}

/// Counts an owner's move to another provider into `signing`, the verified policy about to pin
/// `issuer`: only a policy an owner signed to take a new provider (`accepts_new_provider`) is a move.
/// Higher than any move this device saw, also one an older owner's app signed the count away from
/// (`settled`). Accounts (`sub`) belong to an issuer, so signing in at the one this device used before
/// (`maybe_previous_issuer`) is no move and keeps them; a device that knows none counts it.
pub fn count_provider_move(
  signing: &mut RoleSigningPolicy,
  settled: u64,
  maybe_previous_issuer: Option<&str>,
  issuer: &str,
) {
  let is_same_issuer = maybe_previous_issuer
    .is_some_and(|previous| previous.trim_end_matches('/') == issuer.trim_end_matches('/'));
  if signing.accepts_new_provider && !is_same_issuer {
    signing.provider_moves = signing.provider_moves.max(settled).saturating_add(1);
  }
}

/// The next roster and marks. Applied in the same way by every owner's app, so several of them
/// arrive at the same document and the loser of the version race finds nothing left to do.
pub fn plan_roster(input: RosterPlanInput<'_>) -> RosterPlan {
  let mut plan = RosterPlan {
    members: input.roster.members.clone(),
    marks: input.marks.clone(),
    maybe_last_report_removal: input.maybe_last_report_removal,
    ..RosterPlan::default()
  };
  for gone in input.withdrawn {
    let since = input.reports.get(&gone.user_id).copied();
    plan
      .reports
      .insert(gone.user_id, since.unwrap_or(input.provider_now));
  }
  // Only the newest thing the provider said about a person counts.
  let newest = |user: UserId| {
    input
      .statements
      .iter()
      .filter(|statement| statement.user_id == user)
      .map(|statement| statement.issued_at)
      .max()
  };
  let mut statements: Vec<&ProviderStatement> = input
    .statements
    .iter()
    .filter(|statement| newest(statement.user_id) == Some(statement.issued_at))
    .collect();
  statements.sort_by_key(|statement| (statement.user_id, statement.issued_at));
  // Additions first, so a handover through the same run keeps an owner on the roster.
  for statement in statements
    .iter()
    .filter(|statement| matches!(statement.role, ProviderRole::Owner | ProviderRole::Admin))
  {
    // A report stops tokens from before it; a token issued after it beats it.
    match plan.reports.get(&statement.user_id) {
      Some(since) if statement.issued_at <= *since => continue,
      Some(_) => {
        plan.reports.remove(&statement.user_id);
      },
      None => {},
    }
    apply_role(&mut plan, statement, &input);
  }
  for statement in statements
    .iter()
    .filter(|statement| statement.role == ProviderRole::Neither)
  {
    apply_removal(&mut plan, statement, &input);
  }
  for gone in input.withdrawn {
    if plan.reports.contains_key(&gone.user_id) {
      apply_report(&mut plan, gone, &input);
    }
  }
  plan.contradicted.sort();
  plan.contradicted.dedup();
  plan
}

/// A token that gives somebody a role.
fn apply_role(plan: &mut RosterPlan, statement: &ProviderStatement, input: &RosterPlanInput<'_>) {
  let Some(is_new) = is_news(plan, statement, input) else {
    return;
  };
  let wanted = match statement.role {
    ProviderRole::Owner => RosterRole::Owner,
    _ => RosterRole::Admin,
  };
  let hand = plan
    .marks
    .get(&statement.user_id)
    .and_then(|mark| mark.maybe_by_hand);
  if hand.is_some_and(|hand| !hand_agrees(hand, statement.role)) {
    // What an owner set by hand stands until the provider says the same.
    return;
  }
  let maybe_member = plan
    .members
    .iter()
    .position(|member| member.user_id == statement.user_id);
  // User ID and key are public, and whose token this is the server's word until an account is noted: a
  // token that would be the first to speak for somebody doesn't get to lower them.
  let is_first = plan
    .marks
    .get(&statement.user_id)
    .is_none_or(|mark| mark.maybe_subject.is_none());
  if is_first
    && wanted == RosterRole::Admin
    && maybe_member.is_some_and(|index| plan.members[index].role == RosterRole::Owner)
  {
    plan.contradicted.push(statement.user_id);
    return;
  }
  let is_role_change = maybe_member.is_none_or(|index| plan.members[index].role != wanted);
  let is_key_change =
    maybe_member.is_some_and(|index| plan.members[index].identity_key != statement.identity_key);
  if is_role_change || is_key_change {
    // A token this old could be a replay on a log the server holds back: it only counts while recent.
    if input.provider_now.saturating_sub(statement.issued_at) > input.max_evidence_age_secs {
      return;
    }
    if !is_new {
      return;
    }
    match maybe_member {
      Some(index) => {
        if is_role_change && would_leave_no_owner_as(plan, index, &wanted) {
          plan.contradicted.push(statement.user_id);
          return;
        }
        plan.members[index].role = wanted.clone();
        plan.members[index].identity_key = statement.identity_key.clone();
      },
      None => plan.members.push(RosterMember {
        user_id: statement.user_id,
        role: wanted.clone(),
        identity_key: statement.identity_key.clone(),
      }),
    }
    let change = match (is_role_change, wanted) {
      (false, _) => AutomaticChangeKind::NewKey,
      (true, RosterRole::Owner) => AutomaticChangeKind::Owner,
      (true, _) => AutomaticChangeKind::Admin,
    };
    note_change(plan, statement, change);
    return;
  }
  // Nothing changes for the person: only a first token, or the first to agree with an owner's edit, has
  // to be written down, so that their account is known when a later one takes the role away.
  let has_subject = plan
    .marks
    .get(&statement.user_id)
    .is_some_and(|mark| mark.maybe_subject.is_some());
  if !has_subject || hand.is_some() {
    note_change(plan, statement, AutomaticChangeKind::Noted);
  }
}

/// A token that lists no role for somebody.
fn apply_removal(plan: &mut RosterPlan, statement: &ProviderStatement, input: &RosterPlanInput<'_>) {
  // Without a known account any member could speak for this person: their user ID and key are
  // public. Their first token with the role, or an owner's edit, settle it.
  let Some(mark) = plan.marks.get(&statement.user_id) else {
    return;
  };
  if mark.maybe_subject.is_none() {
    return;
  }
  let hand = mark.maybe_by_hand;
  if is_news(plan, statement, input) != Some(true) {
    return;
  }
  // A real dip the provider took back could otherwise be replayed for ever: marks don't move on tokens
  // that say what the roster says.
  if input.provider_now.saturating_sub(statement.issued_at) > input.max_evidence_age_secs {
    return;
  }
  let maybe_index = plan
    .members
    .iter()
    .position(|member| member.user_id == statement.user_id);
  match (hand, maybe_index) {
    // The provider agrees with the owner's removal: from now on it leads again.
    (Some(HandRole::Removed), _) => note_change(plan, statement, AutomaticChangeKind::Noted),
    // Set by hand without the provider: it stands.
    (Some(_), _) | (None, None) => {},
    (None, Some(index)) => {
      if would_leave_no_owner(plan, index) {
        plan.contradicted.push(statement.user_id);
        return;
      }
      plan.members.remove(index);
      note_change(plan, statement, AutomaticChangeKind::Removed);
    },
  }
}

/// The server reports somebody's role or account gone, and nothing newer said otherwise. The server
/// alone can't take anybody off quickly or many at once: an owner never, as nobody could recover from
/// that; an admin only once the report has stood for the grace time and the last such removal lies an
/// interval back. Until then the owners see it in the console.
fn apply_report(plan: &mut RosterPlan, gone: &WithdrawnRole, input: &RosterPlanInput<'_>) {
  let Some(index) = plan
    .members
    .iter()
    .position(|member| member.user_id == gone.user_id)
  else {
    plan.reports.remove(&gone.user_id);
    return;
  };
  let maybe_mark = plan.marks.get(&gone.user_id);
  // Somebody an owner put on by hand, or from before tokens counted, has no provider role to lose. A
  // gone account is another matter.
  let is_provider_held =
    maybe_mark.is_some_and(|mark| mark.maybe_subject.is_some() && mark.maybe_by_hand.is_none());
  if gone.reason != WithdrawnReason::AccountGone && !is_provider_held {
    plan.reports.remove(&gone.user_id);
    return;
  }
  if gone.user_id == input.me {
    return;
  }
  if plan.members[index].role == RosterRole::Owner {
    plan.contradicted.push(gone.user_id);
    return;
  }
  let since = plan
    .reports
    .get(&gone.user_id)
    .copied()
    .unwrap_or(input.provider_now);
  let has_stood = input.provider_now.saturating_sub(since) >= input.report_grace_secs;
  let is_next_due = plan
    .maybe_last_report_removal
    .is_none_or(|last| input.provider_now.saturating_sub(last) >= input.report_interval_secs);
  if !has_stood || !is_next_due {
    plan.reported.push(gone.user_id);
    return;
  }
  plan.members.remove(index);
  plan.reports.remove(&gone.user_id);
  plan.maybe_last_report_removal = Some(input.provider_now);
  // A token from before this moment can't bring them back.
  let maybe_subject = plan
    .marks
    .get(&gone.user_id)
    .and_then(|mark| mark.maybe_subject.clone());
  let floor = plan
    .marks
    .get(&gone.user_id)
    .map_or(input.provider_now, |mark| mark.issued_at.max(input.provider_now));
  plan.marks.insert(
    gone.user_id,
    ProviderMark {
      maybe_subject,
      issued_at: floor,
      maybe_by_hand: None,
    },
  );
  plan.changes.push(AutomaticChange {
    user_id: gone.user_id,
    change: AutomaticChangeKind::Removed,
    basis: AutomaticBasis::ServerReport,
    maybe_issued_at: None,
  });
}

/// Whether the provider now says what the owner set by hand.
fn hand_agrees(hand: HandRole, role: ProviderRole) -> bool {
  matches!(
    (hand, role),
    (HandRole::Owner, ProviderRole::Owner)
      | (HandRole::Admin, ProviderRole::Admin)
      | (HandRole::Removed, ProviderRole::Neither)
  )
}

/// `Some(true)` if `statement` is newer than the person's mark, `Some(false)` if it is the first
/// token to say the same as the mark, `None` if it can't change anything: not newer, or it can't speak
/// for this person (which goes on the list of contradictions).
fn is_news(
  plan: &mut RosterPlan,
  statement: &ProviderStatement,
  input: &RosterPlanInput<'_>,
) -> Option<bool> {
  // One account at the provider is one person.
  let is_somebody_elses = plan.marks.iter().any(|(user, mark)| {
    *user != statement.user_id && mark.maybe_subject.as_deref() == Some(statement.subject.as_str())
  });
  let maybe_mark = plan.marks.get(&statement.user_id);
  let maybe_noted = maybe_mark.and_then(|mark| mark.maybe_subject.as_ref());
  let can_speak = !is_somebody_elses
    && match maybe_noted {
      Some(subject) => *subject == statement.subject,
      None => names_the_known_key(plan, statement, input),
    };
  if !can_speak {
    plan.contradicted.push(statement.user_id);
    return None;
  }
  let Some(mark) = plan.marks.get(&statement.user_id) else {
    return Some(true);
  };
  if statement.issued_at > mark.issued_at {
    return Some(true);
  }
  // Written down by a report of the server before any token counted: this token is older than that.
  (mark.maybe_subject.is_none() && statement.issued_at == mark.issued_at).then_some(false)
}

/// Whether a token may note this person's account: it names the key the roster has for them, or, for
/// somebody not on the roster, the key this device pinned, if it pinned one.
fn names_the_known_key(
  plan: &RosterPlan,
  statement: &ProviderStatement,
  input: &RosterPlanInput<'_>,
) -> bool {
  match plan
    .members
    .iter()
    .find(|member| member.user_id == statement.user_id)
  {
    Some(member) => member.identity_key == statement.identity_key,
    None => input
      .pinned
      .get(&statement.user_id)
      .is_none_or(|pinned| pinned.as_slice() == statement.identity_key.as_slice()),
  }
}

fn note_change(plan: &mut RosterPlan, statement: &ProviderStatement, change: AutomaticChangeKind) {
  plan.marks.insert(
    statement.user_id,
    ProviderMark {
      maybe_subject: Some(statement.subject.clone()),
      issued_at: statement.issued_at,
      maybe_by_hand: None,
    },
  );
  plan.changes.push(AutomaticChange {
    user_id: statement.user_id,
    change,
    basis: AutomaticBasis::ProviderToken,
    maybe_issued_at: Some(statement.issued_at),
  });
}

/// Whether taking away the entry at `index` leaves nobody who could sign the next roster.
fn would_leave_no_owner(plan: &RosterPlan, index: usize) -> bool {
  let mut remaining = RosterDocument {
    members: plan.members.clone(),
    ..RosterDocument::default()
  };
  remaining.members.remove(index);
  remaining.owner_count() == 0
}

/// Whether giving the entry at `index` the role `wanted` leaves nobody who could sign the next roster.
fn would_leave_no_owner_as(plan: &RosterPlan, index: usize, wanted: &RosterRole) -> bool {
  *wanted != RosterRole::Owner
    && plan.members[index].role == RosterRole::Owner
    && would_leave_no_owner(plan, index)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id as _, Snowflake};

  use super::*;

  const NOW: i64 = 1_000_000;
  const MAX_AGE: i64 = 30 * 24 * 60 * 60;
  const GRACE: i64 = 7 * 24 * 60 * 60;

  mod additions {
    use super::*;

    #[test]
    fn a_token_with_a_role_puts_somebody_on_the_roster() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
      assert_eq!(plan.marks[&bob()].issued_at, NOW);
      assert_eq!(plan.marks[&bob()].maybe_subject.as_deref(), Some("sub-2"));
      assert_eq!(plan.changes[0].change, AutomaticChangeKind::Admin);
      assert_eq!(plan.changes[0].basis, AutomaticBasis::ProviderToken);
    }

    #[test]
    fn the_key_on_the_roster_is_the_one_the_token_names() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[statement(bob(), ProviderRole::Owner, NOW)],
        &[],
      );
      let member = plan
        .members
        .iter()
        .find(|member| member.user_id == bob())
        .unwrap();
      assert_eq!(member.identity_key, key(bob()));
      assert_eq!(member.role, RosterRole::Owner);
    }

    #[test]
    fn a_token_too_old_to_tell_appoints_nobody() {
      let old = statement(bob(), ProviderRole::Admin, NOW - MAX_AGE - 1);
      assert!(!planned(&roster(), &no_marks(), &[old], &[]).has_changes());
      let just_in_time = statement(bob(), ProviderRole::Admin, NOW - MAX_AGE);
      assert!(planned(&roster(), &no_marks(), &[just_in_time], &[]).has_changes());
    }

    #[test]
    fn a_token_without_the_claim_changes_nothing() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let marks = marked(bob(), "sub-2", NOW - 10);
      let plan = planned(
        &signed,
        &marks,
        &[statement(bob(), ProviderRole::NotSaid, NOW)],
        &[],
      );
      assert!(!plan.has_changes());
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn a_new_identity_key_replaces_the_old_one() {
      let mut signed = roster();
      signed.members.push(RosterMember {
        identity_key: Base64Bytes::new(vec![9; 32]),
        ..member(bob(), RosterRole::Admin)
      });
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[],
      );
      assert_eq!(plan.changes[0].change, AutomaticChangeKind::NewKey);
      let member = plan
        .members
        .iter()
        .find(|member| member.user_id == bob())
        .unwrap();
      assert_eq!(member.identity_key, key(bob()));
    }

    #[test]
    fn the_first_token_of_somebody_already_on_the_roster_is_written_down_once() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let token = [statement(bob(), ProviderRole::Admin, NOW)];
      let plan = planned(&signed, &no_marks(), &token, &[]);
      assert_eq!(plan.changes[0].change, AutomaticChangeKind::Noted);
      assert_eq!(plan.members, signed.members);
      // The next round has nothing to sign, and neither has a newer token saying the same.
      assert!(!planned(&signed, &plan.marks, &token, &[]).has_changes());
      let newer = [statement(bob(), ProviderRole::Admin, NOW + 100)];
      assert!(!planned(&signed, &plan.marks, &newer, &[]).has_changes());
    }
  }

  mod removals {
    use super::*;

    #[test]
    fn a_newer_token_without_the_role_takes_it_away() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[statement(bob(), ProviderRole::Neither, NOW)],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(plan.changes[0].change, AutomaticChangeKind::Removed);
      assert_eq!(plan.marks[&bob()].issued_at, NOW);
    }

    #[test]
    fn a_newer_token_with_a_lower_role_demotes() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Owner));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn an_old_token_replayed_after_a_demotion_appoints_nobody() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let appointing = statement(bob(), ProviderRole::Admin, NOW - 100);
      let demoting = statement(bob(), ProviderRole::Neither, NOW - 50);
      let after_demotion = planned(&signed, &marked(bob(), "sub-2", NOW - 100), &[demoting], &[]);
      let demoted_roster = RosterDocument {
        members: after_demotion.members.clone(),
        ..RosterDocument::default()
      };
      let replayed = planned(&demoted_roster, &after_demotion.marks, &[appointing], &[]);
      assert!(!replayed.has_changes(), "{replayed:?}");
      assert_eq!(role_of(&replayed, bob()), None);
      // A token issued later, when the provider gave the role back, counts again.
      let again = statement(bob(), ProviderRole::Admin, NOW);
      let back = planned(&demoted_roster, &after_demotion.marks, &[again], &[]);
      assert_eq!(role_of(&back, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn somebody_elses_token_can_not_take_a_role_away() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      // A member's own token naming Bob's public user ID and key.
      let forged = ProviderStatement {
        subject: "sub-of-a-member".into(),
        ..statement(bob(), ProviderRole::Neither, NOW)
      };
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        std::slice::from_ref(&forged),
        &[],
      );
      assert!(!plan.has_changes());
      assert_eq!(plan.contradicted, vec![bob()]);
      // And before Bob's account is known such a token proves nothing either.
      let unknown = planned(&signed, &no_marks(), &[forged], &[]);
      assert!(!unknown.has_changes());
      assert!(unknown.contradicted.is_empty());
    }

    #[test]
    fn somebody_elses_token_can_not_give_a_role_to_somebody_whose_account_is_known() {
      let forged = ProviderStatement {
        subject: "sub-of-an-admin".into(),
        ..statement(bob(), ProviderRole::Admin, NOW)
      };
      let plan = planned(&roster(), &marked(bob(), "sub-2", NOW - 10), &[forged], &[]);
      assert!(!plan.has_changes());
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn a_dip_too_old_to_tell_takes_nobody_off_owners_included() {
      for role in [RosterRole::Admin, RosterRole::Owner] {
        let mut signed = roster();
        signed.members.push(member(bob(), role.clone()));
        let marks = marked(bob(), "sub-2", NOW - 2 * MAX_AGE);
        let old = statement(bob(), ProviderRole::Neither, NOW - MAX_AGE - 1);
        let plan = planned(&signed, &marks, std::slice::from_ref(&old), &[]);
        assert_eq!(role_of(&plan, bob()), Some(role.clone()));
        let recent = statement(bob(), ProviderRole::Neither, NOW - MAX_AGE);
        assert_eq!(role_of(&planned(&signed, &marks, &[recent], &[]), bob()), None);
      }
    }

    #[test]
    fn an_old_loss_does_not_win_over_a_newer_token_with_the_role() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 100),
        &[
          statement(bob(), ProviderRole::Neither, NOW - 50),
          statement(bob(), ProviderRole::Admin, NOW - 40),
        ],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn a_demotion_of_the_last_owner_to_admin_is_refused_too() {
      let mut signed = RosterDocument::default();
      signed.members.push(member(bob(), RosterRole::Owner));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[],
      );
      assert!(!plan.has_changes());
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn the_last_owner_stays_and_somebody_hears_about_it() {
      let mut signed = RosterDocument::default();
      signed.members.push(member(bob(), RosterRole::Owner));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[statement(bob(), ProviderRole::Neither, NOW)],
        &[],
      );
      assert!(!plan.has_changes());
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn a_handover_in_one_round_keeps_an_owner() {
      let mut signed = RosterDocument::default();
      signed.members.push(member(bob(), RosterRole::Owner));
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        &[
          statement(bob(), ProviderRole::Neither, NOW),
          statement(carol(), ProviderRole::Owner, NOW),
        ],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(role_of(&plan, carol()), Some(RosterRole::Owner));
    }
  }

  mod the_servers_word {
    use super::*;

    #[test]
    fn takes_an_admin_off_only_after_standing_and_a_stored_token_does_not_bring_them_back() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let marks = marked(bob(), "sub-2", NOW - 10);
      let gone = [withdrawn(bob(), WithdrawnReason::RoleGone)];
      let stored = statement(bob(), ProviderRole::Admin, NOW - 5);

      // Just reported: nothing happens, and the owners can see it.
      let first = planned(&signed, &marks, std::slice::from_ref(&stored), &gone);
      assert!(!first.has_changes());
      assert_eq!(first.reported, vec![bob()]);
      assert_eq!(first.reports[&bob()], NOW);

      // Standing for the grace time.
      let later = planned_at(
        &signed,
        &marks,
        std::slice::from_ref(&stored),
        &gone,
        &first.reports,
        None,
        NOW + GRACE,
      );
      assert_eq!(role_of(&later, bob()), None);
      assert_eq!(later.changes.last().unwrap().basis, AutomaticBasis::ServerReport);
      assert_eq!(later.marks[&bob()].issued_at, NOW + GRACE);

      let next = RosterDocument {
        members: later.members.clone(),
        ..RosterDocument::default()
      };
      assert!(
        !planned_at(
          &next,
          &later.marks,
          &[stored],
          &[],
          &BTreeMap::new(),
          None,
          NOW + GRACE
        )
        .has_changes()
      );
      let fresh = statement(bob(), ProviderRole::Admin, NOW + GRACE + 60);
      let back = planned_at(
        &next,
        &later.marks,
        &[fresh],
        &[],
        &BTreeMap::new(),
        None,
        NOW + GRACE + 60,
      );
      assert_eq!(role_of(&back, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn a_token_issued_after_the_report_beats_it_while_it_goes_on() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let marks = marked(bob(), "sub-2", NOW - 10);
      let gone = [withdrawn(bob(), WithdrawnReason::RoleGone)];
      let reports = BTreeMap::from([(bob(), NOW - GRACE)]);
      let newer = statement(bob(), ProviderRole::Admin, NOW - GRACE + 1);
      let plan = planned_at(&signed, &marks, &[newer], &gone, &reports, None, NOW);
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
      assert!(plan.reports.is_empty());
    }

    #[test]
    fn takes_only_one_admin_off_per_interval() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      signed.members.push(member(carol(), RosterRole::Admin));
      let marks = BTreeMap::from([
        (bob(), noted("sub-2", NOW - 10)),
        (carol(), noted("sub-3", NOW - 10)),
      ]);
      let gone = [
        withdrawn(bob(), WithdrawnReason::AccountGone),
        withdrawn(carol(), WithdrawnReason::AccountGone),
      ];
      let reports = BTreeMap::from([(bob(), NOW - GRACE), (carol(), NOW - GRACE)]);
      let plan = planned_at(&signed, &marks, &[], &gone, &reports, None, NOW);
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(role_of(&plan, carol()), Some(RosterRole::Admin));
      assert_eq!(plan.reported, vec![carol()]);
      assert_eq!(plan.maybe_last_report_removal, Some(NOW));
    }

    #[test]
    fn puts_nobody_on_the_roster() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[],
        &[withdrawn(bob(), WithdrawnReason::RoleGone)],
      );
      assert!(!plan.has_changes());
      assert_eq!(plan.members, roster().members);
    }

    #[test]
    fn never_takes_an_owner_off_and_says_so() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Owner));
      let reports = BTreeMap::from([(bob(), NOW - 10 * GRACE)]);
      for reason in [WithdrawnReason::AccountGone, WithdrawnReason::RoleGone] {
        let plan = planned_at(
          &signed,
          &marked(bob(), "sub-2", NOW - 10),
          &[],
          &[withdrawn(bob(), reason)],
          &reports,
          None,
          NOW,
        );
        assert!(!plan.has_changes());
        assert_eq!(plan.contradicted, vec![bob()]);
      }
    }

    #[test]
    fn never_takes_the_signer_off() {
      let reports = BTreeMap::from([(me(), NOW - 10 * GRACE)]);
      let plan = planned_at(
        &roster(),
        &marked(me(), "sub-1", NOW - 10),
        &[],
        &[withdrawn(me(), WithdrawnReason::AccountGone)],
        &reports,
        None,
        NOW,
      );
      assert!(!plan.has_changes());
      assert_eq!(role_of(&plan, me()), Some(RosterRole::Owner));
    }

    #[test]
    fn a_role_report_leaves_an_admin_alone_who_never_had_a_token_or_was_added_by_hand() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let gone = [withdrawn(bob(), WithdrawnReason::RoleGone)];
      let reports = BTreeMap::from([(bob(), NOW - 10 * GRACE)]);
      let unnoted = planned_at(&signed, &no_marks(), &[], &gone, &reports, None, NOW);
      assert_eq!(role_of(&unnoted, bob()), Some(RosterRole::Admin));
      let by_hand = BTreeMap::from([(
        bob(),
        ProviderMark {
          maybe_by_hand: Some(HandRole::Admin),
          ..noted("sub-2", NOW - 10)
        },
      )]);
      let plan = planned_at(&signed, &by_hand, &[], &gone, &reports, None, NOW);
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
    }
  }

  mod by_hand {
    use super::*;

    #[test]
    fn a_removal_by_hand_stands_against_a_token_with_the_role_until_the_provider_agrees() {
      let hand = BTreeMap::from([(
        bob(),
        ProviderMark {
          maybe_by_hand: Some(HandRole::Removed),
          ..noted("sub-2", NOW - 100)
        },
      )]);
      let stored = [statement(bob(), ProviderRole::Admin, NOW - 50)];
      assert!(!planned(&roster(), &hand, &stored, &[]).has_changes());

      // The provider took the role too: the edit is settled, and the provider leads again.
      let agrees = planned(
        &roster(),
        &hand,
        &[statement(bob(), ProviderRole::Neither, NOW - 40)],
        &[],
      );
      assert_eq!(agrees.marks[&bob()].maybe_by_hand, None);
      let again = planned(
        &roster(),
        &agrees.marks,
        &[statement(bob(), ProviderRole::Admin, NOW - 30)],
        &[],
      );
      assert_eq!(role_of(&again, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn somebody_added_by_hand_stays_whatever_tokens_say_until_the_provider_agrees() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let hand = BTreeMap::from([(
        bob(),
        ProviderMark {
          maybe_by_hand: Some(HandRole::Admin),
          ..noted("sub-2", NOW - 100)
        },
      )]);
      let plan = planned(
        &signed,
        &hand,
        &[statement(bob(), ProviderRole::Neither, NOW - 50)],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
      assert!(!plan.has_changes());
      let agrees = planned(
        &signed,
        &hand,
        &[statement(bob(), ProviderRole::Admin, NOW - 40)],
        &[],
      );
      assert_eq!(agrees.marks[&bob()].maybe_by_hand, None);
      let later = planned(
        &signed,
        &agrees.marks,
        &[statement(bob(), ProviderRole::Neither, NOW - 30)],
        &[],
      );
      assert_eq!(role_of(&later, bob()), None);
    }
  }

  mod noting_the_account {
    use super::*;

    #[test]
    fn a_first_token_can_not_lower_an_owner_even_naming_their_real_key() {
      // A provider admin's attribute says `Bob:BobKey`, both public, and the server lists it under Bob.
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Owner));
      let token = ProviderStatement {
        subject: "sub-of-mallory".into(),
        ..statement(bob(), ProviderRole::Admin, NOW)
      };
      let plan = planned(&signed, &no_marks(), &[token], &[]);
      assert!(!plan.has_changes());
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Owner));
      assert!(
        !plan.marks.contains_key(&bob()),
        "her account isn't noted for him"
      );
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn what_the_server_saw_is_no_statement_for_an_account_nobody_noted() {
      let owner = member(bob(), RosterRole::Owner);
      let noted_bob = noted("sub-2", NOW - 10);
      let floor = ProviderMark {
        maybe_subject: None,
        ..noted("x", NOW - 10)
      };
      for (maybe_mark, subject) in [
        (None, "sub-of-mallory"),
        (Some(&floor), "sub-of-mallory"),
        (Some(&noted_bob), "sub-of-mallory"),
      ] {
        assert_eq!(
          role_loss_statement(&owner, maybe_mark, subject.to_owned(), NOW, ProviderRole::Admin),
          None
        );
      }
      let lower = role_loss_statement(
        &owner,
        Some(&noted_bob),
        "sub-2".to_owned(),
        NOW,
        ProviderRole::Admin,
      )
      .unwrap();
      let mut signed = roster();
      signed.members.push(owner);
      let plan = planned(&signed, &marked(bob(), "sub-2", NOW - 10), &[lower], &[]);
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Admin));
    }

    #[test]
    fn what_the_server_saw_is_no_statement_unless_it_is_lower() {
      let admin = member(bob(), RosterRole::Admin);
      let mark = noted("sub-2", NOW - 10);
      for role in [ProviderRole::Admin, ProviderRole::Owner, ProviderRole::NotSaid] {
        assert_eq!(
          role_loss_statement(&admin, Some(&mark), "sub-2".to_owned(), NOW, role),
          None
        );
      }
    }

    #[test]
    fn a_token_naming_another_key_notes_nobody_on_the_roster() {
      // Mallory's attribute says `Bob:MalloryKey`, and a hostile server lists it under Bob.
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Owner));
      let forged = ProviderStatement {
        identity_key: Base64Bytes::new(vec![9; 32]),
        subject: "sub-of-mallory".into(),
        ..statement(bob(), ProviderRole::Admin, NOW)
      };
      let plan = planned(&signed, &no_marks(), &[forged], &[]);
      assert!(!plan.has_changes());
      assert_eq!(role_of(&plan, bob()), Some(RosterRole::Owner));
      assert_eq!(plan.members, signed.members);
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn a_token_naming_another_key_than_the_pinned_one_puts_nobody_on() {
      let forged = ProviderStatement {
        identity_key: Base64Bytes::new(vec![9; 32]),
        ..statement(bob(), ProviderRole::Admin, NOW)
      };
      let pinned = BTreeMap::from([(bob(), [2_u8; 32])]);
      let plan = plan_roster(RosterPlanInput {
        pinned: &pinned,
        ..input(&roster(), &no_marks(), std::slice::from_ref(&forged), &[])
      });
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(plan.contradicted, vec![bob()]);
    }

    #[test]
    fn one_account_can_not_speak_for_two_people() {
      let taken = marked(carol(), "sub-2", NOW - 10);
      let plan = planned(
        &roster(),
        &taken,
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[],
      );
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(plan.contradicted, vec![bob()]);
    }
  }

  mod provider_move {
    use teamcraft_proto::RoleProvider;

    use super::*;

    const NEW: &str = "https://new.example";

    #[test]
    fn only_a_higher_count_under_a_named_provider_is_unsettled() {
      let mut policy = RoleSigningPolicy {
        provider_moves: 2,
        ..RoleSigningPolicy::default()
      };
      assert_eq!(unsettled_provider_move(None, 0), None);
      assert_eq!(unsettled_provider_move(Some(&policy), 0), None, "not pinned yet");
      policy.maybe_provider = Some(RoleProvider {
        issuer: "https://new.example".to_owned(),
        client_id: "teamcraft".to_owned(),
      });
      assert_eq!(unsettled_provider_move(Some(&policy), 1), Some(2));
      assert_eq!(unsettled_provider_move(Some(&policy), 2), None);
      assert_eq!(unsettled_provider_move(Some(&policy), 3), None, "an older policy");
    }

    #[test]
    fn only_a_signed_move_counts_and_above_everything_seen() {
      let mut signing = RoleSigningPolicy::default();
      count_provider_move(&mut signing, 4, None, NEW);
      assert_eq!(signing.provider_moves, 0, "no move");
      signing.accepts_new_provider = true;
      count_provider_move(&mut signing, 4, Some("https://old.example"), NEW);
      assert_eq!(signing.provider_moves, 5, "an older app signed the count away");
    }

    #[test]
    fn signing_in_at_the_same_issuer_again_is_no_move() {
      let mut signing = RoleSigningPolicy {
        accepts_new_provider: true,
        ..RoleSigningPolicy::default()
      };
      count_provider_move(&mut signing, 0, Some("https://new.example/"), NEW);
      assert_eq!(signing.provider_moves, 0);
      count_provider_move(&mut signing, 0, None, NEW);
      assert_eq!(
        signing.provider_moves, 1,
        "a device that knows no provider before counts it"
      );
    }

    #[test]
    fn forgetting_keeps_the_floor_and_the_edit_by_hand() {
      let marks = BTreeMap::from([(
        bob(),
        ProviderMark {
          maybe_subject: Some("sub-b".to_owned()),
          issued_at: NOW,
          maybe_by_hand: Some(HandRole::Owner),
        },
      )]);

      let forgotten = forget_accounts(&marks);

      let mark = &forgotten[&bob()];
      assert_eq!(mark.maybe_subject, None);
      assert_eq!(mark.issued_at, NOW);
      assert_eq!(mark.maybe_by_hand, Some(HandRole::Owner));
    }

    #[test]
    fn after_forgetting_a_new_account_is_noted_without_a_contradiction() {
      let mut roster = roster();
      roster.members.push(member(bob(), RosterRole::Admin));
      let marks = forget_accounts(&marked(bob(), "sub-b", NOW - 10));
      let mut moved = statement(bob(), ProviderRole::Admin, NOW);
      moved.subject = "new-sub-b".to_owned();

      let plan = planned(&roster, &marks, &[moved], &[]);

      assert!(plan.contradicted.is_empty());
      assert_eq!(plan.marks[&bob()].maybe_subject.as_deref(), Some("new-sub-b"));
    }
  }

  #[test]
  fn role_names_come_from_the_anchor_and_the_higher_role_wins() {
    let anchor = RoleProviderAnchor {
      issuer: "https://id.example.com".into(),
      client_id: "chat".into(),
      owner_role: "chat-owner".into(),
      admin_role: "chat-admin".into(),
      roles_claim: Vec::new(),
    };
    let roles = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect::<Vec<_>>();
    assert_eq!(provider_role(None, &anchor), ProviderRole::NotSaid);
    assert_eq!(
      provider_role(Some(&roles(&["chat-admin", "chat-owner"])), &anchor),
      ProviderRole::Owner
    );
    assert_eq!(
      provider_role(Some(&roles(&["chat-admin"])), &anchor),
      ProviderRole::Admin
    );
    assert_eq!(
      provider_role(Some(&roles(&["owner", "admin"])), &anchor),
      ProviderRole::Neither
    );
    assert_eq!(provider_role(Some(&[]), &anchor), ProviderRole::Neither);
  }

  static NOTHING_PINNED: std::sync::LazyLock<BTreeMap<UserId, [u8; 32]>> =
    std::sync::LazyLock::new(BTreeMap::new);
  static NO_REPORTS: std::sync::LazyLock<BTreeMap<UserId, i64>> = std::sync::LazyLock::new(BTreeMap::new);

  fn input<'a>(
    roster: &'a RosterDocument,
    marks: &'a BTreeMap<UserId, ProviderMark>,
    statements: &'a [ProviderStatement],
    withdrawn: &'a [WithdrawnRole],
  ) -> RosterPlanInput<'a> {
    RosterPlanInput {
      roster,
      marks,
      statements,
      withdrawn,
      me: me(),
      provider_now: NOW,
      max_evidence_age_secs: MAX_AGE,
      pinned: &NOTHING_PINNED,
      reports: &NO_REPORTS,
      maybe_last_report_removal: None,
      report_grace_secs: GRACE,
      report_interval_secs: GRACE,
    }
  }

  fn planned(
    roster: &RosterDocument,
    marks: &BTreeMap<UserId, ProviderMark>,
    statements: &[ProviderStatement],
    withdrawn: &[WithdrawnRole],
  ) -> RosterPlan {
    plan_roster(input(roster, marks, statements, withdrawn))
  }

  fn planned_at(
    roster: &RosterDocument,
    marks: &BTreeMap<UserId, ProviderMark>,
    statements: &[ProviderStatement],
    withdrawn: &[WithdrawnRole],
    reports: &BTreeMap<UserId, i64>,
    maybe_last_report_removal: Option<i64>,
    provider_now: i64,
  ) -> RosterPlan {
    plan_roster(RosterPlanInput {
      reports,
      maybe_last_report_removal,
      provider_now,
      ..input(roster, marks, statements, withdrawn)
    })
  }

  fn noted(subject: &str, issued_at: i64) -> ProviderMark {
    ProviderMark {
      maybe_subject: Some(subject.to_string()),
      issued_at,
      maybe_by_hand: None,
    }
  }

  fn me() -> UserId {
    user(1)
  }

  fn bob() -> UserId {
    user(2)
  }

  fn carol() -> UserId {
    user(3)
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn roster() -> RosterDocument {
    RosterDocument {
      version: 1,
      members: vec![member(me(), RosterRole::Owner)],
      ..RosterDocument::default()
    }
  }

  fn member(user: UserId, role: RosterRole) -> RosterMember {
    RosterMember {
      user_id: user,
      role,
      identity_key: key(user),
    }
  }

  fn key(user: UserId) -> Base64Bytes {
    Base64Bytes::new(vec![user.to_string().parse::<u8>().unwrap(); 32])
  }

  fn statement(user: UserId, role: ProviderRole, issued_at: i64) -> ProviderStatement {
    ProviderStatement {
      user_id: user,
      identity_key: key(user),
      subject: format!("sub-{user}"),
      issued_at,
      role,
    }
  }

  fn withdrawn(user: UserId, reason: WithdrawnReason) -> WithdrawnRole {
    WithdrawnRole {
      user_id: user,
      reason,
    }
  }

  fn no_marks() -> BTreeMap<UserId, ProviderMark> {
    BTreeMap::new()
  }

  fn marked(user: UserId, subject: &str, issued_at: i64) -> BTreeMap<UserId, ProviderMark> {
    BTreeMap::from([(user, noted(subject, issued_at))])
  }

  fn role_of(plan: &RosterPlan, user: UserId) -> Option<RosterRole> {
    plan
      .members
      .iter()
      .find(|member| member.user_id == user)
      .map(|member| member.role.clone())
  }
}
