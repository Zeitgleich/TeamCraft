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
