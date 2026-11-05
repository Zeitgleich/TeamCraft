import Foundation

/// A guest membership ending soon (`expiring_guests`), issue #212.
struct ExpiringGuest: Decodable, Identifiable, Equatable, Sendable {
  struct Person: Decodable, Equatable, Sendable {
    let id: String
    let displayName: String
  }

  /// What the user may extend to, as the server decided it; without `days`, as far as their limit
  /// goes.
  struct Offer: Decodable, Equatable, Hashable, Sendable {
    let until: Int64
    let days: Int64?

    var untilDate: Date { Date(timeIntervalSince1970: Double(until) / 1000) }
  }

  let spaceId: String
  let spaceName: String
  let user: Person
  let expiresAt: Int64
  let isOwn: Bool
  let extendOffers: [Offer]?

  var id: String { spaceId + "/" + user.id }
  var expiresAtDate: Date { Date(timeIntervalSince1970: Double(expiresAt) / 1000) }
}
