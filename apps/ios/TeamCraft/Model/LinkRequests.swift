import Foundation
import SwiftUI

/// A new device of the user asking to be linked, as the prompt shows it (docs/e2ee.md §5, "Prompt on
/// every device"). The core reports the seconds left when it was read; the prompt counts down from
/// that moment on, so a wrong local clock changes nothing the server enforces.
struct LinkPrompt: Identifiable, Equatable {
  let id: String
  let deviceName: String
  let platform: LinkPlatform
  let expiresAt: Date

  init(_ request: LinkRequestItem, readAt: Date) {
    id = request.id
    deviceName = request.deviceName
    platform = request.platform
    expiresAt = readAt.addingTimeInterval(TimeInterval(request.secondsLeft))
  }

  func isExpired(at now: Date) -> Bool { expiresAt <= now }
}

/// The step the user is in after taking a request up with Review (or after scanning a code).
struct LinkReview: Equatable {
  /// Name of the new device for "Type the code that <name> shows"; `nil` after a scanned code
  /// (the code carries no name).
  let maybeDeviceName: String?
  /// The request being reviewed; `nil` when the code was scanned or typed instead.
  let maybeRequestId: String?
}

/// How a typed code ended.
enum LinkConfirmResult: Equatable {
  case approved
  /// Not the code on the new device; the user may try again.
  case wrongCode
  /// The last try failed; the new device has to start again.
  case attemptEnded
  case failed(AppFailure)
}

enum LinkPrompts {
  /// The prompts for the current `requests`: keeps the deadline of one already shown (its countdown
  /// started when it first arrived), drops the ones no longer listed and the expired, and hides the
  /// request being reviewed here.
  static func merge(
    current: [LinkPrompt], requests: [LinkRequestItem], readAt now: Date, hiding maybeReviewedId: String?
  ) -> [LinkPrompt] {
    requests
      .filter { $0.id != maybeReviewedId }
      .map { request in
        current.first { $0.id == request.id } ?? LinkPrompt(request, readAt: now)
      }
      .filter { !$0.isExpired(at: now) }
  }

  /// When the first of `prompts` runs out; `nil` without prompts.
  static func nextExpiry(of prompts: [LinkPrompt]) -> Date? {
    prompts.map(\.expiresAt).min()
  }
}

enum LinkCode {
  /// The digits of what the user typed: the code on the new device is numeric, a space or dash
  /// between groups is ignored.
  static func digits(of text: String) -> String {
    text.filter { $0.isASCII && $0.isNumber }
  }
}

extension LinkPlatform {
  /// The name of the platform in the prompt.
  var label: LocalizedStringKey {
    switch self {
    case .android: return "link_platform_android"
    case .ios: return "link_platform_ios"
    case .macos: return "link_platform_macos"
    case .windows: return "link_platform_windows"
    case .linux: return "link_platform_linux"
    case .web: return "link_platform_web"
    case .other: return "link_platform_other"
    }
  }
}
