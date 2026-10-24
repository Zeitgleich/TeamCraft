//! Feedback for user actions (issue #322): a failed action is never silent, and a button that starts
//! work shows that it runs.
//!
//! - [`report_failure`] shows a failed action as an error toast with what to do next and the
//!   technical details. Backends call it from wherever the failure surfaces; the shell installs the
//!   toast with [`set_failure_sink`].
//! - [`PendingAction`] tracks one button's work until the core answered, including backend actions
//!   that are fire-and-forget for the UI: the backend keeps the [`ActionTicket`] of the action and
//!   drops it once the answer is in.
//!
//! Both answer from outside the reactive tree (after an `.await`), when the component that started
//! the work may be gone; they only touch signals with `try_*` (see `reactive.rs`).

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::ErrorCode;

pub use teamcraft_client_api::failure::{ActionFailure, FailureKind};

use crate::i18n::I18n;

/// Where failures go, see [`set_failure_sink`].
type FailureSink = Rc<dyn Fn(ActionFailure)>;
/// What a finished [`ActionTicket`] calls, with the first failure.
type OnDone = Box<dyn FnOnce(Option<ActionFailure>)>;

thread_local! {
  /// The installed sink and the number it was installed under.
  static SINK: RefCell<Option<(u64, FailureSink)>> = RefCell::new(None);
  static NEXT_SINK: Cell<u64> = const { Cell::new(0) };
  static CURRENT_TICKET: RefCell<Option<Rc<ActionTicket>>> = const { RefCell::new(None) };
}

/// Shows a failed user action. Before the shell is up (or after it is gone) the failure is only
/// logged.
pub fn report_failure(failure: ActionFailure) {
  tracing::warn!(detail = %failure.detail, "action failed");
  // Cloned out of the cell: the sink may start work that reports again.
  let maybe_sink = SINK.with(|sink| sink.borrow().as_ref().map(|(_, sink)| sink.clone()));
  if let Some(sink) = maybe_sink {
    sink(failure);
  }
}

/// Shows a failure of work the core does in the background for a read (loading history, a search
/// segment), which no button waits for. Network failures stay quiet: the offline banner already
/// says so, and scrolling while offline must not stack up toasts.
pub fn report_background_failure(failure: ActionFailure) {
  if failure.kind == FailureKind::Network {
    tracing::debug!(detail = %failure.detail, "background command failed offline");
    return;
  }
  report_failure(failure);
}

/// Where [`report_failure`] sends failures until the current reactive owner is cleaned up.
pub fn set_failure_sink(sink: impl Fn(ActionFailure) + 'static) {
  let number = NEXT_SINK.with(|next| next.replace(next.get() + 1));
  SINK.with(|slot| *slot.borrow_mut() = Some((number, Rc::new(sink))));
  on_cleanup(move || {
    // Only the sink of this owner: a newer shell may have installed its own meanwhile.
    SINK.with(|slot| {
      let mut slot = slot.borrow_mut();
      if slot.as_ref().is_some_and(|(current, _)| *current == number) {
        *slot = None;
      }
    });
  });
}

/// The localized message for a failure: what happened and what to do next.
pub fn failure_message(i18n: I18n, failure: &ActionFailure) -> String {
  i18n.t(failure_message_key(failure))
}

/// The i18n key of a failure's message; unknown causes get the generic one (with the details).
pub fn failure_message_key(failure: &ActionFailure) -> &'static str {
  match &failure.kind {
    FailureKind::Network => "error-network",
    FailureKind::Other => "error-generic",
    FailureKind::Server(code) => match code {
      ErrorCode::NotSpaceMember => "error-not-space-member",
      ErrorCode::Forbidden | ErrorCode::NoTeamcraftRole => "error-forbidden",
      ErrorCode::Unauthorized | ErrorCode::InvalidToken => "error-signed-out",
      ErrorCode::RateLimited => "error-rate-limited",
      ErrorCode::PayloadTooLarge => "error-too-large",
      ErrorCode::Unavailable | ErrorCode::Internal => "error-server-trouble",
      ErrorCode::Conflict | ErrorCode::EpochConflict | ErrorCode::VersionConflict => "error-conflict",
      ErrorCode::LastOwner => "admin-error-last-owner",
      // All `*_not_found` codes, including ones this version doesn't know yet.
      other if other.as_str().ends_with("_not_found") => "error-not-found",
      _ => "error-generic",
    },
  }
}

/// One button's work: pending while it runs, so the button can show progress and refuse a second
/// click. Owned by the component that creates it; answers that arrive after the component is gone
/// are dropped.
#[derive(Clone, Copy)]
pub struct PendingAction {
  is_pending: RwSignal<bool>,
  maybe_failure: RwSignal<Option<ActionFailure>>,
}

/// The completion of the backend actions started inside one [`PendingAction::run`]. A backend that
/// answers later keeps a clone (from [`ActionTicket::current`]) until its answer is in; the work is
/// done when the last clone is dropped.
pub struct ActionTicket {
  maybe_failure: RefCell<Option<ActionFailure>>,
  on_done: RefCell<Option<OnDone>>,
}

impl PendingAction {
  pub fn new() -> Self {
    Self {
      is_pending: RwSignal::new(false),
      maybe_failure: RwSignal::new(None),
    }
  }

  /// Whether the work runs (reactive).
  pub fn is_pending(&self) -> bool {
    self.is_pending.get()
  }

  /// The failure of the last run, if it failed (reactive). It has been shown as a toast already;
  /// this is for forms that stay open or mark the field.
  pub fn maybe_failure(&self) -> Option<ActionFailure> {
    self.maybe_failure.get()
  }

  /// Runs `start`, which starts backend actions (`state.write(..)`), and stays pending until the
  /// backend answered all of them. Ignored while the previous run is still pending.
  pub fn run(&self, start: impl FnOnce()) {
    self.run_or_else(start, |_| {});
  }

  /// [`Self::run`], calling `on_failure` when an action failed, even after the component is gone
  /// (to undo what was prepared for its success elsewhere in the app).
  pub fn run_or_else(&self, start: impl FnOnce(), on_failure: impl FnOnce(&ActionFailure) + 'static) {
    if !self.begin() {
      return;
    }
    let this = *self;
    let ticket = Rc::new(ActionTicket::new(move |maybe_failure| {
      if let Some(failure) = &maybe_failure {
        on_failure(failure);
      }
      this.finish(maybe_failure);
    }));
    let maybe_outer = CURRENT_TICKET.with(|current| current.replace(Some(ticket)));
    start();
    // Dropping our clone finishes the run unless a backend kept the ticket.
    CURRENT_TICKET.with(|current| current.replace(maybe_outer));
  }

  /// [`Self::run`], calling `on_success` once every action succeeded (closing the dialog, clearing
  /// the form), even after the component is gone: what it touches must tolerate that
  /// (`try_set`, app-level state).
  pub fn run_and_then(&self, start: impl FnOnce(), on_success: impl FnOnce() + 'static) {
    self.run_with(start, move |maybe_failure| {
      if maybe_failure.is_none() {
        on_success();
      }
    });
  }

  /// [`Self::run`], calling `on_done` with the failure, if any, once the backend answered; like
  /// [`Self::run_and_then`] it may run after the component is gone.
  pub fn run_with(&self, start: impl FnOnce(), on_done: impl FnOnce(Option<&ActionFailure>) + 'static) {
    if !self.begin() {
      return;
    }
    let this = *self;
    let ticket = Rc::new(ActionTicket::new(move |maybe_failure| {
      on_done(maybe_failure.as_ref());
      this.finish(maybe_failure);
    }));
    let maybe_outer = CURRENT_TICKET.with(|current| current.replace(Some(ticket)));
    start();
    CURRENT_TICKET.with(|current| current.replace(maybe_outer));
  }

  /// Runs awaited work (a REST call, an upload); a failure is reported like a failed backend
  /// action. Ignored while the previous run is still pending.
  pub fn spawn(&self, work: impl Future<Output = Result<(), ActionFailure>> + 'static) {
    if !self.begin() {
      return;
    }
    let this = *self;
    spawn_local(async move {
      let maybe_failure = work.await.err();
      if let Some(failure) = &maybe_failure {
        report_failure(failure.clone());
      }
      this.finish(maybe_failure);
    });
  }

  fn begin(&self) -> bool {
    if self.is_pending.try_get_untracked().unwrap_or(true) {
      return false;
    }
    self.is_pending.set(true);
    self.maybe_failure.set(None);
    true
  }

  /// Called when the work is done, possibly after the component is gone: `try_set` then does
  /// nothing instead of panicking.
  fn finish(&self, maybe_failure: Option<ActionFailure>) {
    let _ = self.is_pending.try_set(false);
    let _ = self.maybe_failure.try_set(maybe_failure);
  }
}

impl Default for PendingAction {
  fn default() -> Self {
    Self::new()
  }
}

impl ActionTicket {
  fn new(on_done: impl FnOnce(Option<ActionFailure>) + 'static) -> Self {
    Self {
      maybe_failure: RefCell::new(None),
      on_done: RefCell::new(Some(Box::new(on_done))),
    }
  }

  /// The ticket of the [`PendingAction::run`] that is starting actions right now, if any.
  pub fn current() -> Option<Rc<Self>> {
    CURRENT_TICKET.with(|current| current.borrow().clone())
  }

  /// Records that one of the actions failed; the first failure is kept.
  pub fn fail(&self, failure: ActionFailure) {
    self.maybe_failure.borrow_mut().get_or_insert(failure);
  }
}

impl Drop for ActionTicket {
  fn drop(&mut self) {
    if let Some(on_done) = self.on_done.get_mut().take() {
      on_done(self.maybe_failure.get_mut().take());
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod messages {
    use super::*;

    #[test]
    fn a_channel_add_outside_the_space_says_to_invite_first() {
      let failure = ActionFailure::new(FailureKind::Server(ErrorCode::NotSpaceMember), "x");
      assert_eq!(failure_message_key(&failure), "error-not-space-member");
    }

    #[test]
    fn unknown_codes_and_other_errors_fall_back_to_the_generic_message() {
      for kind in [
        FailureKind::Other,
        FailureKind::Server(ErrorCode::Unknown("brand_new".into())),
        FailureKind::Server(ErrorCode::InvalidRequest),
      ] {
        let failure = ActionFailure::new(kind, "x");
        assert_eq!(failure_message_key(&failure), "error-generic");
      }
    }

    #[test]
    fn not_found_codes_share_one_message_even_unknown_ones() {
      for code in [
        ErrorCode::ChannelNotFound,
        ErrorCode::Unknown("poll_not_found".into()),
      ] {
        let failure = ActionFailure::new(FailureKind::Server(code), "x");
        assert_eq!(failure_message_key(&failure), "error-not-found");
      }
    }

    #[test]
    fn every_message_key_is_translated() {
      let en = include_str!("../i18n/en.ftl");
      let de = include_str!("../i18n/de.ftl");
      let kinds = [
        FailureKind::Network,
        FailureKind::Other,
        FailureKind::Server(ErrorCode::NotSpaceMember),
        FailureKind::Server(ErrorCode::Forbidden),
        FailureKind::Server(ErrorCode::InvalidToken),
        FailureKind::Server(ErrorCode::RateLimited),
        FailureKind::Server(ErrorCode::PayloadTooLarge),
        FailureKind::Server(ErrorCode::Unavailable),
        FailureKind::Server(ErrorCode::Conflict),
        FailureKind::Server(ErrorCode::LastOwner),
        FailureKind::Server(ErrorCode::UserNotFound),
      ];
      for kind in kinds {
        let key = failure_message_key(&ActionFailure::new(kind, "x"));
        for (name, source) in [("en", en), ("de", de)] {
          assert!(
            source.contains(&format!("\n{key} =")),
            "{key} missing in {name}.ftl"
          );
        }
      }
    }
  }

  mod pending {
    use super::*;

    #[test]
    fn a_synchronous_backend_finishes_the_run_at_once() {
      let owner = Owner::new();
      owner.with(|| {
        let pending = PendingAction::new();
        pending.run(|| {});
        assert!(!pending.is_pending.get_untracked());
      });
    }

    #[test]
    fn a_kept_ticket_keeps_the_run_pending_until_it_is_dropped() {
      let owner = Owner::new();
      owner.with(|| {
        let pending = PendingAction::new();
        let mut maybe_kept = None;
        pending.run(|| maybe_kept = ActionTicket::current());
        assert!(pending.is_pending.get_untracked());
        // A second click while it runs does nothing.
        let second = Cell::new(false);
        pending.run(|| second.set(true));
        assert!(!second.get());
        let kept = maybe_kept.take().unwrap();
        kept.fail(ActionFailure::other("refused"));
        drop(kept);
        assert!(!pending.is_pending.get_untracked());
        assert_eq!(
          pending.maybe_failure.get_untracked(),
          Some(ActionFailure::other("refused"))
        );
      });
    }

    #[test]
    fn on_failure_runs_even_after_the_component_is_gone() {
      let failed = Rc::new(Cell::new(false));
      let owner = Owner::new();
      let kept = owner.with(|| {
        let pending = PendingAction::new();
        let mut maybe_kept = None;
        let seen = failed.clone();
        pending.run_or_else(|| maybe_kept = ActionTicket::current(), move |_| seen.set(true));
        maybe_kept.unwrap()
      });
      owner.cleanup();
      kept.fail(ActionFailure::other("refused"));
      drop(kept);
      assert!(failed.get());
    }

    /// Regression for #322: the core answered after the dialog that started the action was closed.
    #[test]
    fn an_answer_after_the_component_is_gone_does_not_panic() {
      let owner = Owner::new();
      let kept = owner.with(|| {
        let pending = PendingAction::new();
        let mut maybe_kept = None;
        pending.run(|| maybe_kept = ActionTicket::current());
        maybe_kept.unwrap()
      });
      owner.cleanup();
      drop(owner);
      kept.fail(ActionFailure::other("refused"));
      drop(kept);
    }

    #[test]
    fn no_ticket_outside_a_run() {
      let owner = Owner::new();
      owner.with(|| {
        PendingAction::new().run(|| {});
        assert!(ActionTicket::current().is_none());
      });
    }
  }

  mod sink {
    use super::*;

    #[test]
    fn failures_reach_the_sink_until_its_owner_is_cleaned_up() {
      let seen = Rc::new(Cell::new(0));
      let owner = Owner::new();
      owner.with(|| {
        let seen = seen.clone();
        set_failure_sink(move |_| seen.set(seen.get() + 1));
      });
      report_failure(ActionFailure::other("one"));
      owner.cleanup();
      report_failure(ActionFailure::other("two"));
      assert_eq!(seen.get(), 1);
    }
  }
}
