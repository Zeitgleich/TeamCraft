//! A user action that failed, as the core hands it to the UI (issue #322: no silent failure).
//!
//! The desktop shell and the browser core answer UI commands with a `String` error; the failure is
//! carried in it with its kind first (`"<kind>: <detail>"`), so the UI can word it without parsing
//! English text. Errors from older cores, or from paths that don't classify, read as
//! [`FailureKind::Other`].

use teamcraft_proto::ErrorCode;

/// Prefix of a failure that never reached the server.
const NETWORK_PREFIX: &str = "network";
/// Prefix of a call failure on this device ([`CallFailure`]): `call_<failure>`.
const CALL_PREFIX: &str = "call_";

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
  /// A call could not use this device's microphone, camera or screen, or the browser lacks what
  /// calls need.
  Call(CallFailure),
  /// Anything else (local store, encryption, a stopped core).
  Other,
}

/// Why a call action failed on this device, so the UI can say what to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallFailure {
  /// The user or a site setting blocked the microphone.
  MicrophoneDenied,
  MicrophoneMissing,
  /// Another app holds the microphone.
  MicrophoneInUse,
  CameraDenied,
  CameraMissing,
  CameraInUse,
  /// The screen could not be shared (not offered by the browser, or refused by a policy).
  ScreenUnavailable,
  /// The browser lacks what calls need (WebTransport, WebCodecs, AudioWorklet).
  Unsupported,
  /// The organization requires a background behind the camera image, which this device can't
  /// apply.
  BackgroundRequired,
}

impl CallFailure {
  const ALL: [Self; 9] = [
    Self::MicrophoneDenied,
    Self::MicrophoneMissing,
    Self::MicrophoneInUse,
    Self::CameraDenied,
    Self::CameraMissing,
    Self::CameraInUse,
    Self::ScreenUnavailable,
    Self::Unsupported,
    Self::BackgroundRequired,
  ];

  /// Stable name on the wire, after [`CALL_PREFIX`].
  pub fn as_str(self) -> &'static str {
    match self {
      Self::MicrophoneDenied => "microphone_denied",
      Self::MicrophoneMissing => "microphone_missing",
      Self::MicrophoneInUse => "microphone_in_use",
      Self::CameraDenied => "camera_denied",
      Self::CameraMissing => "camera_missing",
      Self::CameraInUse => "camera_in_use",
      Self::ScreenUnavailable => "screen_unavailable",
      Self::Unsupported => "unsupported",
      Self::BackgroundRequired => "background_required",
    }
  }

  fn from_wire(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|failure| failure.as_str() == name)
  }
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
    if let Some(failure) = prefix.strip_prefix(CALL_PREFIX).and_then(CallFailure::from_wire) {
      return Self::new(FailureKind::Call(failure), detail);
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
      FailureKind::Call(failure) => format!("{CALL_PREFIX}{}: {}", failure.as_str(), self.detail),
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
  fn call_failures_round_trip() {
    for failure in CallFailure::ALL {
      let failure = ActionFailure::new(FailureKind::Call(failure), "getUserMedia: Permission denied");
      assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
    }
    assert_eq!(
      ActionFailure::new(FailureKind::Call(CallFailure::CameraDenied), "x").to_wire(),
      "call_camera_denied: x"
    );
  }

  #[test]
  fn text_without_a_known_kind_is_other_and_kept_whole() {
    for text in [
      "this device is not set up yet",
      "server: server answered 400: invalid request: nope",
      "some_future_code: detail",
      "call_some_future_failure: detail",
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
