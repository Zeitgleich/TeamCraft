import Foundation

/// Signs out the session of builds from before server entry (#224) before deleting it: once the
/// user entered the server it belonged to, its device's push registration is removed with the old
/// session and its refresh token revoked; after the core's wait bound it is deleted without that.
/// Which identity provider may get the token is decided in the core (`legacy_session`): the old
/// session kept no issuer, so only the entered server's provider with the very token endpoint the
/// token came from qualifies. Tokens are never logged.
actor LegacySignOut {
  private static let movedAtKey = "legacySessionSince"

  private let vault: SessionVault
  private let container: SharedContainer
  private let keys: DatabaseKeyStore
  private let environment: AppEnvironment
  private let urlSession: URLSession

  init(
    vault: SessionVault, container: SharedContainer, keys: DatabaseKeyStore = DatabaseKeyStore(),
    environment: AppEnvironment = .current, urlSession: URLSession = .shared
  ) {
    self.vault = vault
    self.container = container
    self.keys = keys
    self.environment = environment
    self.urlSession = urlSession
  }

  /// Notes when the old session was first seen (the wait bound counts from there); `true` the
  /// first time.
  @discardableResult
  nonisolated static func noteLegacySession(defaults: UserDefaults = .standard) -> Bool {
    guard defaults.object(forKey: movedAtKey) == nil else { return false }
    defaults.set(Date().timeIntervalSince1970 * 1000, forKey: movedAtKey)
    return true
  }

  /// Runs at start and after the user confirmed a server; cheap when nothing is left.
  func run(activeServer: StoredServer?) async {
    guard vault.hasLegacySession else { return }
    guard let session = vault.legacySession(), let refreshToken = session.refreshToken else {
      remove(maybeSession: vault.legacySession())
      return
    }
    let provider = LegacyProviderInfo(
      maybeIssuer: nil, maybeTokenEndpoint: session.tokenEndpoint.absoluteString, clientId: session.clientId)
    var maybeDiscovery: Data?
    if let server = activeServer, let url = URL(string: legacyDiscoveryUrl(issuer: server.issuer)) {
      maybeDiscovery = await fetch(url)
    }
    var isMatching = false
    if let server = activeServer, let discovery = maybeDiscovery {
      isMatching = isLegacyProvider(
        provider: provider, issuer: server.issuer, clientId: server.clientId, discoveryJson: discovery,
        allowInsecureHttp: Self.allowsInsecureHttp)
    }
    let movedAt = Int64(UserDefaults.standard.double(forKey: Self.movedAtKey))
    let now = Int64(Date().timeIntervalSince1970 * 1000)
    switch legacyCleanupAction(nowMs: now, movedAtMs: movedAt, isServerMatching: isMatching) {
    case .waitForServer:
      return
    case .unregisterPushThenRevoke:
      // Invariant: matching means a server and its discovery document are at hand.
      guard let server = activeServer, let discovery = maybeDiscovery else { return }
      let latest = await unregisterPush(session, server: server) ?? refreshToken
      await revoke(provider, issuer: server.issuer, discovery: discovery, refreshToken: latest)
    case .revokeAndDelete:
      // Without an issuer the provider can't be looked up: the token runs out on its own.
      break
    }
    remove(maybeSession: session)
  }

  /// Opens the old store with the old key and unregisters its device's push at `server`. Returns
  /// the refresh token the provider rotated to.
  private func unregisterPush(_ session: OidcSession, server: StoredServer) async -> String? {
    guard let renewed = try? await OidcClient.refresh(session, urlSession: urlSession),
      let key = try? keys.existingKey(for: session.subject)
    else { return nil }
    let config = AppConfig(
      serverUrl: server.address.origin,
      dataDir: container.legacyDataDirectory.path,
      account: session.subject,
      databaseKey: key,
      pushGatewayUrl: environment.pushGatewayURL?.absoluteString)
    do {
      let core = try TeamCraft(
        config: config, tokens: StaticTokenProvider(token: renewed.accessToken),
        listener: ClosureEventListener {})
      try await core.unregisterPush()
    } catch {
      // Best effort: the old device then keeps its registration until an admin removes it.
    }
    return renewed.refreshToken
  }

  private func revoke(_ provider: LegacyProviderInfo, issuer: String, discovery: Data, refreshToken: String) async {
    guard
      let revocation = try? legacyTokenRevocation(
        provider: provider, issuer: issuer, discoveryJson: discovery, refreshToken: refreshToken,
        allowInsecureHttp: Self.allowsInsecureHttp),
      let url = URL(string: revocation.url)
    else {
      // Not the old session's provider (or none to revoke at): the token runs out on its own.
      return
    }
    var fields: [String: String] = [:]
    for field in revocation.form { fields[field.name] = field.value }
    var request = URLRequest(url: url)
    request.httpMethod = "POST"
    request.timeoutInterval = 15
    request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
    request.httpBody = OidcClient.formEncoded(fields).data(using: .utf8)
    // Best effort: the session is deleted on the device either way.
    _ = try? await urlSession.data(for: request)
  }

  private func fetch(_ url: URL) async -> Data? {
    var request = URLRequest(url: url)
    request.timeoutInterval = 15
    guard let (data, response) = try? await urlSession.data(for: request),
      (response as? HTTPURLResponse)?.statusCode == 200
    else { return nil }
    return data
  }

  private func remove(maybeSession: OidcSession?) {
    if let session = maybeSession {
      try? keys.delete(for: session.subject)
    }
    container.removeLegacyData()
    vault.removeLegacySession()
    UserDefaults.standard.removeObject(forKey: Self.movedAtKey)
  }

  private static var allowsInsecureHttp: Bool {
    #if DEBUG
      return true
    #else
      return false
    #endif
  }
}

/// Hands the core one fixed access token (the old session's, for the push unregistration).
private final class StaticTokenProvider: TokenProvider, @unchecked Sendable {
  private let token: String

  init(token: String) {
    self.token = token
  }

  func accessToken() -> String? {
    token
  }
}
