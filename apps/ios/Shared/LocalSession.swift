import Foundation

/// The session of a local account (e-mail and password, `POST /api/v1/auth/local/login`): the
/// server's bearer token for the API and the gateway until it expires. There is no refresh; an
/// expired session is signed out. Kept in the shared keychain next to the OIDC session, so the
/// notification extension can fetch a message with it while the app is closed.
struct LocalSession: Codable, Equatable {
  /// How far past `expiresAt` the token is still handed out, for a local clock that runs ahead. The
  /// server's clock decides in the end: an expired token gets 401 and the app signs out.
  static let clockSkewLeeway: TimeInterval = 300

  var accessToken: String
  /// When the server ends the session (server time).
  var expiresAt: Date
  /// The account key for local data, as the OIDC subject is for SSO accounts.
  var userId: String

  /// Whether the token is worth sending: not yet expired by the local clock, give or take `leeway`.
  func isUsable(now: Date = Date(), leeway: TimeInterval = clockSkewLeeway) -> Bool {
    expiresAt.timeIntervalSince(now) > -leeway
  }
}

// security.md: never print the token, also not by accident through `String(describing:)`.
extension LocalSession: CustomStringConvertible, CustomDebugStringConvertible {
  var description: String { "LocalSession([redacted])" }
  var debugDescription: String { description }
}
