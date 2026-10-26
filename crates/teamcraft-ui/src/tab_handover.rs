//! One active tab per account in the browser (#744), as WhatsApp Web does it.
//!
//! The tab that holds the store's Web Lock runs the core (e2ee.md §9.4, web_tabs.rs) and is the
//! active one: it shows the app, connects, notifies and makes calls. Any other tab of the account
//! is inactive and shows a full-page notice instead of the app, with "Use here". Taking over asks
//! the active tab to hand the store on: it persists drafts and the outbox, stops its core and
//! becomes inactive itself ("TeamCraft was opened in another tab"). A call running there blocks the
//! takeover until the user agrees to end it.
//!
//! This module is the decision logic, free of the browser, so it is tested natively; web_core.rs
//! drives it.

/// The window event that asks every composer to store its draft now, before the tab hands over
/// (instead of after the typing pause, which an unmounted composer never reaches).
pub const PERSIST_DRAFTS_EVENT: &str = "teamcraft-persist-drafts";

/// Where this tab stands, as the page shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabState {
  pub role: TabRole,
  pub takeover: Takeover,
}

/// Whether the app runs in this tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabRole {
  /// The core runs here (also: a browser without Web Locks, where every tab runs its own).
  #[default]
  Active,
  /// Another tab of the account runs the app.
  Inactive(InactiveCause),
}

/// Why this tab is inactive, which decides what the notice says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InactiveCause {
  /// The account was already open in another tab when this one started.
  OpenElsewhere,
  /// This tab was active and handed the app to another tab.
  MovedAway,
}

/// A takeover this inactive tab started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Takeover {
  #[default]
  Idle,
  /// Asked the active tab; waiting for it to let go.
  Asking,
  /// The active tab can't let go without the user's say.
  Blocked(Blocker),
  /// The active tab neither let go nor answered in time (frozen, or gone with its lock held).
  TimedOut,
}

/// What keeps the active tab from handing over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocker {
  /// A call runs there; taking over would end it.
  Call,
}

/// What happened, for [`TabState::apply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabEvent {
  /// This tab got the store and runs the core.
  BecameActive,
  /// Another tab holds the store at this tab's start.
  FoundActiveElsewhere,
  /// The user pressed "Use here" (or "End call and use here").
  UseHere,
  /// The active tab refused to hand over.
  Refused(Blocker),
  /// The active tab neither let go nor answered in time.
  TimedOut,
  /// The user dismissed the warning about the call.
  Cancel,
  /// Another tab took the app from this one.
  HandedAway,
}

/// The active tab's answer to a takeover request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TakeoverAnswer {
  /// Hand over; end the running call first when there is one (the user agreed).
  HandOver {
    ends_call: bool,
  },
  Refuse(Blocker),
}

/// What a starting tab does when another tab holds the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnTaken {
  /// This tab was the active one before a reload: the old page's lock goes away with it, so wait a
  /// moment for it instead of asking the user.
  WaitForOwnLock,
  /// Another tab is active: show the notice.
  ShowNotice,
}

impl TabState {
  /// The state after `event`. Events that don't fit the state change nothing.
  pub fn apply(self, event: TabEvent) -> Self {
    match (self.role, event) {
      (_, TabEvent::BecameActive) => Self::default(),
      (TabRole::Active, TabEvent::FoundActiveElsewhere) => Self::inactive(InactiveCause::OpenElsewhere),
      (TabRole::Active, TabEvent::HandedAway) => Self::inactive(InactiveCause::MovedAway),
      (TabRole::Inactive(_), TabEvent::UseHere) if self.takeover != Takeover::Asking => Self {
        takeover: Takeover::Asking,
        ..self
      },
      (TabRole::Inactive(_), TabEvent::Refused(blocker)) if self.takeover == Takeover::Asking => Self {
        takeover: Takeover::Blocked(blocker),
        ..self
      },
      (TabRole::Inactive(_), TabEvent::TimedOut) if self.takeover == Takeover::Asking => Self {
        takeover: Takeover::TimedOut,
        ..self
      },
      (TabRole::Inactive(_), TabEvent::Cancel) => Self {
        takeover: Takeover::Idle,
        ..self
      },
      _ => self,
    }
  }

  pub fn is_active(self) -> bool {
    self.role == TabRole::Active
  }

  fn inactive(cause: InactiveCause) -> Self {
    Self {
      role: TabRole::Inactive(cause),
      takeover: Takeover::Idle,
    }
  }
}

impl Takeover {
  /// Whether the next "Use here" ends the call the active tab is in: the user saw the warning.
  pub fn ends_call(self) -> bool {
    self == Self::Blocked(Blocker::Call)
  }
}

/// The active tab's answer: a running call ends only when the asking tab says the user agreed.
pub fn answer_takeover(is_in_call: bool, ends_call: bool) -> TakeoverAnswer {
  match (is_in_call, ends_call) {
    (true, false) => TakeoverAnswer::Refuse(Blocker::Call),
    (true, true) => TakeoverAnswer::HandOver { ends_call: true },
    (false, _) => TakeoverAnswer::HandOver { ends_call: false },
  }
}

/// A reload of the active tab must not show the notice (the tab's session storage survives the
/// reload and names the account it was active for).
pub fn on_taken(maybe_was_active_for: Option<&str>, user: &str) -> OnTaken {
  if maybe_was_active_for == Some(user) {
    OnTaken::WaitForOwnLock
  } else {
    OnTaken::ShowNotice
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const OPEN_ELSEWHERE: TabState = TabState {
    role: TabRole::Inactive(InactiveCause::OpenElsewhere),
    takeover: Takeover::Idle,
  };

  mod roles {
    use super::*;

    #[test]
    fn a_tab_starts_active_and_a_second_one_is_inactive() {
      assert!(TabState::default().is_active());
      assert_eq!(
        TabState::default().apply(TabEvent::FoundActiveElsewhere),
        OPEN_ELSEWHERE
      );
    }

    #[test]
    fn the_active_tab_steps_down_when_another_takes_over() {
      let state = TabState::default().apply(TabEvent::HandedAway);
      assert_eq!(state.role, TabRole::Inactive(InactiveCause::MovedAway));
      assert_eq!(state.takeover, Takeover::Idle);
    }

    #[test]
    fn getting_the_store_makes_any_tab_active() {
      let asking = OPEN_ELSEWHERE.apply(TabEvent::UseHere);
      assert_eq!(asking.apply(TabEvent::BecameActive), TabState::default());
    }
  }

  mod takeover {
    use super::*;

    #[test]
    fn use_here_asks_and_a_refusal_shows_the_blocker() {
      let asking = OPEN_ELSEWHERE.apply(TabEvent::UseHere);
      assert_eq!(asking.takeover, Takeover::Asking);
      let blocked = asking.apply(TabEvent::Refused(Blocker::Call));
      assert_eq!(blocked.takeover, Takeover::Blocked(Blocker::Call));
      assert!(blocked.takeover.ends_call(), "the next try ends the call");
      assert_eq!(blocked.apply(TabEvent::Cancel), OPEN_ELSEWHERE);
    }

    #[test]
    fn confirming_after_the_warning_asks_again() {
      let blocked = OPEN_ELSEWHERE
        .apply(TabEvent::UseHere)
        .apply(TabEvent::Refused(Blocker::Call));
      assert_eq!(blocked.apply(TabEvent::UseHere).takeover, Takeover::Asking);
    }

    #[test]
    fn a_silent_active_tab_times_out_and_can_be_asked_again() {
      let timed_out = OPEN_ELSEWHERE.apply(TabEvent::UseHere).apply(TabEvent::TimedOut);
      assert_eq!(timed_out.takeover, Takeover::TimedOut);
      assert!(!timed_out.takeover.ends_call());
      assert_eq!(timed_out.apply(TabEvent::UseHere).takeover, Takeover::Asking);
    }

    #[test]
    fn late_answers_change_nothing() {
      assert_eq!(
        OPEN_ELSEWHERE.apply(TabEvent::Refused(Blocker::Call)),
        OPEN_ELSEWHERE
      );
      assert_eq!(OPEN_ELSEWHERE.apply(TabEvent::TimedOut), OPEN_ELSEWHERE);
      let active = TabState::default();
      assert_eq!(active.apply(TabEvent::UseHere), active);
      assert_eq!(active.apply(TabEvent::Refused(Blocker::Call)), active);
    }

    #[test]
    fn a_second_press_while_asking_changes_nothing() {
      let asking = OPEN_ELSEWHERE.apply(TabEvent::UseHere);
      assert_eq!(asking.apply(TabEvent::UseHere), asking);
    }
  }

  mod answers {
    use super::*;

    #[test]
    fn a_call_blocks_until_the_user_agrees_to_end_it() {
      assert_eq!(
        answer_takeover(true, false),
        TakeoverAnswer::Refuse(Blocker::Call)
      );
      assert_eq!(
        answer_takeover(true, true),
        TakeoverAnswer::HandOver { ends_call: true }
      );
    }

    #[test]
    fn without_a_call_the_tab_hands_over() {
      for ends_call in [false, true] {
        assert_eq!(
          answer_takeover(false, ends_call),
          TakeoverAnswer::HandOver { ends_call: false }
        );
      }
    }
  }

  mod reload {
    use super::*;

    #[test]
    fn a_reloaded_active_tab_waits_for_its_own_lock() {
      assert_eq!(on_taken(Some("42"), "42"), OnTaken::WaitForOwnLock);
    }

    #[test]
    fn a_new_tab_or_another_account_shows_the_notice() {
      assert_eq!(on_taken(None, "42"), OnTaken::ShowNotice);
      assert_eq!(on_taken(Some("7"), "42"), OnTaken::ShowNotice);
    }
  }
}
