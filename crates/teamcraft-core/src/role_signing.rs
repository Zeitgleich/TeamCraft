//! What the admin roster should say, given what the identity provider signed (e2ee.md §3.1.3, issue
//! #1276). Everything here is a pure decision: [`plan_roster`] takes verified statements and the
//! verified roster and returns the next roster. Fetching and verifying are in
//! `client/role_signing.rs`.
//!
//! The rules that matter:
//!
//! - Only a token puts anybody on the roster or gives them a higher role. The server's word can only
//!   take somebody off.
//! - Tokens are ordered by the provider's own `iat`, per person, and a person's mark only moves
//!   forward. A replayed older token after a demotion is not newer than the mark and changes nothing.
//!   No device's clock is read: "how old" is judged against the newest token this device itself got
//!   from the provider.
//! - A token without the role claim says nothing: the realm may just not map roles into ID tokens.
//! - A person's account at the provider (`sub`) is noted the first time their token counts. A token of
//!   another account can't speak for them, so nobody can demote somebody with a token of their own that
//!   names the other person's user ID and key, both of which are public.

use std::collections::{BTreeMap, BTreeSet};

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
  /// The server reports the role or the account gone. Only ever takes somebody off.
  ServerReport,
  #[serde(other)]
  Unknown,
}

/// The provider whose tokens count, and which of its client roles mean what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleProviderAnchor {
  pub issuer: String,
  pub client_id: String,
  pub owner_role: String,
  pub admin_role: String,
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
    })
  }

  /// While the signed policy names no provider: the one this device got its own token from, read with
  /// the role names the policy gives, else the ones everybody starts with. Never the server's word.
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

/// The next roster and marks. Applied in the same way by every owner's app, so several of them
/// arrive at the same document and the loser of the version race finds nothing left to do.
pub fn plan_roster(input: RosterPlanInput<'_>) -> RosterPlan {
  let mut plan = RosterPlan {
    members: input.roster.members.clone(),
    marks: input.marks.clone(),
    ..RosterPlan::default()
  };
  let withdrawn_users: BTreeSet<UserId> = input.withdrawn.iter().map(|gone| gone.user_id).collect();
  let mut statements: Vec<&ProviderStatement> = input.statements.iter().collect();
  statements.sort_by_key(|statement| (statement.user_id, statement.issued_at));
  // Additions first, so a handover through the same run keeps an owner on the roster.
  for statement in statements.iter().filter(|statement| {
    matches!(statement.role, ProviderRole::Owner | ProviderRole::Admin)
      && !withdrawn_users.contains(&statement.user_id)
  }) {
    apply_role(&mut plan, statement, &input);
  }
  for statement in statements
    .iter()
    .filter(|statement| statement.role == ProviderRole::Neither)
  {
    apply_removal(&mut plan, statement);
  }
  for gone in input.withdrawn {
    apply_withdrawal(&mut plan, gone, &input);
  }
  plan.contradicted.sort();
  plan.contradicted.dedup();
  plan
}

/// A token that gives somebody a role.
fn apply_role(plan: &mut RosterPlan, statement: &ProviderStatement, input: &RosterPlanInput<'_>) {
  let Some(is_new) = is_news(plan, statement) else {
    return;
  };
  let wanted = match statement.role {
    ProviderRole::Owner => RosterRole::Owner,
    _ => RosterRole::Admin,
  };
  let maybe_member = plan
    .members
    .iter()
    .position(|member| member.user_id == statement.user_id);
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
  // Nothing changes for the person: only a first token has to be written down, so that their account
  // is known when a later one takes the role away.
  let has_subject = plan
    .marks
    .get(&statement.user_id)
    .is_some_and(|mark| mark.maybe_subject.is_some());
  if !has_subject {
    note_change(plan, statement, AutomaticChangeKind::Noted);
  }
}

/// A token that lists no role for somebody on the roster.
fn apply_removal(plan: &mut RosterPlan, statement: &ProviderStatement) {
  let Some(index) = plan
    .members
    .iter()
    .position(|member| member.user_id == statement.user_id)
  else {
    return;
  };
  // Without a known account any member could speak for this person: their user ID and key are
  // public. Their first token with the role, or an owner's edit, or the server's report settle it.
  let has_subject = plan
    .marks
    .get(&statement.user_id)
    .is_some_and(|mark| mark.maybe_subject.is_some());
  if !has_subject {
    return;
  }
  if is_news(plan, statement) != Some(true) {
    return;
  }
  if would_leave_no_owner(plan, index) {
    plan.contradicted.push(statement.user_id);
    return;
  }
  plan.members.remove(index);
  note_change(plan, statement, AutomaticChangeKind::Removed);
}

/// The server reports the role or the account gone. Enough to take somebody off, as a server can
/// withhold anything anyway and the person's next token brings them back; never the signer.
fn apply_withdrawal(plan: &mut RosterPlan, gone: &WithdrawnRole, input: &RosterPlanInput<'_>) {
  if gone.user_id == input.me {
    return;
  }
  let Some(index) = plan
    .members
    .iter()
    .position(|member| member.user_id == gone.user_id)
  else {
    return;
  };
  // A server that only says an owner's role went could cut the owners down one by one while an honest
  // role list lags: an owner goes only with the account, or by the provider's own token.
  if plan.members[index].role == RosterRole::Owner && gone.reason != WithdrawnReason::AccountGone {
    return;
  }
  if would_leave_no_owner(plan, index) {
    plan.contradicted.push(gone.user_id);
    return;
  }
  plan.members.remove(index);
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
    },
  );
  plan.changes.push(AutomaticChange {
    user_id: gone.user_id,
    change: AutomaticChangeKind::Removed,
    basis: AutomaticBasis::ServerReport,
    maybe_issued_at: None,
  });
}

/// `Some(true)` if `statement` is newer than the person's mark, `Some(false)` if it is the first
/// token to say the same as the mark, `None` if it can't change anything: not newer, or another
/// account (which goes on the list of contradictions).
fn is_news(plan: &mut RosterPlan, statement: &ProviderStatement) -> Option<bool> {
  let Some(mark) = plan.marks.get(&statement.user_id) else {
    return Some(true);
  };
  if mark
    .maybe_subject
    .as_ref()
    .is_some_and(|subject| *subject != statement.subject)
  {
    plan.contradicted.push(statement.user_id);
    return None;
  }
  if statement.issued_at > mark.issued_at {
    return Some(true);
  }
  // Written down by a report of the server before any token counted: this token is older than that.
  (mark.maybe_subject.is_none() && statement.issued_at == mark.issued_at).then_some(false)
}

fn note_change(plan: &mut RosterPlan, statement: &ProviderStatement, change: AutomaticChangeKind) {
  plan.marks.insert(
    statement.user_id,
    ProviderMark {
      maybe_subject: Some(statement.subject.clone()),
      issued_at: statement.issued_at,
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

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id as _, Snowflake};

  use super::*;

  const NOW: i64 = 1_000_000;
  const MAX_AGE: i64 = 30 * 24 * 60 * 60;

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
    fn takes_an_admin_off_and_a_stored_token_does_not_bring_them_back() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Admin));
      let gone = withdrawn(bob(), WithdrawnReason::RoleGone);
      let stored = statement(bob(), ProviderRole::Admin, NOW - 5);
      let plan = planned(
        &signed,
        &marked(bob(), "sub-2", NOW - 10),
        std::slice::from_ref(&stored),
        &[gone],
      );
      assert_eq!(role_of(&plan, bob()), None);
      assert_eq!(plan.changes.last().unwrap().basis, AutomaticBasis::ServerReport);
      assert_eq!(plan.marks[&bob()].issued_at, NOW);

      let next = RosterDocument {
        members: plan.members.clone(),
        ..RosterDocument::default()
      };
      assert!(!planned(&next, &plan.marks, &[stored], &[]).has_changes());
      // The person's next token, issued after that, does.
      let fresh = statement(bob(), ProviderRole::Admin, NOW + 60);
      assert_eq!(
        role_of(&planned(&next, &plan.marks, &[fresh], &[]), bob()),
        Some(RosterRole::Admin)
      );
    }

    #[test]
    fn puts_nobody_on_the_roster() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[],
        &[withdrawn(bob(), WithdrawnReason::RoleGone)],
      );
      assert_eq!(
        plan,
        RosterPlan {
          members: roster().members,
          ..RosterPlan::default()
        }
      );
    }

    #[test]
    fn takes_an_owner_off_only_with_the_account() {
      let mut signed = roster();
      signed.members.push(member(bob(), RosterRole::Owner));
      let role_gone = planned(
        &signed,
        &no_marks(),
        &[],
        &[withdrawn(bob(), WithdrawnReason::RoleGone)],
      );
      assert!(!role_gone.has_changes());
      let account_gone = planned(
        &signed,
        &no_marks(),
        &[],
        &[withdrawn(bob(), WithdrawnReason::AccountGone)],
      );
      assert_eq!(role_of(&account_gone, bob()), None);
    }

    #[test]
    fn never_takes_the_signer_off() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[],
        &[withdrawn(me(), WithdrawnReason::AccountGone)],
      );
      assert!(!plan.has_changes());
      assert_eq!(role_of(&plan, me()), Some(RosterRole::Owner));
    }

    #[test]
    fn a_token_does_not_win_over_it_in_the_same_round() {
      let plan = planned(
        &roster(),
        &no_marks(),
        &[statement(bob(), ProviderRole::Admin, NOW)],
        &[withdrawn(bob(), WithdrawnReason::AccountGone)],
      );
      assert_eq!(role_of(&plan, bob()), None);
    }
  }

  #[test]
  fn role_names_come_from_the_anchor_and_the_higher_role_wins() {
    let anchor = RoleProviderAnchor {
      issuer: "https://id.example.com".into(),
      client_id: "chat".into(),
      owner_role: "chat-owner".into(),
      admin_role: "chat-admin".into(),
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

  fn planned(
    roster: &RosterDocument,
    marks: &BTreeMap<UserId, ProviderMark>,
    statements: &[ProviderStatement],
    withdrawn: &[WithdrawnRole],
  ) -> RosterPlan {
    plan_roster(RosterPlanInput {
      roster,
      marks,
      statements,
      withdrawn,
      me: me(),
      provider_now: NOW,
      max_evidence_age_secs: MAX_AGE,
    })
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
    BTreeMap::from([(
      user,
      ProviderMark {
        maybe_subject: Some(subject.to_string()),
        issued_at,
      },
    )])
  }

  fn role_of(plan: &RosterPlan, user: UserId) -> Option<RosterRole> {
    plan
      .members
      .iter()
      .find(|member| member.user_id == user)
      .map(|member| member.role.clone())
  }
}
