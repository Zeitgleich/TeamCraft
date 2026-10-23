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
  /// Not signed in (no account, or no database key yet).
  case noAccount
}

/// Opens the Rust core for the signed-in account from the shared container and keychain. The app
/// and the notification extension both use it, so they open the same sealed database.
struct CoreFactory {
  let environment: AppEnvironment
  let container: SharedContainer
  let keys: DatabaseKeyStore
  let vault: SessionVault

  init(
    environment: AppEnvironment = .current,
    container: SharedContainer? = nil,
    keys: DatabaseKeyStore = DatabaseKeyStore(),
    vault: SessionVault = SessionVault()
  ) {
    self.environment = environment
    self.container = container ?? SharedContainer(environment: environment)
    self.keys = keys
    self.vault = vault
  }

  /// The app creates the database key on first sign-in (`createKey`); the extension never does.
  func open(account: String, createKey: Bool, listener: EventListener) throws -> TeamCraft {
    let key: Data
    if createKey {
      key = try keys.key(for: account)
    } else if let existing = try keys.existingKey(for: account) {
      key = existing
    } else {
      throw CoreFactoryError.noAccount
    }
    // Created with its protection class before the core writes into it.
    _ = container.downloadsDirectory
    let config = AppConfig(
      serverUrl: environment.serverURL.absoluteString,
      dataDir: container.dataDirectory.path,
      account: account,
      databaseKey: key,
      pushGatewayUrl: environment.pushGatewayURL?.absoluteString)
    return try TeamCraft(config: config, tokens: VaultTokenProvider(vault: vault), listener: listener)
  }
}

/// What the app acts on when a core call fails. The generated error enum's case spelling depends
/// on the UniFFI version, so this reads the description (`Network(message: ...)`) instead.
enum CoreFailure: Equatable {
  case network, signedOut, notEnrolled, wrongPassphrase, storage, recoveryPending, other

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
    } else {
      self = .other
    }
  }
}
