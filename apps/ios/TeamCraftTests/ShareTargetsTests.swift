import XCTest

@testable import TeamCraft

/// The share extension's conversation list and the handover of its pick to the app (issue #1037).
final class ShareTargetsTests: XCTestCase {
  private var root: URL!

  override func setUpWithError() throws {
    root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
  }

  override func tearDownWithError() throws {
    try? FileManager.default.removeItem(at: root)
  }

  // MARK: Snapshot

  func testTheSnapshotKeepsTheNotesAndTheMostRecentInTheCoresOrder() {
    let others = (0..<(ShareTargetsSnapshot.maxRows + 50)).map { row("c\($0)", activity: Int64(10_000 - $0)) }
    let notes = row("n", kind: .notes)
    let snapshot = ShareTargetsSnapshot.make(owner: "alice", rows: [notes] + others)

    XCTAssertEqual(snapshot.rows.count, ShareTargetsSnapshot.maxRows)
    XCTAssertEqual(snapshot.rows.first, notes)
    XCTAssertEqual(Array(snapshot.rows.dropFirst().prefix(3)).map(\.id), ["c0", "c1", "c2"])
  }

  func testTheExtensionShowsTheListOnlyToTheInboxOwner() {
    let snapshot = ShareTargetsSnapshot.make(owner: "alice", rows: [row("a")])
    XCTAssertEqual(snapshot.rows(forInboxOwner: "alice").map(\.id), ["a"])
    XCTAssertTrue(snapshot.rows(forInboxOwner: "bob").isEmpty, "another account signed in meanwhile")
    XCTAssertTrue(snapshot.rows(forInboxOwner: nil).isEmpty, "signed out: nobody owns the inbox")
  }

  func testTheStoreWritesReadsAndDeletes() throws {
    let store = ShareTargetsStore(url: root.appendingPathComponent("share-targets.json"))
    XCTAssertNil(store.read())
    let snapshot = ShareTargetsSnapshot.make(owner: "alice", rows: [row("n", kind: .notes), row("a", activity: 5)])
    try store.write(snapshot)
    XCTAssertEqual(ShareTargetsStore(url: store.maybeURL).read(), snapshot)
    store.delete()
    XCTAssertNil(store.read())
  }

  func testAStoreWithoutTheAppGroupDoesNothing() throws {
    let store = ShareTargetsStore(url: nil)
    try store.write(ShareTargetsSnapshot.make(owner: "alice", rows: [row("a")]))
    XCTAssertNil(store.read())
  }

  // MARK: Picker sections

  func testThePickerShowsNotesAndRecentOnTopAndTheRestByName() {
    let rows = [
      row("n", name: "Notes", kind: .notes),
      row("r1", name: "Zeta", activity: 9),
      row("r2", name: "Alpha", activity: 8),
      row("q", name: "Beta"),
      row("p", name: "Gamma"),
    ]
    let sections = ShareTargetSections(rows: rows, filter: "")
    XCTAssertEqual(sections.top.map(\.id), ["n", "r1", "r2"])
    XCTAssertEqual(sections.others.map(\.id), ["q", "p"])
  }

  func testOnlyTheFiveMostRecentAreOnTop() {
    let rows = (0..<8).map { row("c\($0)", name: "C\($0)", activity: Int64(100 - $0)) }
    let sections = ShareTargetSections(rows: rows, filter: "")
    XCTAssertEqual(sections.top.count, ShareTargetSections.recentCount)
    XCTAssertEqual(sections.others.map(\.id), ["c5", "c6", "c7"])
  }

  func testSearchingFindsByNameAndSpaceOnceByName() {
    let rows = [
      row("a", name: "design", space: "Product", activity: 3),
      row("b", name: "Random", space: "Design team"),
      row("c", name: "General"),
    ]
    let sections = ShareTargetSections(rows: rows, filter: " Design ")
    XCTAssertTrue(sections.top.isEmpty)
    XCTAssertEqual(sections.others.map(\.id), ["a", "b"])
  }

  // MARK: Handover

  func testThePickAndTheCaptionGoWithTheShare() {
    let share = StagedShare(id: "s", text: "https://example.org", files: [], leftOut: 0)
    let picked = ShareHandoff.apply(destination: "42", caption: "  Look at this \n", to: share)
    XCTAssertEqual(picked.maybeConversationId, "42")
    XCTAssertEqual(picked.text, "Look at this\nhttps://example.org")
  }

  func testChoosingInTheAppKeepsTheShareAsItWas() {
    let share = StagedShare(id: "s", text: "hi", files: [], leftOut: 0)
    XCTAssertEqual(ShareHandoff.apply(destination: nil, caption: "   ", to: share), share)
  }

  func testACaptionAloneBecomesTheText() {
    let share = StagedShare(id: "s", text: "", files: [StagedShareFile(storedAs: "0/a.jpg")], leftOut: 0)
    let picked = ShareHandoff.apply(destination: "1", caption: "Holiday", to: share)
    XCTAssertEqual(picked.text, "Holiday")
    XCTAssertEqual(picked.files, share.files)
  }

  func testAVeryLongCaptionIsCut() {
    let share = StagedShare(id: "s", text: "", files: [], leftOut: 0)
    let caption = String(repeating: "x", count: ShareHandoff.maxCaptionLength + 10)
    let cut = ShareHandoff.apply(destination: nil, caption: caption, to: share)
    XCTAssertEqual(cut.text.count, ShareHandoff.maxCaptionLength)
  }

  func testThePickIsCommittedAndReadBack() throws {
    let inbox = ShareInbox(root: root)
    var share = try inbox.begin()
    share.text = "hi"
    try inbox.commit(share)
    XCTAssertNil(inbox.oldest()?.maybeConversationId, "committed without a pick first")

    try inbox.commit(ShareHandoff.apply(destination: "42", caption: "", to: share))
    XCTAssertEqual(ShareInbox(root: root).oldest()?.maybeConversationId, "42")
  }

  func testAShareWrittenBeforeThePickerHasNoConversation() throws {
    let old = #"{"id":"1-x","text":"hi","files":[{"storedAs":"0/a.pdf"}],"leftOut":0}"#
    let share = try JSONDecoder().decode(StagedShare.self, from: Data(old.utf8))
    XCTAssertNil(share.maybeConversationId)
    XCTAssertEqual(share.files, [StagedShareFile(storedAs: "0/a.pdf")])
  }

  func testTheInboxKnowsItsOwner() {
    let inbox = ShareInbox(root: root)
    XCTAssertNil(inbox.owner())
    inbox.claim(for: "alice")
    XCTAssertEqual(inbox.owner(), "alice")
    inbox.wipe()
    XCTAssertNil(inbox.owner(), "after a sign-out nobody owns it, so the extension shows no list")
  }

  // MARK: Routing in the app

  func testAKnownConversationOpensAtOnce() {
    XCTAssertEqual(
      ShareDestinationRoute.decide(destination: "42", targetIds: ["1", "42"], hasWaitedEnough: false), .open("42"))
  }

  func testAGoneConversationGoesToThePicker() {
    XCTAssertEqual(ShareDestinationRoute.decide(destination: "42", targetIds: ["1"], hasWaitedEnough: false), .pick)
  }

  func testNoConversationsYetWaitsAndThenGoesToThePicker() {
    XCTAssertEqual(ShareDestinationRoute.decide(destination: "42", targetIds: [], hasWaitedEnough: false), .wait)
    XCTAssertEqual(ShareDestinationRoute.decide(destination: "42", targetIds: [], hasWaitedEnough: true), .pick)
  }

  // MARK: Helpers

  private func row(
    _ id: String, name: String? = nil, kind: ShareTargetRow.Kind = .channel, space: String? = nil,
    activity: Int64? = nil
  ) -> ShareTargetRow {
    ShareTargetRow(
      conversationId: id, name: name ?? id, kind: kind, maybeSpaceName: space, maybeLastActivityMs: activity)
  }
}
