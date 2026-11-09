import CryptoKit
import XCTest

@testable import TeamCraft

/// The device key and DPoP proofs in the app's own token requests (docs/e2ee.md §3.1.2, issue #120),
/// with a software stand-in key: the Simulator has no Secure Enclave. The proofs themselves come from
/// the core, through the real FFI; only the provider's answers are scripted.
final class DeviceProofTests: XCTestCase {
  private static let server = StoredServer(
    address: ServerAddress(
      origin: "https://team.example.com", host: "team.example.com", storageKey: "team.example.com",
      serverInfoUrl: "https://team.example.com/api/v1/server-info"),
    issuer: "https://id.example.com/realms/team", clientId: "teamcraft", scopes: ["openid"], displayName: "Example",
    serverVersion: "1.0.3", isLoginPageAlwaysShown: false, maybeLoginHint: nil, profileJson: "{}")
  private static let tokenEndpoint = URL(string: "https://id.example.com/token?x=1")!
  private static let tokenItem = "oidc-session.team.example.com"
  private static let keyItem = "dpop-key.team.example.com"

  private var suiteName = ""
  private var defaults: UserDefaults!
  private var servers: ServerStore!
  private var items: MemorySecretItems!

  override func setUpWithError() throws {
    suiteName = "DeviceProofTests-\(UUID().uuidString)"
    defaults = try XCTUnwrap(UserDefaults(suiteName: suiteName))
    items = MemorySecretItems()
    servers = ServerStore(defaults: defaults)
    servers.activate(
      ServerAccount(origin: "https://team.example.com", storageKey: "team.example.com", profileJson: "{}"))
    StubProtocol.reset()
  }

  override func tearDown() {
    defaults.removePersistentDomain(forName: suiteName)
    StubProtocol.reset()
  }

  // MARK: Proofs in a token request

  func testARefreshProofNamesTheTokenEndpointAndVerifiesWithTheStoredKey() throws {
    let key = try storedKey()
    let thumbprint = try thumbprint(of: key)
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.token(Self.boundToken(thumbprint))]

    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(thumbprint))
    XCTAssertEqual(vault.load()?.boundKey, thumbprint)

    let proof = try XCTUnwrap(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    let parts = proof.split(separator: ".").map(String.init)
    let header = try Self.json(parts[0])
    let claims = try Self.json(parts[1])
    XCTAssertEqual(header["typ"] as? String, "dpop+jwt")
    XCTAssertEqual(header["alg"] as? String, "ES256")
    XCTAssertEqual(claims["htm"] as? String, "POST")
    // No query: what RFC 9449 §4.2 asks and what the server compares.
    XCTAssertEqual(claims["htu"] as? String, "https://id.example.com/token")
    XCTAssertNil(claims["ath"], "no access token goes along at a token endpoint")
    XCTAssertNil(claims["nonce"])
    XCTAssertFalse(try XCTUnwrap(claims["jti"] as? String).isEmpty)
    let iat = try XCTUnwrap(claims["iat"] as? Double)
    XCTAssertEqual(iat, Date().timeIntervalSince1970, accuracy: 30)
    let signature = try P256.Signing.ECDSASignature(rawRepresentation: XCTUnwrap(PushPayload.decodeBase64URL(parts[2])))
    let verifier = try P256.Signing.PublicKey(x963Representation: key.publicPoint)
    XCTAssertTrue(verifier.isValidSignature(signature, for: Data("\(parts[0]).\(parts[1])".utf8)))
  }

  func testANonceChallengeGetsOneMoreTryWithTheNonce() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.oauthError("use_dpop_nonce", nonce: "provider-nonce"), .token(Self.boundToken(thumbprint))]

    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(thumbprint))

    XCTAssertEqual(StubProtocol.recorded.count, 2)
    let first = try claims(of: StubProtocol.recorded[0])
    let second = try claims(of: StubProtocol.recorded[1])
    XCTAssertNil(first["nonce"])
    XCTAssertEqual(second["nonce"] as? String, "provider-nonce")
    XCTAssertNotEqual(first["jti"] as? String, second["jti"] as? String)
  }

  /// One retry, never more: the provider's nonces are remembered per process only, so the extension
  /// meets the challenge in each run and must not ask for ever (issue #120).
  func testAProviderThatKeepsChallengingIsAskedTwiceAndTheSessionStays() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.oauthError("use_dpop_nonce", nonce: "one"), .oauthError("use_dpop_nonce", nonce: "two")]

    XCTAssertNil(vault.currentAccessToken(), "the token was expired and no new one came")

    XCTAssertEqual(StubProtocol.recorded.count, 2)
    XCTAssertNotNil(vault.load(), "a provider that keeps refusing proofs is no end of the session")
  }

  /// A wrong device clock: the provider's `Date` says so, and the one more try is dated by it.
  func testAProvidersDateCorrectsTheClockForTheOneMoreTry() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = TimeZone(identifier: "GMT")
    formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss 'GMT'"
    let serverNow = Date().addingTimeInterval(3_600)
    StubProtocol.answers = [
      .oauthErrorAt("invalid_request", date: formatter.string(from: serverNow)), .token(Self.boundToken(thumbprint)),
    ]

    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(thumbprint))

    XCTAssertEqual(StubProtocol.recorded.count, 2)
    let iat = try XCTUnwrap(try claims(of: StubProtocol.recorded[1])["iat"] as? Double)
    XCTAssertEqual(iat, serverNow.timeIntervalSince1970, accuracy: 30)
  }

  func testAProofGoesWithoutARedirect() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.redirect(to: URL(string: "https://elsewhere.example.com/token")!)]

    _ = vault.currentAccessToken()

    XCTAssertEqual(StubProtocol.recorded.count, 1, "a proof names one URL; the redirect isn't followed")
  }

  /// A key belongs to one server: a prover kept for another must not sign here.
  func testAServerSwitchedToSignsWithItsOwnKey() throws {
    let first = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: first))
    StubProtocol.answers = [.token(Self.boundToken(first))]
    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(first))

    servers.activate(
      ServerAccount(origin: "https://other.example.com", storageKey: "other.example.com", profileJson: "{}"))
    let other = try storedKey()
    let otherThumbprint = try thumbprint(of: other)
    XCTAssertNotEqual(first, otherThumbprint)
    try vault.save(session(boundKey: otherThumbprint))
    StubProtocol.clearRecorded()
    StubProtocol.answers = [.token(Self.boundToken(otherThumbprint))]

    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(otherThumbprint))

    let proof = try XCTUnwrap(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    let jwk = try XCTUnwrap(Self.json(String(proof.split(separator: ".")[0]))["jwk"] as? [String: Any])
    let publicKey = try P256.Signing.PublicKey(x963Representation: other.publicPoint)
    XCTAssertEqual(jwk["x"] as? String, Self.base64URL(publicKey.x963Representation.dropFirst().prefix(32)))
  }

  // MARK: A key that isn't there

  func testAMissingKeyGoesNoFurtherAndKeepsTheSession() throws {
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: "thumbprint-of-a-key-that-is-gone"))

    XCTAssertEqual(vault.currentAccessToken(), "old-access", "the core sees the binding and says why")

    XCTAssertTrue(StubProtocol.recorded.isEmpty, "nothing goes to the provider")
    XCTAssertNotNil(vault.load(), "a missing key signs nobody out")
    XCTAssertTrue(vault.isDeviceKeyLost)
  }

  /// The session is bound to another key than this device has: as lost as none.
  func testAnotherKeyIsALostKey() throws {
    _ = try storedKey()
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: "thumbprint-of-another-key"))

    XCTAssertEqual(vault.currentAccessToken(), "old-access")

    XCTAssertTrue(StubProtocol.recorded.isEmpty)
    XCTAssertTrue(vault.isDeviceKeyLost)
    XCTAssertNotNil(vault.load())
  }

  /// A locked keychain: nothing goes to the provider, the session stays, and the core gets a token
  /// it holds back instead of `nil`, which would read as signed out.
  func testAKeyThatCantBeReadIsAWaitAndNoSignOut() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    items.failsKeyReads = true

    XCTAssertEqual(vault.currentAccessToken(), "old-access")

    XCTAssertTrue(StubProtocol.recorded.isEmpty)
    XCTAssertNotNil(vault.load())
    XCTAssertFalse(vault.isDeviceKeyLost, "a key that can't be read is no lost key")
    items.failsKeyReads = false
    StubProtocol.answers = [.token(Self.boundToken(thumbprint))]
    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(thumbprint), "and works when it can be read again")
  }

  func testAFreshBoundTokenComesBackWithoutAKeyRead() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint, expiresIn: 600))
    items.failsKeyReads = true
    XCTAssertEqual(vault.currentAccessToken(), "old-access")
    XCTAssertTrue(StubProtocol.recorded.isEmpty)
  }

  // MARK: Sign-out

  func testASignOutTakesTheKeyAlong() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    XCTAssertNotNil(items.values[Self.keyItem])

    vault.clear()

    XCTAssertNil(items.values[Self.keyItem])
    XCTAssertNil(vault.load())
    XCTAssertFalse(vault.isDeviceKeyLost)
  }

  func testARefusedRefreshTokenEndsTheSessionAndTheKey() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.oauthError("invalid_grant", nonce: nil)]

    XCTAssertNil(vault.currentAccessToken())

    XCTAssertNil(vault.load())
    XCTAssertNil(items.values[Self.keyItem])
  }

  func testALocalAccountTakesTheKeyAlong() throws {
    _ = try storedKey()
    let (vault, _) = makeVault()
    try vault.saveLocal(LocalSession(accessToken: "local", expiresAt: Date().addingTimeInterval(600), userId: "7"))
    XCTAssertNil(items.values[Self.keyItem])
  }

  // MARK: Existing sessions and server policies

  func testASessionFromBeforeDecodesAsUnbound() throws {
    var stored = try XCTUnwrap(
      JSONSerialization.jsonObject(with: JSONEncoder().encode(session(boundKey: "x"))) as? [String: Any])
    stored["boundKey"] = nil
    items.values[Self.tokenItem] = try JSONSerialization.data(withJSONObject: stored)
    let (vault, _) = makeVault()
    XCTAssertEqual(vault.load()?.accessToken, "old-access")
    XCTAssertNil(vault.load()?.boundKey)
  }

  /// The extension leaves binding to the app: no proof, no key, for a session that isn't bound.
  func testTheExtensionNeitherBindsNorMakesAKey() throws {
    let (vault, asked) = makeVault(bindsSessions: false, offer: .offered, providerTakesProofs: true)
    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.token("unbound-access")]

    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")

    XCTAssertNil(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    XCTAssertNil(items.values[Self.keyItem])
    XCTAssertEqual(asked.count, 0, "and doesn't ask the server either")
  }

  /// The policy `off`, an older server, a proxy that strips the header: `server-info` has no nonce.
  func testAServerThatOffersNoProofsLeavesTheSessionUnbound() throws {
    let (vault, _) = makeVault(offer: .notOffered, providerTakesProofs: true)
    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.token("unbound-access")]

    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")

    XCTAssertNil(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    XCTAssertNil(items.values[Self.keyItem], "no key without a use for it")
    XCTAssertNil(vault.load()?.boundKey)
  }

  func testAProviderWithoutProofsLeavesTheSessionUnbound() throws {
    let (vault, asked) = makeVault(offer: .offered, providerTakesProofs: false)
    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.token("unbound-access")]

    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")

    XCTAssertNil(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    XCTAssertEqual(asked.count, 0, "the server isn't asked when the provider wouldn't do it anyway")
  }

  /// `optional` and `required` as an existing sign-in in the app sees them: its refresh binds it.
  func testAnExistingSessionGetsBoundByItsNextRefreshInTheApp() throws {
    let (vault, _) = makeVault(offer: .offered, providerTakesProofs: true)
    try vault.save(session(boundKey: nil))
    let key = try storedKey()
    let thumbprint = try thumbprint(of: key)
    StubProtocol.answers = [.token(Self.boundToken(thumbprint))]

    XCTAssertEqual(vault.currentAccessToken(), Self.boundToken(thumbprint))

    XCTAssertNotNil(StubProtocol.recorded.first?.value(forHTTPHeaderField: "DPoP"))
    XCTAssertEqual(vault.load()?.boundKey, thumbprint)
  }

  /// A provider may advertise proofs and not take them for this client: the session goes on unbound,
  /// and later refreshes of this run don't pay for the refusal.
  func testARefusedProofLeavesTheSessionUnboundAndIsNotTriedAgain() throws {
    let (vault, asked) = makeVault(offer: .offered, providerTakesProofs: true)
    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.oauthError("invalid_dpop_proof", nonce: nil), .token("unbound-access")]

    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")

    XCTAssertNotNil(StubProtocol.recorded[0].value(forHTTPHeaderField: "DPoP"))
    XCTAssertNil(StubProtocol.recorded[1].value(forHTTPHeaderField: "DPoP"))
    XCTAssertNil(vault.load()?.boundKey)

    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.token("unbound-access")]
    StubProtocol.clearRecorded()
    let beforeAsking = asked.count
    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")
    XCTAssertNil(StubProtocol.recorded[0].value(forHTTPHeaderField: "DPoP"))
    XCTAssertEqual(asked.count, beforeAsking)
  }

  /// A key made to bind a session that then can't sign doesn't stop the refresh, and doesn't stay.
  func testAKeyThatCantSignDoesntStopABindingRefresh() throws {
    let brokenKey = StoredDeviceKey(kind: .software, keyData: Data([0xff]), publicPoint: try storedKey().publicPoint)
    items.values[Self.keyItem] = try JSONEncoder().encode(brokenKey)
    let (vault, _) = makeVault(offer: .offered, providerTakesProofs: true)
    try vault.save(session(boundKey: nil))
    StubProtocol.answers = [.token("unbound-access")]

    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")

    XCTAssertEqual(StubProtocol.recorded.count, 1)
    XCTAssertNil(StubProtocol.recorded[0].value(forHTTPHeaderField: "DPoP"))
    XCTAssertNil(items.values[Self.keyItem], "an unusable key doesn't stay around")
  }

  /// A bound session whose refresh answers with an unbound token is unbound from then on, as when the
  /// provider stops binding for this client.
  func testAnUnboundAnswerUnbindsTheSession() throws {
    let thumbprint = try thumbprint(of: storedKey())
    let (vault, _) = makeVault()
    try vault.save(session(boundKey: thumbprint))
    StubProtocol.answers = [.token("unbound-access")]
    XCTAssertEqual(vault.currentAccessToken(), "unbound-access")
    XCTAssertNil(vault.load()?.boundKey)
  }

  // MARK: A new sign-in

  func testASignInGetsAProofOnlyWhenBothSidesWouldUseIt() async throws {
    let endpoints = Self.endpoints(takesProofs: true)

    let (offered, _) = makeProofs(offer: .offered)
    guard case .with(_, let fallback) = try await offered.forSignIn(endpoints: endpoints, server: Self.server) else {
      return XCTFail("the sign-in should be bound")
    }
    XCTAssertEqual(fallback, .onRefusal)
    XCTAssertNotNil(items.values[Self.keyItem])
    items.values[Self.keyItem] = nil

    let (withheld, _) = makeProofs(offer: .notOffered)
    guard case .none = try await withheld.forSignIn(endpoints: endpoints, server: Self.server) else {
      return XCTFail("no nonce, no binding")
    }
    XCTAssertNil(items.values[Self.keyItem], "no key without a use for it")

    let (noOffer, asked) = makeProofs(offer: .offered)
    guard case .none = try await noOffer.forSignIn(endpoints: Self.endpoints(takesProofs: false), server: Self.server)
    else {
      return XCTFail("a provider without proofs doesn't bind")
    }
    XCTAssertEqual(asked.count, 0)
  }

  /// A server that doesn't answer says nothing about proofs: the sign-in fails, after one more try.
  func testAServerThatDoesntAnswerFailsTheSignIn() async throws {
    let (proofs, asked) = makeProofs(offer: .noAnswer)
    do {
      _ = try await proofs.forSignIn(endpoints: Self.endpoints(takesProofs: true), server: Self.server)
      XCTFail("expected a network error")
    } catch OidcError.network {
      XCTAssertEqual(asked.count, 2)
    }
    XCTAssertNil(items.values[Self.keyItem])
  }

  func testAKeyThatCantBeKeptLeavesTheSignInUnbound() async throws {
    let keys = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil })
    let proofs = SessionProofs(
      keys: keys, servers: servers, bindsSessions: true, askServer: { _ in .offered },
      providerTakesProofs: { _ in true }, activeServer: { Self.server })
    items.failsWrites = true
    guard case .none = try await proofs.forSignIn(endpoints: Self.endpoints(takesProofs: true), server: Self.server)
    else {
      return XCTFail("a key that can't be kept is no reason to fail the sign-in")
    }
  }

  // MARK: The key itself

  func testTheEnclaveIsPreferredAndSoftwareIsTheFallback() throws {
    let made = StoredDeviceKey(kind: .secureEnclave, keyData: Data([1]), publicPoint: Data([4]))
    let withEnclave = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { made })
    XCTAssertEqual(try withEnclave.keyOrNew(), made)
    XCTAssertEqual(try withEnclave.keyOrNew(), made, "the second call finds the first key")
    withEnclave.delete()

    let refusing = DeviceKeyStore(
      keychain: items, servers: servers, makeEnclaveKey: { throw DeviceKeyStoreError.noEnclaveAccessControl })
    XCTAssertEqual(try refusing.keyOrNew().kind, .software)
    refusing.delete()

    let simulator = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil })
    XCTAssertEqual(try simulator.keyOrNew().kind, .software)
  }

  func testAKeyThatCantBeReadIsNeverOverwritten() throws {
    let store = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil })
    let key = try store.keyOrNew()
    let storedBytes = items.values[Self.keyItem]
    items.failsKeyReads = true
    XCTAssertThrowsError(try store.keyOrNew())
    XCTAssertEqual(items.values[Self.keyItem], storedBytes)
    items.failsKeyReads = false
    XCTAssertEqual(try store.read(), key)
  }

  func testTheCoreSignsWithTheStoredKeyAndNamesItsErrors() throws {
    let store = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil })
    let device = VaultDeviceKey(store: store)
    XCTAssertThrowsError(try device.publicKey()) { XCTAssertEqual($0 as? DeviceProofError, .KeyLost) }
    let key = try store.keyOrNew()
    XCTAssertEqual(try device.publicKey(), key.publicPoint)
    let signature = try device.sign(message: Data("hello".utf8))
    XCTAssertEqual(signature.count, 64)
    items.failsKeyReads = true
    XCTAssertThrowsError(try device.sign(message: Data())) { XCTAssertEqual($0 as? DeviceProofError, .KeyUnavailable) }
    XCTAssertThrowsError(try device.publicKey()) { XCTAssertEqual($0 as? DeviceProofError, .KeyUnavailable) }
  }

  func testAKeyNeverPrintsItsSecret() {
    let key = StoredDeviceKey(kind: .software, keyData: Data("secret".utf8), publicPoint: Data([4]))
    XCTAssertFalse(String(describing: key).contains("c2Vjcm"))
    XCTAssertFalse(String(reflecting: key).contains("secret"))
  }

  func testTheServersAnswerSaysWhatItSays() {
    XCTAssertEqual(ServerProofOffer(status: 200, maybeNonce: "n"), .offered)
    XCTAssertEqual(ServerProofOffer(status: 200, maybeNonce: nil), .notOffered)
    XCTAssertEqual(ServerProofOffer(status: 404, maybeNonce: nil), .notOffered)
    XCTAssertEqual(ServerProofOffer(status: 421, maybeNonce: "n"), .notOffered)
    XCTAssertEqual(ServerProofOffer(status: 503, maybeNonce: "n"), .noAnswer)
    XCTAssertEqual(ServerProofOffer(status: 302, maybeNonce: nil), .noAnswer)
  }

  func testTheDiscoveryDocumentNamesProofs() throws {
    let json = """
      {"issuer":"https://id.example.com","authorization_endpoint":"https://id.example.com/a",
      "token_endpoint":"https://id.example.com/t","dpop_signing_alg_values_supported":["RS256","ES256"]}
      """
    XCTAssertTrue(try JSONDecoder().decode(OidcClient.Endpoints.self, from: Data(json.utf8)).takesProofs)
    let without = """
      {"issuer":"https://id.example.com","authorization_endpoint":"https://id.example.com/a",
      "token_endpoint":"https://id.example.com/t"}
      """
    XCTAssertFalse(try JSONDecoder().decode(OidcClient.Endpoints.self, from: Data(without.utf8)).takesProofs)
  }

  // MARK: Helpers

  private func storedKey() throws -> StoredDeviceKey {
    try DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil }).keyOrNew()
  }

  private func thumbprint(of key: StoredDeviceKey) throws -> String {
    try ProviderProver(key: StandInDeviceKey(key: key)).thumbprint()
  }

  private func makeVault(
    bindsSessions: Bool = true, offer: ServerProofOffer = .notOffered, providerTakesProofs: Bool = false
  ) -> (SessionVault, AskCounter) {
    let (proofs, asked) = makeProofs(
      bindsSessions: bindsSessions, offer: offer, providerTakesProofs: providerTakesProofs)
    let vault = SessionVault(
      keychain: items, servers: servers, refreshLockURL: nil, refreshTimeout: 5, proofs: proofs,
      urlSession: StubProtocol.session)
    return (vault, asked)
  }

  private func makeProofs(
    bindsSessions: Bool = true, offer: ServerProofOffer, providerTakesProofs: Bool = true
  ) -> (SessionProofs, AskCounter) {
    let asked = AskCounter()
    let keys = DeviceKeyStore(keychain: items, servers: servers, makeEnclaveKey: { nil })
    let proofs = SessionProofs(
      keys: keys, servers: servers, bindsSessions: bindsSessions,
      askServer: { _ in
        asked.increment()
        return offer
      },
      providerTakesProofs: { _ in providerTakesProofs }, activeServer: { Self.server })
    return (proofs, asked)
  }

  private func session(boundKey: String?, expiresIn: TimeInterval = -60) -> OidcSession {
    OidcSession(
      accessToken: "old-access", refreshToken: "old-refresh", idToken: nil,
      expiresAt: Date().addingTimeInterval(expiresIn), subject: "user-1", displayName: nil,
      tokenEndpoint: Self.tokenEndpoint, clientId: "teamcraft", loginHint: nil, boundKey: boundKey)
  }

  private func claims(of request: URLRequest) throws -> [String: Any] {
    let proof = try XCTUnwrap(request.value(forHTTPHeaderField: "DPoP"))
    return try Self.json(String(proof.split(separator: ".")[1]))
  }

  private static func endpoints(takesProofs: Bool) -> OidcClient.Endpoints {
    OidcClient.Endpoints(
      issuer: nil, authorizationEndpoint: URL(string: "https://id.example.com/auth")!,
      tokenEndpoint: tokenEndpoint, dpopSigningAlgValuesSupported: takesProofs ? ["ES256"] : nil)
  }

  private static func json(_ segment: String) throws -> [String: Any] {
    let data = try XCTUnwrap(PushPayload.decodeBase64URL(segment))
    return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
  }

  /// A JWT whose payload names `thumbprint` as `cnf.jkt`, as the core reads a bound token.
  private static func boundToken(_ thumbprint: String) -> String {
    let claims = #"{"cnf":{"jkt":"\#(thumbprint)"}}"#
    return "e30.\(base64URL(Data(claims.utf8))).c2ln"
  }

  private static func base64URL(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }
}

/// The key without the store, to read its thumbprint in a test.
private final class StandInDeviceKey: DeviceProofKey, @unchecked Sendable {
  private let key: StoredDeviceKey

  init(key: StoredDeviceKey) {
    self.key = key
  }

  func publicKey() throws -> Data { key.publicPoint }

  func sign(message: Data) throws -> Data { try key.signature(of: message) }
}

/// How often the server was asked.
private final class AskCounter: @unchecked Sendable {
  private let lock = NSLock()
  private var value = 0

  var count: Int { lock.withLock { value } }

  func increment() {
    lock.withLock { value += 1 }
  }
}

/// The keychain, in memory, that can refuse to read the key like a locked one, or to write.
private final class MemorySecretItems: SecretItems, @unchecked Sendable {
  private let lock = NSLock()
  private var stored: [String: Data] = [:]
  private var isKeyReadFailing = false
  private var isWriteFailing = false

  var values: [String: Data] {
    get { lock.withLock { stored } }
    set { lock.withLock { stored = newValue } }
  }

  var failsKeyReads: Bool {
    get { lock.withLock { isKeyReadFailing } }
    set { lock.withLock { isKeyReadFailing = newValue } }
  }

  var failsWrites: Bool {
    get { lock.withLock { isWriteFailing } }
    set { lock.withLock { isWriteFailing = newValue } }
  }

  func read(_ account: String) throws -> Data? {
    try lock.withLock {
      if isKeyReadFailing, account.hasPrefix("dpop-key.") {
        throw KeychainError.unexpectedStatus(errSecInteractionNotAllowed)
      }
      return stored[account]
    }
  }

  func write(_ data: Data, for account: String) throws {
    try lock.withLock {
      if isWriteFailing { throw KeychainError.unexpectedStatus(errSecNotAvailable) }
      stored[account] = data
    }
  }

  func delete(_ account: String) throws {
    lock.withLock { stored[account] = nil }
  }
}

/// A scripted identity provider.
private final class StubProtocol: URLProtocol {
  enum Answer {
    case token(String)
    case oauthError(String, nonce: String?)
    case oauthErrorAt(String, date: String)
    case redirect(to: URL)
  }

  private static let lock = NSLock()
  private static var scripted: [Answer] = []
  private static var requests: [URLRequest] = []

  static let session: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [StubProtocol.self]
    return URLSession(configuration: configuration)
  }()

  static var answers: [Answer] {
    get { lock.withLock { scripted } }
    set { lock.withLock { scripted = newValue } }
  }

  static var recorded: [URLRequest] { lock.withLock { requests } }

  static func clearRecorded() {
    lock.withLock { requests = [] }
  }

  static func reset() {
    lock.withLock {
      scripted = []
      requests = []
    }
  }

  override class func canInit(with request: URLRequest) -> Bool { true }
  override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

  override func startLoading() {
    let answer: Answer? = Self.lock.withLock {
      Self.requests.append(request)
      return Self.scripted.isEmpty ? nil : Self.scripted.removeFirst()
    }
    guard let answer, let url = request.url else {
      client?.urlProtocol(self, didFailWithError: URLError(.notConnectedToInternet))
      return
    }
    switch answer {
    case .token(let token):
      finish(status: 200, headers: [:], body: Data(#"{"access_token":"\#(token)","expires_in":300}"#.utf8), url: url)
    case .oauthError(let code, let nonce):
      let headers = nonce.map { ["DPoP-Nonce": $0] } ?? [:]
      finish(status: 400, headers: headers, body: Data(#"{"error":"\#(code)"}"#.utf8), url: url)
    case .oauthErrorAt(let code, let date):
      finish(status: 400, headers: ["Date": date], body: Data(#"{"error":"\#(code)"}"#.utf8), url: url)
    case .redirect(let target):
      let response = HTTPURLResponse(
        url: url, statusCode: 302, httpVersion: "HTTP/1.1", headerFields: ["Location": target.absoluteString])!
      client?.urlProtocol(self, wasRedirectedTo: URLRequest(url: target), redirectResponse: response)
      // A refused redirect ends in this answer; a followed one would come back as a new request.
      client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
      client?.urlProtocolDidFinishLoading(self)
    }
  }

  override func stopLoading() {}

  private func finish(status: Int, headers: [String: String], body: Data, url: URL) {
    let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
    client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
    client?.urlProtocol(self, didLoad: body)
    client?.urlProtocolDidFinishLoading(self)
  }
}
