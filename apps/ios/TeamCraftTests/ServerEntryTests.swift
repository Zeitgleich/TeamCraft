import XCTest

@testable import TeamCraft

final class ServerEntryTests: XCTestCase {
  private static let team = "https://team.example.com"
  private static let other = "https://other.example.com"

  // MARK: Start decision

  func testAStoredServerIsUsedWhenNothingIsLocked() {
    let managed = ManagedServer(maybeAddress: "other.example.com", isLocked: false)
    XCTAssertEqual(decide(stored: Self.team, managed), .useStored)
  }

  func testAStoredServerIsUsedWhenItIsTheLockedOne() {
    let managed = ManagedServer(maybeAddress: "team.example.com", isLocked: true)
    XCTAssertEqual(decide(stored: Self.team, managed), .useStored)
  }

  func testALockOnAnotherServerSignsTheStoredAccountOut() {
    let managed = ManagedServer(maybeAddress: "other.example.com", isLocked: true)
    XCTAssertEqual(decide(stored: Self.team, managed), .forgetStoredAndCheck(address: "other.example.com"))
  }

  func testWithoutAnAccountAPreconfiguredServerIsChecked() {
    let managed = ManagedServer(maybeAddress: "team.example.com", isLocked: false)
    XCTAssertEqual(decide(stored: nil, managed), .check(address: "team.example.com"))
  }

  func testWithoutAnythingTheUserEntersTheServer() {
    XCTAssertEqual(decide(stored: nil, ManagedServer(maybeAddress: nil, isLocked: false)), .enterServer)
  }

  func testALockedServerRefusesOthersAndAnUnlockedOneAllowsAll() {
    let locked = ManagedServer(maybeAddress: "team.example.com", isLocked: true)
    XCTAssertTrue(locked.allows(origin: Self.team, normalize: normalize))
    XCTAssertFalse(locked.allows(origin: Self.other, normalize: normalize))
    let open = ManagedServer(maybeAddress: "team.example.com", isLocked: false)
    XCTAssertTrue(open.allows(origin: Self.other, normalize: normalize))
  }

  // MARK: Managed configuration

  func testTheManagedConfigurationWinsOverTheBuildDefault() {
    let managed: [String: Any] = ["server_url": " team.example.com ", "lock_server": true]
    XCTAssertEqual(
      ManagedServer.resolve(managed: managed, buildDefault: "other.example.com"),
      ManagedServer(maybeAddress: "team.example.com", isLocked: true))
  }

  func testTheBuildDefaultIsNeverLocked() {
    XCTAssertEqual(
      ManagedServer.resolve(managed: ["lock_server": true], buildDefault: "team.example.com"),
      ManagedServer(maybeAddress: "team.example.com", isLocked: false))
  }

  func testEmptyOrMistypedValuesMeanNoServer() {
    XCTAssertEqual(
      ManagedServer.resolve(managed: ["server_url": "  ", "lock_server": true], buildDefault: ""),
      ManagedServer(maybeAddress: nil, isLocked: false))
    XCTAssertEqual(
      ManagedServer.resolve(managed: ["server_url": 42], buildDefault: nil),
      ManagedServer(maybeAddress: nil, isLocked: false))
    XCTAssertEqual(
      ManagedServer.resolve(managed: ["server_url": "team.example.com", "lock_server": "yes"], buildDefault: nil),
      ManagedServer(maybeAddress: "team.example.com", isLocked: false))
  }

  func testTheManagedConfigurationIsReadFromItsDefaultsKey() throws {
    let defaults = try XCTUnwrap(UserDefaults(suiteName: "ServerEntryTests.managed"))
    defer { defaults.removePersistentDomain(forName: "ServerEntryTests.managed") }
    defaults.set(["server_url": "team.example.com", "lock_server": true], forKey: ManagedServer.configurationKey)
    let environment = AppEnvironment(
      maybeDefaultServerURL: "other.example.com", oidcRedirectScheme: "de.zeitgleich.teamcraft",
      appGroupId: "", keychainAccessGroup: "", clientVersion: "1.0.0")
    XCTAssertEqual(
      ManagedServer.current(defaults: defaults, environment: environment),
      ManagedServer(maybeAddress: "team.example.com", isLocked: true))
  }

  // MARK: Failures

  func testTransportErrorsMapToWhatTheUserCanDo() {
    for code: URLError.Code in [.notConnectedToInternet, .cannotFindHost, .cannotConnectToHost, .timedOut,
      .networkConnectionLost]
    {
      XCTAssertEqual(ServerFailure(transport: URLError(code)), .unreachable, "\(code)")
    }
    for code: URLError.Code in [.secureConnectionFailed, .serverCertificateUntrusted, .serverCertificateHasBadDate,
      .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid, .clientCertificateRequired]
    {
      XCTAssertEqual(ServerFailure(transport: URLError(code)), .tls, "\(code)")
    }
    XCTAssertEqual(ServerFailure(transport: URLError(.appTransportSecurityRequiresSecureConnection)), .httpsOnly)
    XCTAssertEqual(ServerFailure(transport: URLError(.dataLengthExceedsMaximum)), .notTeamCraft)
  }

  func testCoreRefusalsMapByVariantWhateverTheirSpelling() {
    let expected: [(String, ServerFailure)] = [
      ("Empty(message: \"no server address\")", .invalidAddress),
      ("InvalidAddress(message: \"x\")", .invalidAddress),
      ("HasCredentials(message: \"x\")", .invalidAddress),
      ("HttpsOnly(message: \"x\")", .httpsOnly),
      ("NotTeamCraft(message: \"x\")", .notTeamCraft),
      ("CaptivePortal(message: \"x\")", .captivePortal),
      ("Unavailable(message: \"HTTP 503\")", .unavailable),
      ("AppTooOld(message: \"2.0.0\")", .appTooOld),
      ("ServerTooOld(message: \"0.9.0\")", .serverTooOld),
      ("InvalidSignIn(message: \"x\")", .invalidSignIn),
      ("RedirectNotAllowed(message: \"x\")", .notSetUpForApp),
      ("notTeamCraft(message: \"x\")", .notTeamCraft),
      ("SomethingNew(message: \"x\")", .notTeamCraft),
    ]
    for (description, failure) in expected {
      XCTAssertEqual(ServerFailure(entryErrorDescription: description), failure, description)
    }
  }

  // MARK: Store

  func testTheStoreKeepsOneEntryPerServerAndTheActiveOne() throws {
    let suite = "ServerEntryTests.store"
    let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    let store = ServerStore(defaults: defaults)
    XCTAssertNil(store.active)

    store.activate(account(Self.team, "team.example.com", profile: "old"))
    store.activate(account(Self.other, "other.example.com", profile: "other"))
    store.activate(account(Self.team, "team.example.com", profile: "new"))

    let reloaded = ServerStore(defaults: defaults)
    XCTAssertEqual(reloaded.accounts.count, 2)
    XCTAssertEqual(reloaded.active, account(Self.team, "team.example.com", profile: "new"))

    reloaded.forgetActive()
    XCTAssertNil(ServerStore(defaults: defaults).active)
    XCTAssertEqual(ServerStore(defaults: defaults).accounts.map(\.origin), [Self.other])
  }

  func testAnUnreadableListMeansNoAccounts() throws {
    let suite = "ServerEntryTests.corrupt"
    let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
    defer { defaults.removePersistentDomain(forName: suite) }
    defaults.set(Data("{ not json".utf8), forKey: "serverAccounts")
    defaults.set(Self.team, forKey: "activeServerOrigin")
    let store = ServerStore(defaults: defaults)
    XCTAssertTrue(store.accounts.isEmpty)
    XCTAssertNil(store.active)
    XCTAssertNil(store.activeServer)
  }

  // MARK: Helpers

  private func decide(stored: String?, _ managed: ManagedServer) -> ServerStartDecision {
    ServerStartDecision.decide(maybeStoredOrigin: stored, managed: managed, normalize: normalize)
  }

  /// Stands in for `normalizeServerAddress` (tested in the core): adds the scheme.
  private func normalize(_ address: String) -> String? {
    let trimmed = address.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty else { return nil }
    return trimmed.hasPrefix("https://") ? trimmed : "https://" + trimmed
  }

  private func account(_ origin: String, _ storageKey: String, profile: String) -> ServerAccount {
    ServerAccount(origin: origin, storageKey: storageKey, profileJson: profile)
  }
}
