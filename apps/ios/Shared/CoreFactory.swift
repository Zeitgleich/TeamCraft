import Foundation

/// Hands the Rust core the access token of the keychain session (it asks on its engine thread).
final class VaultTokenProvider: TokenProvider, @unchecked Sendable {
  private let vault: SessionVault

  init(vault: SessionVault) {
    self.vault = vault
  }

  func accessToken() -> String? {
    vault.currentAccessToken()
  }
}

/// Forwards the core's change callbacks (called on the engine thread).
final class ClosureEventListener: EventListener, @unchecked Sendable {
  private let handler: () -> Void

  init(_ handler: @escaping () -> Void) {
    self.handler = handler
  }

  func onChange() {
    handler()
  }
}

enum CoreFactoryError: Error, Equatable {
  /// Not signed in (no server, no account, or no database key yet).
  case noAccount
}

/// Opens the Rust core for the signed-in account of the active server from the shared container
/// and keychain. The app and the notification extension both use it, so they open the same sealed
/// database.
struct CoreFactory {
  let environment: AppEnvironment
  let container: SharedContainer
  let keys: DatabaseKeyStore
  let vault: SessionVault
  let servers: ServerStore

  init(
    environment: AppEnvironment = .current,
    container: SharedContainer? = nil,
    keys: DatabaseKeyStore = DatabaseKeyStore(),
    vault: SessionVault = SessionVault(),
    servers: ServerStore = ServerStore()
  ) {
    self.environment = environment
    self.container = container ?? SharedContainer(environment: environment)
    self.keys = keys
    self.vault = vault
    self.servers = servers
  }

  /// Opens the core for `account` (the OIDC subject) on the active server. The app creates the
  /// database key on first sign-in (`createKey`); the extension never does.
  func open(account: String, createKey: Bool, listener: EventListener) throws -> TeamCraft {
    guard let server = servers.activeServer else { throw CoreFactoryError.noAccount }
    let keyName = Self.databaseKeyName(storageKey: server.address.storageKey, account: account)
    let key: Data
    if createKey {
      key = try keys.key(for: keyName)
    } else if let existing = try keys.existingKey(for: keyName) {
      key = existing
    } else {
      throw CoreFactoryError.noAccount
    }
    // Created with its protection class before the core writes into it.
    _ = container.downloadsDirectory
    // The first connection goes through the system proxy too, also in the notification extension,
    // which has no network monitor (issue #157).
    SystemProxy.report()
    let config = AppConfig(
      serverUrl: server.address.origin,
      dataDir: container.dataDirectory.path,
      account: account,
      databaseKey: key,
      pushGatewayUrl: environment.pushGatewayURL?.absoluteString)
    return try TeamCraft(config: config, tokens: VaultTokenProvider(vault: vault), listener: listener)
  }

  /// Deletes the database key of `account` on the active server (sign-out).
  func deleteDatabaseKey(account: String) {
    guard let active = servers.active else { return }
    try? keys.delete(for: Self.databaseKeyName(storageKey: active.storageKey, account: account))
  }

  /// One key per server and account, so the same subject on two servers never shares a key.
  /// Storage keys never contain `|`, so the name splits back unambiguously.
  static func databaseKeyName(storageKey: String, account: String) -> String {
    "\(storageKey)|\(account)"
  }
}

/// What the app acts on when a core call fails. The generated error enum's case spelling depends
/// on the UniFFI version, so this reads the description (`Network(message: ...)`) instead.
enum CoreFailure: Equatable {
  case network, signedOut, notEnrolled, wrongPassphrase, storage, recoveryPending
  /// A move of the call into a meeting room didn't check out (docs/meetings.md): stay in the call.
  case callMoveRefused
  /// Device linking: the typed code is not the one on the new device (try again), or the last try failed.
  case linkCodeRejected, linkAttemptEnded
  /// Device linking: not a link code; a code for the other side (issue #998); a code that ran out.
  case linkCodeInvalid, linkCodeForNewDevice, linkCodeForSignedInDevice, linkCodeExpired
  case other

  init(_ error: Error) {
    let name = String(describing: error).lowercased()
    if name.hasPrefix("network") {
      self = .network
    } else if name.hasPrefix("signedout") {
      self = .signedOut
    } else if name.hasPrefix("notenrolled") {
      self = .notEnrolled
    } else if name.hasPrefix("wrongpassphrase") {
      self = .wrongPassphrase
    } else if name.hasPrefix("storage") {
      self = .storage
    } else if name.hasPrefix("recoverypending") {
      self = .recoveryPending
    } else if name.hasPrefix("callmoverefused") {
      self = .callMoveRefused
    } else if name.hasPrefix("linkcoderejected") {
      self = .linkCodeRejected
    } else if name.hasPrefix("linkattemptended") {
      self = .linkAttemptEnded
    } else if name.hasPrefix("linkcodeinvalid") {
      self = .linkCodeInvalid
    } else if name.hasPrefix("linkcodefornewdevice") {
      self = .linkCodeForNewDevice
    } else if name.hasPrefix("linkcodeforsignedindevice") {
      self = .linkCodeForSignedInDevice
    } else if name.hasPrefix("linkcodeexpired") {
      self = .linkCodeExpired
    } else {
      self = .other
    }
  }
}
