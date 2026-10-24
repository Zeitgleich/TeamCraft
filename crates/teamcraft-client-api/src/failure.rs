//! A user action that failed, as the core hands it to the UI (issue #322: no silent failure).
//!
//! The desktop shell and the browser core answer UI commands with a `String` error; the failure is
//! carried in it with its kind first (`"<kind>: <detail>"`), so the UI can word it without parsing
//! English text. Errors from older cores, or from paths that don't classify, read as
//! [`FailureKind::Other`].

use teamcraft_proto::ErrorCode;

/// Prefix of a failure that never reached the server.
const NETWORK_PREFIX: &str = "network";

/// What went wrong and the technical detail behind it, for the "Details" of an error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionFailure {
  pub kind: FailureKind,
  /// English text for developers and support. Never contains message content or secrets.
  pub detail: String,
}

/// The cause of a failure, as far as the UI words it differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailureKind {
  /// The server refused with this stable error code.
  Server(ErrorCode),
  /// The server could not be reached.
  Network,
  /// Anything else (local store, encryption, a stopped core).
  Other,
}

impl ActionFailure {
  pub fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
    Self {
      kind,
      detail: detail.into(),
    }
  }

  /// A failure whose cause is not known; worded as a generic error with the detail.
  pub fn other(detail: impl Into<String>) -> Self {
    Self::new(FailureKind::Other, detail)
  }

  /// Reads a failure from a core's error text; text without a known kind is [`FailureKind::Other`].
  pub fn from_wire(text: &str) -> Self {
    let Some((prefix, detail)) = text.split_once(": ") else {
      return Self::other(text);
    };
    if prefix == NETWORK_PREFIX {
      return Self::new(FailureKind::Network, detail);
    }
    match ErrorCode::from(prefix.to_owned()) {
      ErrorCode::Unknown(_) => Self::other(text),
      code => Self::new(FailureKind::Server(code), detail),
    }
  }

  /// The error text a core answers with; [`Self::from_wire`] reads it back.
  pub fn to_wire(&self) -> String {
    match &self.kind {
      FailureKind::Server(code) => format!("{}: {}", code.as_str(), self.detail),
      FailureKind::Network => format!("{NETWORK_PREFIX}: {}", self.detail),
      FailureKind::Other => self.detail.clone(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_failures_round_trip() {
    let failure = ActionFailure::new(
      FailureKind::Server(ErrorCode::NotSpaceMember),
      "server: server answered 400: the user is not a member of the space",
    );
    assert_eq!(
      failure.to_wire(),
      "not_space_member: server: server answered 400: the user is not a member of the space"
    );
    assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
  }

  #[test]
  fn network_failures_round_trip() {
    let failure = ActionFailure::new(FailureKind::Network, "server: network error: offline");
    assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
  }

  #[test]
  fn text_without_a_known_kind_is_other_and_kept_whole() {
    for text in [
      "this device is not set up yet",
      "server: server answered 400: invalid request: nope",
      "some_future_code: detail",
      "",
    ] {
      assert_eq!(
        ActionFailure::from_wire(text),
        ActionFailure::other(text),
        "{text}"
      );
    }
  }

  #[test]
  fn other_failures_are_sent_as_their_detail() {
    assert_eq!(
      ActionFailure::other("the core stopped").to_wire(),
      "the core stopped"
    );
  }
}
