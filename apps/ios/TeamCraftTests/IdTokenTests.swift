import XCTest

@testable import TeamCraft

/// The session's ID token on its way to the core (docs/e2ee.md §3.1.3, issue #1288).
final class IdTokenTests: XCTestCase {
  private static let subject = "user-1"
  private static let clientId = "teamcraft"
  private static let issuer = "https://id.example.com/realms/team"

  private var suiteName = ""
  private var defaults: UserDefaults!
  private var items: MemorySecretItems!

  override func setUpWithError() throws {
    suiteName = "IdTokenTests-\(UUID().uuidString)"
    defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    items = MemorySecretItems()
    ServerStore(defaults: defaults).activate(
      ServerAccount(origin: "https://team.example.com", storageKey: "team.example.com", profileJson: "{}"))
  }

  override func tearDown() {
    defaults.removePersistentDomain(forName: suiteName)
  }

  // MARK: What is stored

  func testTheVaultKeepsTheIdTokenWithTheSession() throws {
    let vault = makeVault()
    let token = Self.jwt(iat: 1)
    try vault.save(session(idToken: token))
    XCTAssertEqual(vault.load()?.idToken, token)
    XCTAssertEqual(vault.currentIdToken(), token)
    vault.clear()
    XCTAssertNil(vault.currentIdToken())
  }

  func testASignInStoredByAnOlderVersionHasNoIdTokenUntilARefreshBringsOne() throws {
    var stored = try XCTUnwrap(
      JSONSerialization.jsonObject(with: JSONEncoder().encode(session(idToken: "dropped"))) as? [String: Any])
    stored["idToken"] = nil
    items.values["oidc-session.team.example.com"] = try JSONSerialization.data(withJSONObject: stored)
    let fresh = Self.jwt(iat: 2)
    let vault = makeVault(refresh: { $0.renewed(by: Self.response(idToken: fresh)) })
    XCTAssertEqual(vault.load()?.accessToken, "old-access")
    XCTAssertNil(vault.currentIdToken())
    XCTAssertEqual(vault.currentAccessToken(forceRefresh: true), "new-access")
    XCTAssertEqual(vault.currentIdToken(), fresh)
  }

  func testALocalAccountHasNoIdToken() throws {
    let vault = makeVault()
    try vault.saveLocal(LocalSession(accessToken: "local", expiresAt: Date().addingTimeInterval(600), userId: "7"))
    XCTAssertNil(vault.currentIdToken())
  }

  func testTheSessionNeverPrintsItsTokens() {
    let printed = session(idToken: "secret-id-token")
    XCTAssertFalse(String(describing: printed).contains("secret"))
    XCTAssertFalse(String(reflecting: printed).contains("secret"))
  }

  // MARK: What a refresh leaves

  func testARefreshReplacesTheIdToken() {
    let newer = Self.jwt(iat: 2)
    XCTAssertEqual(session(idToken: Self.jwt(iat: 1)).renewed(by: Self.response(idToken: newer)).idToken, newer)
  }

  func testARefreshWithoutAnIdTokenLeavesNone() {
    XCTAssertNil(session(idToken: Self.jwt(iat: 1)).renewed(by: Self.response(idToken: nil)).idToken)
  }

  func testARefreshTokenOfSomebodyElseIsNotKept() {
    let previous = session(idToken: Self.jwt(iat: 1))
    for foreign in [
      Self.jwt(iat: 2, subject: "user-2"), Self.jwt(iat: 2, audience: "other"),
      Self.jwt(iat: 2, issuer: "https://evil.example"), "not-a-jwt",
    ] {
      XCTAssertNil(previous.renewed(by: Self.response(idToken: foreign)).idToken)
    }
  }

  // MARK: Forced refresh

  func testAFreshTokenIsOnlyRefreshedWhenForced() throws {
    let refreshes = Counter()
    let vault = makeVault(refresh: {
      refreshes.increment()
      return $0.renewed(by: Self.response(idToken: Self.jwt(iat: 2)))
    })
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    XCTAssertEqual(vault.currentAccessToken(), "old-access")
    XCTAssertEqual(refreshes.value, 0)
    XCTAssertEqual(vault.currentAccessToken(forceRefresh: true), "new-access")
    XCTAssertEqual(refreshes.value, 1)
    XCTAssertEqual(vault.currentAccessToken(), "new-access")
    XCTAssertEqual(refreshes.value, 1)
  }

  func testAForcedRefreshThatFailsKeepsTheSessionAndItsGoodToken() throws {
    let token = Self.jwt(iat: 1)
    let vault = makeVault(refresh: { _ in throw OidcError.network("offline") })
    try vault.save(session(idToken: token))
    XCTAssertEqual(vault.currentAccessToken(forceRefresh: true), "old-access")
    XCTAssertEqual(vault.currentIdToken(), token)
    XCTAssertEqual(vault.load()?.refreshToken, "old-refresh")
  }

  func testARefusedRefreshTokenEndsTheSessionAlsoWhenForced() throws {
    let vault = makeVault(refresh: { _ in throw OidcError.sessionExpired })
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    XCTAssertNil(vault.currentAccessToken(forceRefresh: true))
    XCTAssertNil(vault.load())
  }

  func testARefreshThatTakesTooLongKeepsTheSession() throws {
    let vault = makeVault(refreshTimeout: 0.05) { session in
      try await Task.sleep(nanoseconds: 2_000_000_000)
      return session
    }
    try vault.save(session(idToken: Self.jwt(iat: 1), expiresIn: -60))
    XCTAssertNil(vault.currentAccessToken())
    XCTAssertEqual(vault.load()?.refreshToken, "old-refresh")
  }

  // MARK: The provider and the core

  func testAttachingTellsTheCoreTheIdToken() throws {
    let token = Self.jwt(iat: 1)
    let vault = makeVault()
    try vault.save(session(idToken: token))
    let core = FakeCore()
    VaultTokenProvider(vault: vault).attach(core)
    XCTAssertEqual(core.noted, [token])
  }

  func testOneRequestIsOneForcedRefreshAndTheNewIdTokenReachesTheCore() throws {
    let newer = Self.jwt(iat: 2)
    let refreshes = Counter()
    let vault = makeVault(refresh: {
      refreshes.increment()
      return $0.renewed(by: Self.response(idToken: newer))
    })
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    let core = FakeCore()
    let provider = VaultTokenProvider(vault: vault)
    provider.attach(core)
    core.isRequestPending = true
    XCTAssertEqual(token(provider.accessToken()), "new-access")
    XCTAssertEqual(token(provider.accessToken()), "new-access")
    XCTAssertEqual(refreshes.value, 1)
    XCTAssertEqual(core.noted.last, newer)
  }

  func testAFailedForcedRefreshIsNotRetriedForTheSameRequest() throws {
    let refreshes = Counter()
    let vault = makeVault(refresh: { _ in
      refreshes.increment()
      throw OidcError.network("offline")
    })
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    let core = FakeCore()
    let provider = VaultTokenProvider(vault: vault)
    provider.attach(core)
    core.isRequestPending = true
    XCTAssertEqual(token(provider.accessToken()), "old-access")
    XCTAssertEqual(token(provider.accessToken()), "old-access")
    XCTAssertEqual(refreshes.value, 1)
  }

  func testACoreThatIsGoneDoesNotFailTheTokenCallAndIsNotKeptAlive() throws {
    let vault = makeVault()
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    let provider = VaultTokenProvider(vault: vault)
    weak var weakCore: FakeCore?
    autoreleasepool {
      let core = FakeCore()
      weakCore = core
      provider.attach(core)
    }
    XCTAssertNil(weakCore)
    XCTAssertEqual(token(provider.accessToken()), "old-access")
  }

  func testAProviderWithoutACoreNeverForcesARefresh() throws {
    let refreshes = Counter()
    let vault = makeVault(refresh: {
      refreshes.increment()
      return $0
    })
    try vault.save(session(idToken: Self.jwt(iat: 1)))
    XCTAssertEqual(token(VaultTokenProvider(vault: vault).accessToken()), "old-access")
    XCTAssertEqual(refreshes.value, 0)
  }

  /// Issue #1671: a refresh that fails for now keeps the session, and the core hears it is not a
  /// sign-out; a refused refresh token ends the session, and the core hears that.
  func testAFailedRefreshIsNoSignOutButARefusedOneIs() throws {
    let offline = makeVault(refresh: { _ in throw OidcError.network("offline") })
    try offline.save(session(idToken: nil, expiresIn: -60))
    XCTAssertTrue(isUnavailable(VaultTokenProvider(vault: offline).accessToken()))

    let refused = makeVault(refresh: { _ in throw OidcError.sessionExpired })
    try refused.save(session(idToken: nil, expiresIn: -60))
    XCTAssertTrue(isSignedOut(VaultTokenProvider(vault: refused).accessToken()))
  }

  // MARK: Helpers

  private func token(_ answer: AccessToken) -> String? {
    if case .token(let value) = answer { return value }
    return nil
  }

  private func isUnavailable(_ answer: AccessToken) -> Bool {
    if case .unavailable = answer { return true }
    return false
  }

  private func isSignedOut(_ answer: AccessToken) -> Bool {
    if case .signedOut = answer { return true }
    return false
  }

  private func makeVault(
    refreshTimeout: TimeInterval = 5,
    refresh: @escaping @Sendable (OidcSession) async throws -> OidcSession = { $0 }
  ) -> SessionVault {
    SessionVault(
      keychain: items, servers: ServerStore(defaults: defaults), refreshLockURL: nil,
      refreshTimeout: refreshTimeout, refresh: refresh)
  }

  private func session(idToken: String?, expiresIn: TimeInterval = 600) -> OidcSession {
    OidcSession(
      accessToken: "old-access", refreshToken: "old-refresh", idToken: idToken,
      expiresAt: Date().addingTimeInterval(expiresIn), subject: Self.subject, displayName: nil,
      tokenEndpoint: URL(string: "https://id.example.com/token")!, clientId: Self.clientId, loginHint: nil)
  }

  private static func response(idToken: String?) -> TokenResponse {
    TokenResponse(accessToken: "new-access", refreshToken: nil, idToken: idToken, expiresIn: 300)
  }

  private static func jwt(
    iat: Int, subject: String = subject, audience: String = clientId, issuer: String = issuer
  ) -> String {
    let claims: [String: Any] = ["sub": subject, "aud": audience, "iss": issuer, "iat": iat]
    let payload = try! JSONSerialization.data(withJSONObject: claims)
      .base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
    return "e30.\(payload).sig"
  }
}

private final class MemorySecretItems: SecretItems, @unchecked Sendable {
  private let lock = NSLock()
  private var stored: [String: Data] = [:]

  var values: [String: Data] {
    get { lock.withLock { stored } }
    set { lock.withLock { stored = newValue } }
  }

  func read(_ account: String) throws -> Data? { values[account] }
  func write(_ data: Data, for account: String) throws { values[account] = data }
  func delete(_ account: String) throws { values[account] = nil }
}

private final class FakeCore: IdTokenReceiver {
  var isRequestPending = false
  var noted: [String?] = []

  func takeIdTokenRequest() -> Bool {
    defer { isRequestPending = false }
    return isRequestPending
  }

  func noteIdToken(idToken: String?) {
    noted.append(idToken)
  }
}

private final class Counter: @unchecked Sendable {
  private let lock = NSLock()
  private var count = 0

  var value: Int { lock.withLock { count } }
  func increment() { lock.withLock { count += 1 } }
}
