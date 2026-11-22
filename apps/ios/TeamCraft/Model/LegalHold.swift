import Foundation

/// The legal hold part of the core's chat snapshot (issue #211, docs/data-export.md §4.2): the holds
/// of the workspace policy this device verified. Absent before one was verified.
struct LegalHoldSnapshot: Decodable, Sendable {
  struct Security: Decodable, Sendable {
    let legalHolds: [LegalHold]?
  }

  let security: Security?

  /// The hold on `conversationId`, if the verified policy holds it.
  func hold(on conversationId: String) -> LegalHold? {
    security?.legalHolds?.first { $0.conversationId == conversationId }
  }
}

/// One held conversation: its members read the case reference in a banner.
struct LegalHold: Decodable, Equatable, Sendable {
  let conversationId: String
  /// The organization's case or matter reference; no names, no reasons.
  let reference: String
  /// When an owner placed it (epoch milliseconds, the owner's clock): shown, never decisive.
  var placedAt: Int64?
}

extension AppModel {
  /// The legal hold on a conversation under the verified workspace policy, or nil.
  func legalHold(on conversationId: String) async -> LegalHold? {
    guard let json = try? await core().snapshot() else { return nil }
    return Wire.decode(LegalHoldSnapshot.self, from: json)?.hold(on: conversationId)
  }
}
