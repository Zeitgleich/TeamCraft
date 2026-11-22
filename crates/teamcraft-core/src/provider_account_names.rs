//! What the identity provider calls the accounts behind a contradiction, for the owners (e2ee.md §3.1.4,
//! issue #2241). The signed roster notes only an account's subject (`sub`): enough to decide, but an
//! owner who has to tell which of two accounts is the person's needs the names the provider shows.
//!
//! Those names are personal data and change, so they never go into the roster or anywhere else that
//! leaves the device: an owner's app keeps them in its encrypted local store, from the verified tokens
//! its own signing rounds read. The noted account's names are only known from the token that got it
//! noted (the server keeps no token of a noted account), so they are kept from then on for every noted
//! account, as long as the roster notes it; the contradicting account's for as long as the
//! contradiction stands. Another owner's device that didn't see the noting token knows only the subject.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::attestation::AccountNames;
use teamcraft_proto::UserId;

use crate::role_signing::ProviderMark;

/// The accounts this owner's device saw verified tokens of, per person; local state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAccountNames {
  #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
  pub people: BTreeMap<UserId, PersonAccounts>,
}

/// One person's accounts: the one the roster notes, and another one that signs in for them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonAccounts {
  #[serde(rename = "noted", default, skip_serializing_if = "Option::is_none")]
  pub maybe_noted: Option<SeenAccount>,
  #[serde(rename = "contradicting", default, skip_serializing_if = "Option::is_none")]
  pub maybe_contradicting: Option<SeenAccount>,
}

/// An account at the provider, as its newest verified token this device read named it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenAccount {
  #[serde(rename = "sub")]
  pub subject: String,
  /// The token's `iat` (provider's clock): a newer token of the same account replaces the names.
  pub issued_at: i64,
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  #[serde(rename = "email", default, skip_serializing_if = "Option::is_none")]
  pub maybe_email: Option<String>,
}

/// A token of `user_id` that passed every check in this round.
#[derive(Clone, Copy, Debug)]
pub struct SeenToken<'a> {
  pub user_id: UserId,
  pub subject: &'a str,
  pub issued_at: i64,
  pub names: &'a AccountNames,
}

impl ProviderAccountNames {
  /// Takes the names of `seen` for the accounts that matter by the roster's `marks` (after this
  /// round's plan) and its `contradicted` people, and forgets every other: an account no longer noted,
  /// a contradiction that no longer stands. Whether anything changed.
  pub fn update(
    &mut self,
    marks: &BTreeMap<UserId, ProviderMark>,
    contradicted: &[UserId],
    seen: &[SeenToken<'_>],
  ) -> bool {
    let before = self.clone();
    for token in seen {
      self.take(marks, contradicted, token);
    }
    self.people.retain(|user, accounts| {
      let maybe_noted = marks.get(user).and_then(|mark| mark.maybe_subject.as_deref());
      if accounts
        .maybe_noted
        .as_ref()
        .is_some_and(|account| Some(account.subject.as_str()) != maybe_noted)
      {
        accounts.maybe_noted = None;
      }
      if !contradicted.contains(user) {
        accounts.maybe_contradicting = None;
      }
      accounts.maybe_noted.is_some() || accounts.maybe_contradicting.is_some()
    });
    *self != before
  }

  /// An owner corrected `user`'s account: nothing about it is worth keeping.
  pub fn forget(&mut self, user: UserId) -> bool {
    self.people.remove(&user).is_some()
  }

  fn take(&mut self, marks: &BTreeMap<UserId, ProviderMark>, contradicted: &[UserId], token: &SeenToken<'_>) {
    let maybe_noted = marks
      .get(&token.user_id)
      .and_then(|mark| mark.maybe_subject.as_deref());
    let is_noted = maybe_noted == Some(token.subject);
    if !is_noted && !contradicted.contains(&token.user_id) {
      return;
    }
    let accounts = self.people.entry(token.user_id).or_default();
    let slot = if is_noted {
      &mut accounts.maybe_noted
    } else {
      &mut accounts.maybe_contradicting
    };
    let is_newer = match slot {
      None => true,
      Some(account) if account.subject != token.subject => true,
      // A newer token of the same account that names it: the provider may have renamed it.
      Some(account) => token.issued_at > account.issued_at && !token.names.is_empty(),
    };
    if is_newer {
      *slot = Some(SeenAccount {
        subject: token.subject.to_string(),
        issued_at: token.issued_at,
        maybe_username: token.names.maybe_username.clone(),
        maybe_email: token.names.maybe_email.clone(),
      });
    }
  }
}

// Personal data: never in a log line, also not through a `?` of a containing type.
impl std::fmt::Debug for SeenAccount {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter
      .debug_struct("SeenAccount")
      .field("issued_at", &self.issued_at)
      .finish_non_exhaustive()
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id as _, Snowflake};

  use super::*;

  const NOTED: &str = "noted-sub";
  const OTHER: &str = "other-sub";

  #[test]
  fn keeps_the_names_of_a_noted_account() {
    let mut names = ProviderAccountNames::default();
    let marks = marks_noting(NOTED);
    let jdoe = named("jdoe");
    assert!(names.update(&marks, &[], &[token(NOTED, 10, &jdoe)]));
    let noted = names.people[&bob()].maybe_noted.as_ref().unwrap();
    assert_eq!(noted.maybe_username.as_deref(), Some("jdoe"));
    assert!(names.people[&bob()].maybe_contradicting.is_none());
    // The same round again changes nothing.
    assert!(!names.update(&marks, &[], &[token(NOTED, 10, &jdoe)]));
  }

  #[test]
  fn keeps_the_contradicting_account_only_while_the_contradiction_stands() {
    let mut names = ProviderAccountNames::default();
    let marks = marks_noting(NOTED);
    let real = named("real");
    names.update(&marks, &[bob()], &[token(OTHER, 20, &real)]);
    let other = names.people[&bob()].maybe_contradicting.as_ref().unwrap();
    assert_eq!(other.subject, OTHER);
    // A round in which the server held the token back keeps it while the contradiction stands.
    assert!(!names.update(&marks, &[bob()], &[]));
    assert!(names.update(&marks, &[], &[]));
    assert!(names.people.is_empty());
  }

  #[test]
  fn ignores_tokens_of_accounts_that_matter_for_nothing() {
    let mut names = ProviderAccountNames::default();
    let somebody = named("somebody");
    // Not noted, not contradicted: nothing is kept about it.
    assert!(!names.update(&marks_noting(NOTED), &[], &[token(OTHER, 20, &somebody)]));
    assert!(!names.update(&BTreeMap::new(), &[], &[token(NOTED, 20, &somebody)]));
    assert!(names.people.is_empty());
  }

  #[test]
  fn forgets_the_noted_names_once_the_roster_forgets_the_account() {
    let mut names = ProviderAccountNames::default();
    let jdoe = named("jdoe");
    names.update(&marks_noting(NOTED), &[], &[token(NOTED, 10, &jdoe)]);
    let forgotten = BTreeMap::from([(
      bob(),
      ProviderMark {
        maybe_subject: None,
        issued_at: 30,
        maybe_by_hand: None,
      },
    )]);
    assert!(names.update(&forgotten, &[], &[]));
    assert!(names.people.is_empty());
  }

  #[test]
  fn a_newer_token_renames_and_an_older_or_nameless_one_does_not() {
    let mut names = ProviderAccountNames::default();
    let marks = marks_noting(NOTED);
    let (old, new, nameless) = (named("old"), named("new"), AccountNames::default());
    names.update(&marks, &[], &[token(NOTED, 10, &old)]);
    assert!(!names.update(&marks, &[], &[token(NOTED, 5, &new)]));
    assert!(!names.update(&marks, &[], &[token(NOTED, 20, &nameless)]));
    assert!(names.update(&marks, &[], &[token(NOTED, 20, &new)]));
    let noted = names.people[&bob()].maybe_noted.as_ref().unwrap();
    assert_eq!(noted.maybe_username.as_deref(), Some("new"));
  }

  #[test]
  fn forget_drops_everything_about_the_person() {
    let mut names = ProviderAccountNames::default();
    let jdoe = named("jdoe");
    names.update(
      &marks_noting(NOTED),
      &[bob()],
      &[token(NOTED, 10, &jdoe), token(OTHER, 20, &jdoe)],
    );
    assert!(names.forget(bob()));
    assert!(!names.forget(bob()));
  }

  #[test]
  fn debug_never_prints_names_or_accounts() {
    let account = SeenAccount {
      subject: NOTED.to_string(),
      issued_at: 1,
      maybe_username: Some("jdoe".to_string()),
      maybe_email: Some("j@example.com".to_string()),
    };
    let printed = format!("{account:?}");
    assert!(!printed.contains("jdoe") && !printed.contains("example") && !printed.contains(NOTED));
  }

  fn bob() -> UserId {
    UserId::from_snowflake(Snowflake::new(2).unwrap())
  }

  fn named(username: &str) -> AccountNames {
    AccountNames {
      maybe_username: Some(username.to_string()),
      maybe_email: None,
    }
  }

  fn marks_noting(subject: &str) -> BTreeMap<UserId, ProviderMark> {
    BTreeMap::from([(
      bob(),
      ProviderMark {
        maybe_subject: Some(subject.to_string()),
        issued_at: 1,
        maybe_by_hand: None,
      },
    )])
  }

  fn token<'a>(subject: &'a str, issued_at: i64, names: &'a AccountNames) -> SeenToken<'a> {
    SeenToken {
      user_id: bob(),
      subject,
      issued_at,
      names,
    }
  }
}
