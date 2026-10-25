import XCTest

@testable import TeamCraft

final class OidcTests: XCTestCase {
  private let environment = AppEnvironment(
    oidcRedirectScheme: "de.zeitgleich.teamcraft", appGroupId: "group.de.zeitgleich.teamcraft",
    keychainAccessGroup: "", clientVersion: "1.0.0")

  func testPkceMatchesTheRfcTestVector() {
    // RFC 7636 appendix B.
    let pkce = Pkce(verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk")
    XCTAssertEqual(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
  }

  func testGeneratedVerifiersAreLongEnoughAndDiffer() {
    let first = Pkce.generate()
    XCTAssertGreaterThanOrEqual(first.verifier.count, 43)
    XCTAssertNotEqual(first.verifier, Pkce.generate().verifier)
    XCTAssertFalse(first.verifier.contains("="))
  }

  @MainActor
  func testAuthorizationURLCarriesPkceAndRedirect() throws {
    let endpoints = OidcClient.Endpoints(
      authorizationEndpoint: URL(string: "https://id.example.com/auth")!,
      tokenEndpoint: URL(string: "https://id.example.com/token")!)
    let client = OidcClient(
      issuer: URL(string: "https://id.example.com/realms/team")!, clientId: "teamcraft",
      scopes: ["openid", "profile", "email", "offline_access"], redirectURI: environment.oidcRedirectURI)
    let url = WebLogin.authorizationURL(endpoints: endpoints, client: client, pkce: Pkce(verifier: "v"), state: "s1")
    let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
    func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
    XCTAssertEqual(value("response_type"), "code")
    XCTAssertEqual(value("client_id"), "teamcraft")
    XCTAssertEqual(value("redirect_uri"), "de.zeitgleich.teamcraft:/oauth/callback")
    // The scopes the server announced, not a fixed list.
    XCTAssertEqual(value("scope"), "openid profile email offline_access")
    XCTAssertEqual(value("code_challenge_method"), "S256")
    XCTAssertEqual(value("state"), "s1")
    // Without login parameters (a default call) none of them is sent.
    XCTAssertNil(value("prompt"))
    XCTAssertNil(value("login_hint"))
  }

  /// Issue #448: the login page with the last account and the app's language.
  @MainActor
  func testAuthorizationURLAsksForTheLoginPageWithHintAndLanguage() throws {
    let endpoints = OidcClient.Endpoints(
      authorizationEndpoint: URL(string: "https://id.example.com/auth")!,
      tokenEndpoint: URL(string: "https://id.example.com/token")!)
    let client = OidcClient(
      issuer: URL(string: "https://id.example.com/realms/team")!, clientId: "teamcraft",
      scopes: ["openid"], redirectURI: environment.oidcRedirectURI)
    let login = LoginParameters(prompt: "login", loginHint: "alice@example.com", uiLocales: "de")
    let url = WebLogin.authorizationURL(
      endpoints: endpoints, client: client, pkce: Pkce(verifier: "v"), state: "s1", login: login)
    let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
    func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
    XCTAssertEqual(value("prompt"), "login")
    XCTAssertEqual(value("login_hint"), "alice@example.com")
    XCTAssertEqual(value("ui_locales"), "de")
  }

  @MainActor
  func testAuthorizationCodeRequiresTheExpectedState() throws {
    let good = URL(string: "de.zeitgleich.teamcraft:/oauth/callback?code=abc&state=s1")!
    XCTAssertEqual(try WebLogin.authorizationCode(from: good, expectedState: "s1"), "abc")
    XCTAssertThrowsError(try WebLogin.authorizationCode(from: good, expectedState: "other")) {
      XCTAssertEqual($0 as? OidcError, .stateMismatch)
    }
    let denied = URL(string: "de.zeitgleich.teamcraft:/oauth/callback?error=access_denied&state=s1")!
    XCTAssertThrowsError(try WebLogin.authorizationCode(from: denied, expectedState: "s1")) {
      XCTAssertEqual($0 as? OidcError, .invalidResponse)
    }
  }

  func testIdTokenClaimsAreReadFromThePayload() {
    let payload = Data(#"{"sub":"user-1","name":"Alex Example"}"#.utf8)
    let jwt = "e30." + Pkce.base64URL(payload) + ".sig"
    XCTAssertEqual(IdTokenClaims(jwt: jwt)?.subject, "user-1")
    XCTAssertEqual(IdTokenClaims(jwt: jwt)?.name, "Alex Example")
    XCTAssertNil(IdTokenClaims(jwt: "not-a-jwt"))
  }

  func testDiscoveryMustBeTheIssuersOwnAndHttps() throws {
    let issuer = URL(string: "https://id.example.com/realms/team")!
    let good = OidcClient.Endpoints(
      issuer: issuer.absoluteString, authorizationEndpoint: URL(string: "https://id.example.com/auth")!,
      tokenEndpoint: URL(string: "https://id.example.com/token")!)
    XCTAssertTrue(good.isTrusted(for: issuer))
    var otherIssuer = good
    otherIssuer.issuer = "https://evil.example"
    XCTAssertFalse(otherIssuer.isTrusted(for: issuer))
    XCTAssertFalse(OidcClient.Endpoints.isSafe(URL(string: "file:///etc/passwd")!))
    XCTAssertFalse(OidcClient.Endpoints.isSafe(URL(string: "https://user:pw@id.example.com/x")!))
  }

  func testIdTokensMustBeForThisIssuerAndClient() throws {
    func jwt(_ claims: [String: Any]) -> String {
      let payload = try! JSONSerialization.data(withJSONObject: claims)
      let encoded = payload.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
      return "e30.\(encoded).sig"
    }
    let issuer = "https://id.example.com/realms/team"
    let good = IdTokenClaims(jwt: jwt(["sub": "u1", "iss": issuer, "aud": ["teamcraft", "other"]]))
    XCTAssertEqual(good?.isIssued(by: issuer, to: "teamcraft"), true)
    let single = IdTokenClaims(jwt: jwt(["sub": "u1", "iss": issuer, "aud": "teamcraft"]))
    XCTAssertEqual(single?.isIssued(by: issuer, to: "teamcraft"), true)
    let foreign = IdTokenClaims(jwt: jwt(["sub": "u1", "iss": "https://evil.example", "aud": "teamcraft"]))
    XCTAssertEqual(foreign?.isIssued(by: issuer, to: "teamcraft"), false)
    XCTAssertEqual(single?.isIssued(by: issuer, to: "another-client"), false)
  }

  func testFormEncodingEscapesReservedCharacters() {
    XCTAssertEqual(OidcClient.formEncoded(["b": "x y&z", "a": "1"]), "a=1&b=x%20y%26z")
  }

  func testSessionFreshnessHonoursTheLeeway() {
    let now = Date()
    let session = OidcSession(
      accessToken: "t", refreshToken: nil, expiresAt: now.addingTimeInterval(20), subject: "s",
      displayName: nil, tokenEndpoint: URL(string: "https://id.example.com/token")!, clientId: "teamcraft")
    XCTAssertFalse(session.isFresh(leeway: 30, now: now))
    XCTAssertTrue(session.isFresh(leeway: 10, now: now))
  }
}
