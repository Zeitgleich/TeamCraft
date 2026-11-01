import XCTest

@testable import TeamCraft

final class CoreFailureTests: XCTestCase {
  func testBackupFailuresAreToldApartFromAWrongPassphrase() {
    // Issue #1067: only a failed unwrap reads "wrong passphrase".
    struct WrongPassphrase: Error {}
    struct BackupOutOfMemory: Error {}
    struct BackupTooNew: Error {}
    struct BackupDamaged: Error {}
    XCTAssertEqual(CoreFailure(WrongPassphrase()), .wrongPassphrase)
    XCTAssertEqual(CoreFailure(BackupOutOfMemory()), .backupOutOfMemory)
    XCTAssertEqual(CoreFailure(BackupTooNew()), .backupTooNew)
    XCTAssertEqual(CoreFailure(BackupDamaged()), .backupDamaged)
    XCTAssertEqual(AppFailure(.backupOutOfMemory).message, "error_backup_out_of_memory")
    XCTAssertEqual(AppFailure(.backupTooNew).message, "error_backup_too_new")
    XCTAssertEqual(AppFailure(.backupDamaged).message, "error_backup_damaged")
  }
}

final class DiagnosticsExportTests: XCTestCase {
  func testOsVersionNamesSystemVersionAndModel() {
    XCTAssertEqual(
      DiagnosticsExport.osVersion(systemName: "iOS", systemVersion: "18.1", model: "iPhone"),
      "iOS 18.1 (iPhone)")
  }

  func testExportTextStartsWithTheVersions() {
    // Issue #1094: the text comes from the core, so both platforms export the same format.
    let text = diagnosticsText(appVersion: "1.2.3 (45)", osVersion: "iOS 18.1 (iPhone)")
    XCTAssertTrue(
      text.hasPrefix("TeamCraft diagnostics\nApp version: 1.2.3 (45)\nOS version: iOS 18.1 (iPhone)\n"))
  }
}
