import XCTest

@testable import TeamCraft

final class SignInTests: XCTestCase {
  private static let grantedJSON =
    #"{"access_token":"token-a","expires_at":1767225600000,"user_id":"9007199254740993"}"#

  // MARK: Server answer

  func testAGrantedAnswerBecomesALocalSession() {
    let result = LocalSignInResult.from(status: 200, body: Data(Self.grantedJSON.utf8))
    let expected = LocalSession(
      accessToken: "token-a", expiresAt: Date(timeIntervalSince1970: 1_767_225_600), userId: "9007199254740993")
    XCTAssertEqual(result, .signedIn(expected))
  }

  func testANumericUserIdIsAccepted() {
    let json = #"{"access_token":"token-a","expires_at":1767225600000,"user_id":42}"#
    guard case .signedIn(let session) = LocalSignInResult.from(status: 200, body: Data(json.utf8)) else {
      return XCTFail("expected a session")
    }
    XCTAssertEqual(session.userId, "42")
  }

  func testAnUnreadableGrantIsAGenericFailure() {
    XCTAssertEqual(LocalSignInResult.from(status: 200, body: Data("<html>Hotel Wi-Fi</html>".utf8)), .failed(.generic))
    XCTAssertEqual(
      LocalSignInResult.from(status: 200, body: Data(#"{"access_token":"","expires_at":1,"user_id":"1"}"#.utf8)),
      .failed(.generic))
  }

  func testWrongCredentialsAndCodesGetOneAnswer() {
    XCTAssertEqual(result(401, code: "unauthorized"), .failed(.wrongCredentials))
    XCTAssertEqual(result(403, code: "mfa_required"), .failed(.wrongCredentials))
    XCTAssertEqual(LocalSignInResult.from(status: 401, body: Data()), .failed(.wrongCredentials))
  }

  func testDisabledLocalAccountsAreReported() {
    XCTAssertEqual(result(403, code: "local_accounts_disabled"), .failed(.disabled))
    XCTAssertEqual(LocalSignInResult.from(status: 503, body: Data()), .failed(.disabled))
    XCTAssertEqual(LocalSignInResult.from(status: 404, body: Data()), .failed(.disabled))
  }

  func testRateLimitsAreReported() {
    XCTAssertEqual(result(429, code: "rate_limited"), .failed(.rateLimited))
    XCTAssertEqual(LocalSignInResult.from(status: 429, body: Data()), .failed(.rateLimited))
  }

  func testOtherAnswersAreGeneric() {
    XCTAssertEqual(LocalSignInResult.from(status: 500, body: Data()), .failed(.generic))
    XCTAssertEqual(LocalSignInResult.from(status: 302, body: Data()), .failed(.generic))
  }

  func testTheRequestBodyLeavesOutAMissingCode() throws {
    let encoder = JSONEncoder()
    let without = try encoder.encode(LocalLoginRequestBody(login: "a@example.com", password: "p", totpCode: nil))
    let withCode = try encoder.encode(LocalLoginRequestBody(login: "a@example.com", password: "p", totpCode: "123456"))
    XCTAssertNil(try object(without)["totp_code"])
    XCTAssertEqual(try object(withCode)["totp_code"] as? String, "123456")
    XCTAssertEqual(try object(withCode)["login"] as? String, "a@example.com")
  }

  // MARK: Local session

  func testALocalSessionRoundTripsThroughItsStoredForm() throws {
    let session = LocalSession(
      accessToken: "token-a", expiresAt: Date(timeIntervalSince1970: 1_767_225_600), userId: "7")
    let decoded = try JSONDecoder().decode(LocalSession.self, from: JSONEncoder().encode(session))
    XCTAssertEqual(decoded, session)
  }

  func testALocalSessionIsUsableUntilItsExpiryPlusTheLeeway() {
    let now = Date()
    let session = LocalSession(accessToken: "t", expiresAt: now, userId: "7")
    XCTAssertTrue(session.isUsable(now: now.addingTimeInterval(-60), leeway: 0))
    XCTAssertFalse(session.isUsable(now: now.addingTimeInterval(1), leeway: 0))
    XCTAssertTrue(session.isUsable(now: now.addingTimeInterval(200), leeway: 300))
    XCTAssertFalse(session.isUsable(now: now.addingTimeInterval(301), leeway: 300))
  }

  func testALocalSessionNeverPrintsItsToken() {
    let session = LocalSession(accessToken: "secret-token", expiresAt: Date(), userId: "7")
    XCTAssertFalse(String(describing: session).contains("secret-token"))
    XCTAssertFalse(String(reflecting: session).contains("secret-token"))
  }

  // MARK: Layout

  func testOneMethodShowsOneSection() {
    XCTAssertEqual(SignInSection.layout(methods: [.sso]), [.sso])
    XCTAssertEqual(SignInSection.layout(methods: [.local]), [.local])
  }

  func testTwoMethodsKeepTheServerOrderWithADivider() {
    XCTAssertEqual(SignInSection.layout(methods: [.local, .sso]), [.local, .divider, .sso])
    XCTAssertEqual(SignInSection.layout(methods: [.sso, .local]), [.sso, .divider, .local])
  }

  func testRepeatedOrMissingMethodsAreHandled() {
    XCTAssertEqual(SignInSection.layout(methods: [.sso, .sso, .local, .sso]), [.sso, .divider, .local])
    XCTAssertEqual(SignInSection.layout(methods: []), [.sso])
  }

  // MARK: Form

  func testTheFormNeedsEmailAndPassword() {
    var form = LocalSignInForm()
    XCTAssertFalse(form.canSubmit(requiresCode: false))
    form.email = "  a@example.com "
    XCTAssertFalse(form.canSubmit(requiresCode: false))
    form.password = "p"
    XCTAssertTrue(form.canSubmit(requiresCode: false))
    XCTAssertEqual(form.trimmedEmail, "a@example.com")
    XCTAssertNil(form.maybeCode)
  }

  func testARequiredCodeMustBeComplete() {
    var form = LocalSignInForm(email: "a@example.com", password: "p")
    XCTAssertFalse(form.canSubmit(requiresCode: true))
    form.code = "123"
    XCTAssertFalse(form.canSubmit(requiresCode: true))
    XCTAssertFalse(form.canSubmit(requiresCode: false))
    form.code = "123456"
    XCTAssertTrue(form.canSubmit(requiresCode: true))
    XCTAssertEqual(form.maybeCode, "123456")
  }

  func testAFailureClearsTheCodeAndAfterAWrongAnswerThePassword() {
    var form = LocalSignInForm(email: "a@example.com", password: "p", code: "123456")
    form.apply(.rateLimited)
    XCTAssertEqual(form.password, "p")
    XCTAssertEqual(form.code, "")
    XCTAssertEqual(form.maybeFailure, .rateLimited)
    form.code = "654321"
    form.apply(.wrongCredentials)
    XCTAssertEqual(form.password, "")
    XCTAssertEqual(form.code, "")
    XCTAssertEqual(form.email, "a@example.com")
  }

  func testCodesKeepOnlyAsciiDigits() {
    XCTAssertEqual(LocalSignInForm.sanitizedCode("123 456"), "123456")
    XCTAssertEqual(LocalSignInForm.sanitizedCode("12345678"), "123456")
    XCTAssertEqual(LocalSignInForm.sanitizedCode("\u{0661}\u{0662}abc"), "")
  }

  // MARK: Helpers

  private func result(_ status: Int, code: String) -> LocalSignInResult {
    let body = #"{"error_code":"\#(code)","message":"refused"}"#
    return LocalSignInResult.from(status: status, body: Data(body.utf8))
  }

  private func object(_ data: Data) throws -> [String: Any] {
    try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
  }
}
