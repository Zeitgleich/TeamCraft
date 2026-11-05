import XCTest

@testable import TeamCraft

final class ShareInboxTests: XCTestCase {
  private var root: URL!
  private var inbox: ShareInbox!

  override func setUpWithError() throws {
    root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    inbox = ShareInbox(root: root)
  }

  override func tearDownWithError() throws {
    try? FileManager.default.removeItem(at: root)
  }

  func testACommittedShareIsFoundAgainAndItsFileKeepsItsName() throws {
    var share = try inbox.begin()
    share.text = "hello"
    try inbox.add(file: try sourceFile(), named: "report.pdf", to: &share)
    XCTAssertNil(inbox.oldest(), "not before the list is written")
    try inbox.commit(share)

    let found = ShareInbox(root: root).oldest()
    XCTAssertEqual(found, share)
    let stored = inbox.url(of: share.files[0], in: share)
    XCTAssertEqual(stored.lastPathComponent, "report.pdf")
    XCTAssertTrue(FileManager.default.fileExists(atPath: stored.path))
  }

  func testASendersNameStaysInsideTheShare() throws {
    var share = try inbox.begin()
    try inbox.add(file: try sourceFile(), named: "../../evil.txt", to: &share)
    let stored = inbox.url(of: share.files[0], in: share).standardizedFileURL.path
    XCTAssertTrue(stored.hasPrefix(root.appendingPathComponent(share.id).standardizedFileURL.path))
    XCTAssertEqual((stored as NSString).lastPathComponent, "evil.txt")
  }

  func testNamesLoseControlAndDirectionCharacters() {
    XCTAssertEqual(ShareInbox.cleanName("invoice\u{202E}fdp.exe"), "invoicefdp.exe")
    XCTAssertEqual(ShareInbox.cleanName(" a\n.txt "), "a.txt")
    for hollow in ["", "  ", "..", "/", "\u{202E}"] {
      XCTAssertEqual(ShareInbox.cleanName(hollow), "file")
    }
  }

  func testTheOldestComesFirstAndHandledOnesGo() throws {
    let first = try inbox.begin(now: Date(timeIntervalSince1970: 1000))
    let second = try inbox.begin(now: Date(timeIntervalSince1970: 2000))
    try inbox.commit(second)
    try inbox.commit(first)

    XCTAssertEqual(inbox.oldest()?.id, first.id)
    XCTAssertEqual(inbox.oldest(except: [first.id])?.id, second.id)
    inbox.delete(first.id)
    XCTAssertEqual(inbox.oldest()?.id, second.id)
    inbox.wipe()
    XCTAssertNil(inbox.oldest())
  }

  func testPruningDropsOldSharesAndCopiesCutShort() throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let old = try inbox.begin(now: now.addingTimeInterval(-8 * 24 * 3600))
    try inbox.commit(old)
    let recent = try inbox.begin(now: now.addingTimeInterval(-24 * 3600))
    try inbox.commit(recent)
    let cutShort = try inbox.begin(now: now.addingTimeInterval(-2 * 3600))
    let copying = try inbox.begin(now: now.addingTimeInterval(-60))

    inbox.prune(now: now)

    let left = try FileManager.default.contentsOfDirectory(atPath: root.path).sorted()
    XCTAssertEqual(left, [recent.id, copying.id].sorted())
    XCTAssertFalse(left.contains(old.id))
    XCTAssertFalse(left.contains(cutShort.id))
  }

  func testAShareStaysForTheSamePersonAndGoesForAnother() throws {
    XCTAssertFalse(inbox.claim(for: "1"), "nobody signed in before")
    let share = try inbox.begin()
    try inbox.commit(share)
    XCTAssertFalse(inbox.claim(for: "1"))
    XCTAssertNotNil(inbox.oldest())

    XCTAssertTrue(inbox.claim(for: "2"))
    XCTAssertNil(inbox.oldest())
    XCTAssertFalse(inbox.claim(for: "3"), "nothing left to lose")
  }

  func testAfterASignOutNobodyOwnsWhatIsSharedNext() throws {
    inbox.claim(for: "1")
    inbox.wipe()
    let share = try inbox.begin()
    try inbox.commit(share)
    XCTAssertFalse(inbox.claim(for: "2"))
    XCTAssertNotNil(inbox.oldest())
  }

  private func sourceFile() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try Data("content".utf8).write(to: url)
    addTeardownBlock { try? FileManager.default.removeItem(at: url) }
    return url
  }
}
