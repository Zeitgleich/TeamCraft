//! Queued UI commands the server or the network refused, for the mobile apps (#2242).
//!
//! Deleting a channel or a space runs as a queued core command; the sync loop used to log a refusal
//! (`legal_hold`, `last_channel_admin`, `import_in_progress`, …) and the app showed nothing. Now the
//! loop queues the failure here and tells the app that something changed; the app takes the
//! failures ([`crate::TeamCraft::take_command_failures`]) and words each one with its own texts,
//! chosen by the server's error code, like the web client's `failure_message_key`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_client_api::failure::{ActionFailure, FailureKind};

/// How many failures wait for the app. The oldest go first: a person who sees ten snackbars in a
/// row has no use for the eleventh.
const MAX_WAITING: usize = 8;

/// Why a queued command failed, as far as the apps word it differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CommandFailureKind {
  /// The server refused with a stable error code ([`CommandFailure::maybe_error_code`]).
  Refused,
  /// Nobody answered: the server is out of reach (or a captive portal or proxy answered). Not
  /// queued any more ([`CommandFailures::report`]): the apps show the connection state instead.
  Unreachable,
  /// Anything else (local store, encryption, a stopped core).
  Other,
}

/// A queued command that failed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CommandFailure {
  pub kind: CommandFailureKind,
  /// The server's `error_code` (`legal_hold`) for [`CommandFailureKind::Refused`].
  pub maybe_error_code: Option<String>,
  /// English text for the log. Never contains message content or secrets.
  pub detail: String,
}

impl From<ActionFailure> for CommandFailure {
  fn from(failure: ActionFailure) -> Self {
    let (kind, maybe_error_code) = match &failure.kind {
      FailureKind::Server(code) => (CommandFailureKind::Refused, Some(code.as_str().to_owned())),
      FailureKind::Network | FailureKind::CaptivePortal | FailureKind::UntrustedCertificate => {
        (CommandFailureKind::Unreachable, None)
      },
      _ => (CommandFailureKind::Other, None),
    };
    Self {
      kind,
      maybe_error_code,
      detail: failure.detail,
    }
  }
}

/// The failures that wait for the app; shared between the sync loop and the handle.
#[derive(Clone, Default)]
pub(crate) struct CommandFailures {
  waiting: Arc<Mutex<VecDeque<CommandFailure>>>,
}

impl CommandFailures {
  /// Queues a failure for the app, unless the server was only out of reach (a restart, no network):
  /// that is the connection state the app shows already, not a failed action to tell about.
  /// Returns whether it was queued.
  pub(crate) fn report(&self, failure: ActionFailure) -> bool {
    if failure.is_unreachable() {
      tracing::debug!(detail = %failure.detail, "a queued command found the server unreachable");
      return false;
    }
    self.push(failure.into());
    true
  }

  /// Queues a failure. The same one as the newest waiting is dropped: a retry that fails the same
  /// way is not news.
  pub(crate) fn push(&self, failure: CommandFailure) {
    let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
    if waiting.back() == Some(&failure) {
      return;
    }
    if waiting.len() >= MAX_WAITING {
      waiting.pop_front();
    }
    waiting.push_back(failure);
  }

  /// Everything that waits, oldest first.
  pub(crate) fn take(&self) -> Vec<CommandFailure> {
    let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
    waiting.drain(..).collect()
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::ErrorCode;

  use super::*;

  #[test]
  fn a_server_refusal_keeps_its_error_code() {
    let failure = CommandFailure::from(ActionFailure::new(
      FailureKind::Server(ErrorCode::LegalHold),
      "the conversation is under legal hold",
    ));
    assert_eq!(failure.kind, CommandFailureKind::Refused);
    assert_eq!(failure.maybe_error_code.as_deref(), Some("legal_hold"));
  }

  #[test]
  fn refusals_of_a_deletion_carry_the_codes_the_apps_word() {
    for (code, wire) in [
      (ErrorCode::LastChannelAdmin, "last_channel_admin"),
      (ErrorCode::ImportInProgress, "import_in_progress"),
      (ErrorCode::SpaceHasHiddenChannels, "space_has_hidden_channels"),
    ] {
      let failure = CommandFailure::from(ActionFailure::new(FailureKind::Server(code), "refused"));
      assert_eq!(failure.maybe_error_code.as_deref(), Some(wire));
    }
  }

  #[test]
  fn an_unreachable_server_has_no_code() {
    for kind in [
      FailureKind::Network,
      FailureKind::CaptivePortal,
      FailureKind::UntrustedCertificate,
    ] {
      let failure = CommandFailure::from(ActionFailure::new(kind, "offline"));
      assert_eq!(failure.kind, CommandFailureKind::Unreachable);
      assert_eq!(failure.maybe_error_code, None);
    }
  }

  #[test]
  fn other_failures_have_no_code() {
    let failure = CommandFailure::from(ActionFailure::other("store closed"));
    assert_eq!(failure.kind, CommandFailureKind::Other);
    assert_eq!(failure.maybe_error_code, None);
    assert_eq!(failure.detail, "store closed");
  }

  #[test]
  fn an_unreachable_server_is_not_reported_as_a_failure() {
    let failures = CommandFailures::default();
    for kind in [
      FailureKind::Network,
      FailureKind::CaptivePortal,
      FailureKind::UntrustedCertificate,
    ] {
      assert!(!failures.report(ActionFailure::new(kind, "server answered 502: ")));
    }
    assert!(failures.take().is_empty());
    assert!(failures.report(ActionFailure::new(
      FailureKind::Server(ErrorCode::LegalHold),
      "refused"
    )));
    assert_eq!(failures.take().len(), 1);
  }

  #[test]
  fn waiting_failures_are_taken_once_in_order() {
    let failures = CommandFailures::default();
    failures.push(other("first"));
    failures.push(other("second"));
    let taken: Vec<_> = failures
      .take()
      .into_iter()
      .map(|failure| failure.detail)
      .collect();
    assert_eq!(taken, ["first", "second"]);
    assert!(failures.take().is_empty());
  }

  #[test]
  fn a_repeat_of_the_newest_failure_is_dropped() {
    let failures = CommandFailures::default();
    failures.push(other("same"));
    failures.push(other("same"));
    assert_eq!(failures.take().len(), 1);
  }

  #[test]
  fn the_oldest_failures_go_when_too_many_wait() {
    let failures = CommandFailures::default();
    for number in 0..MAX_WAITING + 3 {
      failures.push(other(&number.to_string()));
    }
    let taken = failures.take();
    assert_eq!(taken.len(), MAX_WAITING);
    assert_eq!(taken[0].detail, "3");
  }

  fn other(detail: &str) -> CommandFailure {
    CommandFailure::from(ActionFailure::other(detail))
  }
}
