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
/// Prefixes of a failure where something on the network answered instead of the server.
const CAPTIVE_PORTAL_PREFIX: &str = "captive_portal";
const UNTRUSTED_CERTIFICATE_PREFIX: &str = "untrusted_certificate";
/// Prefix of a system notification that could not be shown on this device: `notification_<failure>`.
const NOTIFICATION_PREFIX: &str = "notification_";
/// Prefix of a device link that failed ([`LinkFailure`]): `link_<failure>`.
const LINK_PREFIX: &str = "link_";

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
  /// A captive portal answered instead of the server: sign in to the network first.
  CaptivePortal,
  /// The connection was refused over the certificate: usually a proxy that inspects encrypted
  /// traffic with a CA this device doesn't trust.
  UntrustedCertificate,
  /// A system notification could not be shown on this device ([`NotificationFailure`]).
  Notification(NotificationFailure),
  /// Linking a device failed ([`LinkFailure`], issue #645).
  Link(LinkFailure),
  /// Anything else (local store, encryption, a stopped core).
  Other,
}

/// Why a system notification could not be shown, so the UI can say what to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationFailure {
  /// The browser or system doesn't allow notifications for this site or app.
  NotAllowed,
  /// This browser has no notifications at all.
  Unsupported,
  /// The browser or system refused to show it.
  Refused,
}

/// Why linking a device failed, so the UI can say what to do next (issue #645).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFailure {
  /// The scanned or typed text is no device link code.
  CodeInvalid,
  /// The other device speaks another linking version: one of the apps needs an update.
  OtherVersion,
  /// The new device no longer waits for this code: show a new one and scan again.
  PeerGone,
  /// The other device stopped answering.
  TimedOut,
  /// Nobody took the code up in time: show a new one.
  CodeExpired,
  /// A device of the account denied the request.
  Denied,
  /// The keys went out but the new device did not confirm them, or could not open them.
  NotConfirmed,
  /// The request was taken up by another device, withdrawn or expired.
  RequestGone,
  /// A signed-in device read a code that the new device has to scan (issue #998).
  ForNewDevice,
  /// The new device read a code that a signed-in device has to scan.
  ForSignedInDevice,
}

impl LinkFailure {
  const ALL: [Self; 10] = [
    Self::CodeInvalid,
    Self::OtherVersion,
    Self::PeerGone,
    Self::TimedOut,
    Self::CodeExpired,
    Self::Denied,
    Self::NotConfirmed,
    Self::RequestGone,
    Self::ForNewDevice,
    Self::ForSignedInDevice,
  ];

  /// Stable name on the wire, after [`LINK_PREFIX`].
  pub fn as_str(self) -> &'static str {
    match self {
      Self::CodeInvalid => "code_invalid",
      Self::OtherVersion => "other_version",
      Self::PeerGone => "peer_gone",
      Self::TimedOut => "timed_out",
      Self::CodeExpired => "code_expired",
      Self::Denied => "denied",
      Self::NotConfirmed => "not_confirmed",
      Self::RequestGone => "request_gone",
      Self::ForNewDevice => "for_new_device",
      Self::ForSignedInDevice => "for_signed_in_device",
    }
  }

  fn from_wire(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|failure| failure.as_str() == name)
  }
}

impl NotificationFailure {
  const ALL: [Self; 3] = [Self::NotAllowed, Self::Unsupported, Self::Refused];

  /// Stable name on the wire, after [`NOTIFICATION_PREFIX`].
  pub fn as_str(self) -> &'static str {
    match self {
      Self::NotAllowed => "not_allowed",
      Self::Unsupported => "unsupported",
      Self::Refused => "refused",
    }
  }

  fn from_wire(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|failure| failure.as_str() == name)
  }
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
  /// Another app holds the camera.
  CameraInUse,
  /// The camera offers no picture format this app can use.
  CameraUnsupported,
  /// An iPhone or iPad camera (Continuity Camera) didn't come up: declined on the phone, the
  /// phone locked or out of reach (#819).
  CameraNotConnected,
  /// The camera couldn't be opened for a reason the system doesn't tell.
  CameraUnavailable,
  /// The screen could not be shared (not offered by the browser, or refused by a policy).
  ScreenUnavailable,
  /// macOS refused the screen capture to the desktop app (Screen & System Audio Recording), also
  /// while its switch shows on: a grant from an earlier build no longer matches (#940).
  ScreenDenied,
  /// The browser opens the screen picker only straight from a click, and the click no longer
  /// counted (Safari, issue #930). Choosing to share again usually works.
  ScreenNeedsClick,
  /// Linux on X11: screens are shared only on Wayland (through the desktop portal).
  ScreenNeedsWayland,
  /// The camera started, but no picture of it could be sent (it delivered nothing, or nothing
  /// could be encoded).
  CameraNoPicture,
  /// The camera stopped by itself mid-call (unplugged, taken by the system or another app, or its
  /// pictures could no longer be encoded); the video is off (#920).
  CameraStopped,
  /// The screen share started, but no picture of it could be sent.
  ScreenNoPicture,
  /// The camera sends, but only black (covered, a privacy shutter, a camera that doesn't wake
  /// up). The video stays on.
  CameraBlack,
  /// The screen share sends, but only black: on macOS before the Screen Recording permission
  /// applies, which needs an app restart (issue #683), or protected content. The share stays on.
  ScreenBlack,
  /// The browser lacks what calls need (WebTransport, WebCodecs, AudioWorklet).
  Unsupported,
  /// The call server (SFU) can't be reached, while the chat server can: often a network that
  /// blocks UDP (QUIC).
  ServerUnreachable,
  /// The organization requires a background behind the camera image, which this device can't
  /// apply.
  BackgroundRequired,
  /// The background effect couldn't start on this device (no GPU for it, or its model didn't
  /// load); the camera sends without it.
  EffectsUnavailable,
  /// This device isn't in the conversation's encryption group yet (the welcome hasn't come, or
  /// another device has to let it in), so it can't check who is in the call.
  EncryptionPending,
  /// A move of the call into a meeting room didn't check out: nobody in the conversation's group
  /// announced it, or the room holds people the move didn't name (docs/meetings.md).
  MoveRefused,
  /// The chosen speakers can't be used; the sound played on the default output instead.
  SpeakersUnavailable,
  /// No speakers or other audio output to play on.
  SpeakersMissing,
  /// The browser didn't let the page play sound (no interaction with the page yet, or a site
  /// setting).
  SoundBlocked,
  /// The call broke off and couldn't be resumed within the reconnect grace period while the chat
  /// server stayed reachable: the call server went away (restarted, crashed) and didn't come back
  /// in time (issue #427).
  ServerLost,
  /// The call broke off and couldn't be resumed within the reconnect grace period, along with the
  /// connection to the chat server: this device's own network.
  ConnectionLost,
}

impl CallFailure {
  const ALL: [Self; 29] = [
    Self::MicrophoneDenied,
    Self::MicrophoneMissing,
    Self::MicrophoneInUse,
    Self::CameraDenied,
    Self::CameraMissing,
    Self::CameraInUse,
    Self::CameraUnsupported,
    Self::CameraNotConnected,
    Self::CameraUnavailable,
    Self::ScreenUnavailable,
    Self::ScreenDenied,
    Self::ScreenNeedsClick,
    Self::ScreenNeedsWayland,
    Self::CameraNoPicture,
    Self::CameraStopped,
    Self::ScreenNoPicture,
    Self::CameraBlack,
    Self::ScreenBlack,
    Self::Unsupported,
    Self::ServerUnreachable,
    Self::BackgroundRequired,
    Self::EffectsUnavailable,
    Self::EncryptionPending,
    Self::MoveRefused,
    Self::SpeakersUnavailable,
    Self::SpeakersMissing,
    Self::SoundBlocked,
    Self::ServerLost,
    Self::ConnectionLost,
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
      Self::CameraUnsupported => "camera_unsupported",
      Self::CameraNotConnected => "camera_not_connected",
      Self::CameraUnavailable => "camera_unavailable",
      Self::ScreenUnavailable => "screen_unavailable",
      Self::ScreenDenied => "screen_denied",
      Self::ScreenNeedsClick => "screen_needs_click",
      Self::ScreenNeedsWayland => "screen_needs_wayland",
      Self::CameraNoPicture => "camera_no_picture",
      Self::CameraStopped => "camera_stopped",
      Self::ScreenNoPicture => "screen_no_picture",
      Self::CameraBlack => "camera_black",
      Self::ScreenBlack => "screen_black",
      Self::Unsupported => "unsupported",
      Self::ServerUnreachable => "server_unreachable",
      Self::BackgroundRequired => "background_required",
      Self::EffectsUnavailable => "effects_unavailable",
      Self::EncryptionPending => "encryption_pending",
      Self::MoveRefused => "move_refused",
      Self::SpeakersUnavailable => "speakers_unavailable",
      Self::SpeakersMissing => "speakers_missing",
      Self::SoundBlocked => "sound_blocked",
      Self::ServerLost => "server_lost",
      Self::ConnectionLost => "connection_lost",
    }
  }

  fn from_wire(name: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|failure| failure.as_str() == name)
  }

  /// A camera that didn't open or deliver: choosing another camera may help (#819).
  pub fn is_camera_failure(self) -> bool {
    matches!(
      self,
      Self::CameraInUse
        | Self::CameraUnsupported
        | Self::CameraNotConnected
        | Self::CameraUnavailable
        | Self::CameraNoPicture
        | Self::CameraStopped
        | Self::CameraBlack
        | Self::CameraMissing
    )
  }
}

impl ActionFailure {
  pub fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
    Self {
      kind,
      detail: detail.into(),
    }
  }

  /// Never reached the server: offline, or intercepted on the way. The connection banner already
  /// says so, so background work stays quiet about it.
  pub fn is_unreachable(&self) -> bool {
    matches!(
      self.kind,
      FailureKind::Network | FailureKind::CaptivePortal | FailureKind::UntrustedCertificate
    )
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
    let maybe_kind = match prefix {
      NETWORK_PREFIX => Some(FailureKind::Network),
      CAPTIVE_PORTAL_PREFIX => Some(FailureKind::CaptivePortal),
      UNTRUSTED_CERTIFICATE_PREFIX => Some(FailureKind::UntrustedCertificate),
      _ => None,
    };
    if let Some(kind) = maybe_kind {
      return Self::new(kind, detail);
    }
    if let Some(failure) = prefix.strip_prefix(CALL_PREFIX).and_then(CallFailure::from_wire) {
      return Self::new(FailureKind::Call(failure), detail);
    }
    if let Some(failure) = prefix
      .strip_prefix(NOTIFICATION_PREFIX)
      .and_then(NotificationFailure::from_wire)
    {
      return Self::new(FailureKind::Notification(failure), detail);
    }
    if let Some(failure) = prefix.strip_prefix(LINK_PREFIX).and_then(LinkFailure::from_wire) {
      return Self::new(FailureKind::Link(failure), detail);
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
      FailureKind::Notification(failure) => {
        format!("{NOTIFICATION_PREFIX}{}: {}", failure.as_str(), self.detail)
      },
      FailureKind::Link(failure) => format!("{LINK_PREFIX}{}: {}", failure.as_str(), self.detail),
      FailureKind::CaptivePortal => format!("{CAPTIVE_PORTAL_PREFIX}: {}", self.detail),
      FailureKind::UntrustedCertificate => format!("{UNTRUSTED_CERTIFICATE_PREFIX}: {}", self.detail),
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
  fn link_failures_round_trip() {
    for failure in LinkFailure::ALL {
      let failure = ActionFailure::new(FailureKind::Link(failure), "device link: relay closed");
      assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
    }
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
  fn camera_failures_offer_another_camera_but_a_refusal_does_not() {
    assert!(CallFailure::CameraNotConnected.is_camera_failure());
    assert!(CallFailure::CameraInUse.is_camera_failure());
    assert!(CallFailure::CameraUnavailable.is_camera_failure());
    // Another camera won't help against a refused permission or a missing microphone.
    assert!(!CallFailure::CameraDenied.is_camera_failure());
    assert!(!CallFailure::MicrophoneInUse.is_camera_failure());
  }

  #[test]
  fn notification_failures_round_trip() {
    for failure in NotificationFailure::ALL {
      let failure = ActionFailure::new(FailureKind::Notification(failure), "browser: denied");
      assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
    }
  }

  #[test]
  fn interceptions_round_trip_and_count_as_unreachable() {
    for kind in [FailureKind::CaptivePortal, FailureKind::UntrustedCertificate] {
      let failure = ActionFailure::new(kind.clone(), "server: the network intercepted the connection");
      assert_eq!(ActionFailure::from_wire(&failure.to_wire()), failure);
      assert!(failure.is_unreachable());
    }
    assert!(!ActionFailure::other("x").is_unreachable());
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
