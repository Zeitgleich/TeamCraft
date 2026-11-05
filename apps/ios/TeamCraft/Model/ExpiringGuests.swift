import Foundation

/// A guest membership ending soon (`expiring_guests`), issue #212.
struct ExpiringGuest: Decodable, Identifiable, Equatable, Sendable {
  struct Person: Decodable, Equatable, Sendable {
    let id: String
    let displayName: String
  }

  static let dayMs: Int64 = 24 * 60 * 60 * 1000
  private static let extendDays: [Int64] = [30, 90]

  let spaceId: String
  let spaceName: String
  let user: Person
  let expiresAt: Int64
  let isOwn: Bool
  let canExtend: Bool
  let extendLimit: Int64?

  var id: String { spaceId + "/" + user.id }
  var expiresAtDate: Date { Date(timeIntervalSince1970: Double(expiresAt) / 1000) }

  /// What the user may offer: nothing without the right, nothing past their limit (the server's word).
  var extendOptions: [Int64] {
    guard canExtend else { return [] }
    return Self.extendDays.filter { days in
      extendLimit.map { expiresAt + days * Self.dayMs <= $0 } ?? true
    }
  }
}
