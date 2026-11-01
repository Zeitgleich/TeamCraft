import XCTest

@testable import TeamCraft

final class FieldErrorsTests: XCTestCase {
  private static let goodPassphrase = "correct horse battery"

  // MARK: Errors

  func testFieldsWithoutAProblemAreLeftOut() {
    let errors = FieldErrors<NewPassphraseField, PassphraseProblem>([
      .passphrase: nil, .repeated: nil,
    ])
    XCTAssertTrue(errors.isValid)
    XCTAssertNil(errors.maybeFirst)
  }

  func testTheFirstInvalidFieldIsTheTopmost() {
    let errors = FieldErrors<NewPassphraseField, PassphraseProblem>([
      .repeated: .mismatch, .passphrase: .tooShort,
    ])
    XCTAssertEqual(errors.maybeFirst, .passphrase)
  }

  func testAServerRefusalGoesToItsField() {
    let errors = FieldErrors<NewPassphraseField, PassphraseProblem>().with(.wrong, at: .passphrase)
    XCTAssertEqual(errors[.passphrase], .wrong)
    XCTAssertEqual(errors.with(nil, at: .repeated), errors)
  }

  // MARK: Passphrases

  func testBothPassphraseErrorsShowAtOnce() {
    let errors = PassphraseRules.errors(passphrase: "short", repeated: "")
    XCTAssertEqual(errors[.passphrase], .tooShort)
    XCTAssertEqual(errors[.repeated], .repeatMissing)
    XCTAssertEqual(errors.maybeFirst, .passphrase)
  }

  func testADifferentRepetitionIsSaidAtTheSecondField() {
    let errors = PassphraseRules.errors(
      passphrase: Self.goodPassphrase, repeated: Self.goodPassphrase + "!")
    XCTAssertNil(errors[.passphrase])
    XCTAssertEqual(errors[.repeated], .mismatch)
    XCTAssertTrue(
      PassphraseRules.errors(passphrase: Self.goodPassphrase, repeated: Self.goodPassphrase).isValid
    )
  }

  func testASinglePassphraseIsMissingOrTooShort() {
    XCTAssertEqual(PassphraseRules.problem(passphrase: ""), .missing)
    XCTAssertEqual(
      PassphraseRules.problem(
        passphrase: String(repeating: "a", count: PassphraseRules.minimumLength - 1)), .tooShort)
    XCTAssertNil(
      PassphraseRules.problem(
        passphrase: String(repeating: "a", count: PassphraseRules.minimumLength)))
  }

  // MARK: Sign-in

  func testEveryMissingSignInFieldIsNamedAtOnce() {
    var form = LocalSignInForm()
    form.email = "  "
    let errors = form.errors(requiresCode: true)
    XCTAssertEqual(errors[.email], .emailMissing)
    XCTAssertEqual(errors[.password], .passwordMissing)
    XCTAssertEqual(errors[.code], .codeMissing)
    XCTAssertEqual(errors.maybeFirst, .email)
  }

  func testAnOptionalCodeIsEmptyOrComplete() {
    var form = LocalSignInForm()
    form.email = "user@example.com"
    form.password = "secret"
    XCTAssertTrue(form.errors(requiresCode: false).isValid)
    form.code = "123"
    XCTAssertEqual(form.errors(requiresCode: false)[.code], .codeIncomplete)
    XCTAssertEqual(form.errors(requiresCode: false).maybeFirst, .code)
  }

  func testOnlyWrongCredentialsGoToTheField() {
    XCTAssertEqual(LocalSignInFailure.wrongCredentials.maybeField, .password)
    for failure in [LocalSignInFailure.disabled, .rateLimited, .unreachable, .generic] {
      XCTAssertNil(failure.maybeField)
    }
  }

  // MARK: Single fields

  func testTheNewDevicesCodeHasSixDigits() {
    XCTAssertEqual(LinkCode.problem(typed: ""), .missing("link_code_missing"))
    XCTAssertEqual(LinkCode.problem(typed: "123 45"), .incomplete("link_code_incomplete"))
    XCTAssertNil(LinkCode.problem(typed: "123 456"))
  }

  func testABlankTextIsRefused() {
    XCTAssertEqual(
      TextProblem.required(" ", missing: "channel_name_missing"), .missing("channel_name_missing"))
    XCTAssertNil(TextProblem.required("general", missing: "channel_name_missing"))
  }

  func testAKeywordIsNeitherBlankNorADuplicate() {
    XCTAssertEqual(keywordProblem("  ", existing: []), .missing("rules_keyword_missing"))
    XCTAssertEqual(
      keywordProblem("Deploy", existing: ["deploy"]), .duplicate("rules_keyword_duplicate"))
    XCTAssertNil(keywordProblem("release", existing: ["deploy"]))
  }
}
