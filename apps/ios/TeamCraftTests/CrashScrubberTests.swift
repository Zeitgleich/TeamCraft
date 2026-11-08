import XCTest

@testable import TeamCraft

/// The crash report scrubber (docs/crash-reports.md, "Scrubbing"): the same cases as the core's
/// `scrub.rs` and the Android app's `CrashScrubberTest`.
final class CrashScrubberTests: XCTestCase {
  /// What a session with secret content could leave behind; none of it may reach a report.
  private let secrets = [
    "meet me at the harbour at nine",
    "Alice Example",
    "quarterly-figures.xlsx",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
    "s3cr3t-pa55word",
    "X-Amz-Signature=",
    "Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ",
    "alice@example.com",
    "henning",
    "share-link-secret",
  ]

  /// Texts an exception or a log line could carry in such a session.
  private let leakyTexts = [
    #"Failed to send "meet me at the harbour at nine" to Person { display_name: "Alice Example" }"#,
    "cannot open /var/mobile/Containers/Data/Application/A1/Documents/quarterly-figures.xlsx",
    "auth failed: Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
    "sign-in refused password=s3cr3t-pa55word for alice@example.com",
    "upload to https://cdn.example.com/upload?X-Amz-Signature=abc123&X-Amz-Credential=x failed",
    "key material Q2hhbm5lbCBrZXkgbWF0ZXJpYWwgaGVyZQ rejected",
    "config at /Users/henning/Library/teamcraft/app.toml unreadable",
    "share link https://team.example.com/s/1#share-link-secret expired",
  ]

  func testAReportFromASessionWithSecretsContainsNoneOfThem() {
    let scrubbed = leakyTexts.map { CrashScrubber.scrub($0) }.joined(separator: "\n")
    for secret in secrets {
      XCTAssertFalse(scrubbed.contains(secret), "\(secret) leaked into \(scrubbed)")
    }
    // What helps fixing it is still there.
    XCTAssertTrue(scrubbed.contains("Failed to send"))
    XCTAssertTrue(scrubbed.contains("https://cdn.example.com/upload?[redacted]"))
  }

  func testQuotedValuesGoWhole() {
    XCTAssertEqual(
      CrashScrubber.removeQuoted(#"Person { display_name: "Alice Example", note: "say \"hi\"" }"#),
      #"Person { display_name: "[redacted]", note: "[redacted]" }"#)
    XCTAssertEqual(CrashScrubber.removeQuoted(#"open "half"#), #"open "[redacted]"#)
    XCTAssertEqual(CrashScrubber.removeQuoted("no quotes here"), "no quotes here")
  }

  func testIdsRoutesAndWordsStay() {
    let line = "sync failed conversation_id=1234 at /api/v1/conversations/7346/messages: timeout"
    XCTAssertEqual(CrashScrubber.scrub(line), line)
  }

  func testURLsKeepHostAndPathOnly() {
    XCTAssertEqual(
      CrashScrubber.redact("GET https://user:pw@team.example.com/api/v1/x?token=abc#frag."),
      "GET https://[redacted]@team.example.com/api/v1/x?[redacted].")
    XCTAssertEqual(CrashScrubber.redact("open file:///private/var/a.pdf"), "open [path]")
  }

  func testLongTextsAreCut() {
    let scrubbed = CrashScrubber.scrub(String(repeating: "x", count: 2_000))
    XCTAssertEqual(scrubbed.count, CrashScrubber.maxMessageChars + 1)
    XCTAssertTrue(scrubbed.hasSuffix("…"))
  }

  func testTheUserDecidesUnlessReportsAreMandatory() {
    let optional = CrashConfig(
      dsn: "https://key@errors.example.com/1", sampleRate: 0.1, environment: "production", allowsOptOut: true,
      showsNotice: true)
    let mandatory = CrashConfig(
      dsn: optional.dsn, sampleRate: 0.1, environment: "production", allowsOptOut: false, showsNotice: true)
    XCTAssertTrue(CrashReports.isActive(config: optional, isAllowedByUser: true))
    XCTAssertFalse(CrashReports.isActive(config: optional, isAllowedByUser: false))
    XCTAssertTrue(CrashReports.isActive(config: mandatory, isAllowedByUser: false))
  }

  func testInstallationIDsAreRandomHex() {
    let first = CrashReports.newInstallationID()
    XCTAssertEqual(first.count, 32)
    XCTAssertTrue(first.allSatisfy(\.isHexDigit))
    XCTAssertNotEqual(first, CrashReports.newInstallationID())
  }
}
