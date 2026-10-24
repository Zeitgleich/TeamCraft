import Foundation

/// The servers the user signed in to, with one active entry, in the App Group defaults so the
/// notification extension opens the core for the same server (docs/operations.md "Server address in
/// the apps"). A list rather than one value, so several accounts later only add UI. Holds no
/// secrets: tokens are in the keychain, keyed by the entry's storage key.
///
/// Entries only ever come from a server the user (or the device's management) chose and confirmed;
/// the app never switches servers because of what a server says.
struct ServerStore {
  private static let accountsKey = "serverAccounts"
  private static let activeOriginKey = "activeServerOrigin"

  let defaults: UserDefaults

  init(defaults: UserDefaults = ServerStore.sharedDefaults()) {
    self.defaults = defaults
  }

  /// Every stored entry; an unreadable list counts as empty (the user enters the server again).
  var accounts: [ServerAccount] {
    guard let data = defaults.data(forKey: Self.accountsKey) else { return [] }
    return (try? JSONDecoder().decode([ServerAccount].self, from: data)) ?? []
  }

  /// The entry in use, as stored.
  var active: ServerAccount? {
    guard let origin = defaults.string(forKey: Self.activeOriginKey) else { return nil }
    return accounts.first { $0.origin == origin }
  }

  /// The entry in use, checked again by the core (`storedServer`); `nil` when it is missing pieces
  /// or was altered, and the app shows the server entry.
  var activeServer: StoredServer? {
    guard let active, let server = storedServer(profileJson: active.profileJson),
      server.address.origin == active.origin
    else { return nil }
    return server
  }

  /// Stores `server` (replacing an older entry of the same server) and makes it the active one.
  func activate(_ server: StoredServer) {
    activate(ServerAccount(server))
  }

  func activate(_ account: ServerAccount) {
    var all = accounts.filter { $0.origin != account.origin }
    all.append(account)
    write(all)
    defaults.set(account.origin, forKey: Self.activeOriginKey)
  }

  /// Forgets the active entry: the next start shows the server entry.
  func forgetActive() {
    guard let origin = defaults.string(forKey: Self.activeOriginKey) else { return }
    write(accounts.filter { $0.origin != origin })
    defaults.removeObject(forKey: Self.activeOriginKey)
  }

  /// The App Group defaults both processes read; the process's own without the entitlement.
  static func sharedDefaults(environment: AppEnvironment = .current) -> UserDefaults {
    UserDefaults(suiteName: environment.appGroupId) ?? .standard
  }

  private func write(_ all: [ServerAccount]) {
    // Encoding plain strings can't fail; on the impossible error the old list stays.
    guard let data = try? JSONEncoder().encode(all) else { return }
    defaults.set(data, forKey: Self.accountsKey)
  }
}

/// One server account: the server's origin, the name its local data is kept under, and the checked
/// sign-in profile (read back through `storedServer`).
struct ServerAccount: Codable, Equatable {
  let origin: String
  let storageKey: String
  let profileJson: String
}

extension ServerAccount {
  init(_ server: StoredServer) {
    self.init(
      origin: server.address.origin, storageKey: server.address.storageKey, profileJson: server.profileJson)
  }
}
