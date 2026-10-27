import XCTest

@testable import TeamCraft

/// The lines system messages show (#769) and the read state of own messages.
final class SystemLineTests: XCTestCase {
  private let names = ["1": "Ada", "2": "Sam"]

  func testSystemEventsNameTheirAuthor() {
    XCTAssertEqual(line(event: .joined), .joined(name: "Sam"))
    XCTAssertEqual(line(event: .left), .left(name: "Sam"))
    XCTAssertEqual(line(event: .renamed(name: "Project A")), .renamed(name: "Sam", channel: "Project A"))
    XCTAssertEqual(line(event: .topicChanged(topic: "Launch")), .topicChanged(name: "Sam", topic: "Launch"))
  }

  func testWhoAddedWhomWinsOverThePlainJoin() {
    // The core reports an addition as the added person's join, with the change in the extras.
    let added = line(event: .joined, extras: #"{"event":"member_added","user":"2","by":"1","history":"full"}"#)
    XCTAssertEqual(added, .memberAdded(name: "Sam", by: "Ada", sharesHistory: true))
    let removed = line(event: .left, extras: #"{"event":"member_removed","user":"2","by":"1"}"#)
    XCTAssertEqual(removed, .memberRemoved(name: "Sam", by: "Ada"))
  }

  func testChannelChangesNameWhoMadeThem() {
    XCTAssertEqual(line(extras: #"{"event":"archived","by":"1"}"#), .archived(maybeBy: "Ada"))
    XCTAssertEqual(line(extras: #"{"event":"made_public","by":"1"}"#), .madePublic(maybeBy: "Ada"))
    XCTAssertEqual(line(extras: #"{"event":"moved","by":"1","space":""}"#), .moved(maybeBy: "Ada", space: ""))
    // A person this device doesn't know yet: the author is who made the change.
    XCTAssertEqual(line(extras: #"{"event":"pinned","by":"2"}"#, names: [:]), .pinned(by: "Sam"))
  }

  /// Issue #818: no member's announcement matched the change the server reports, so no `by`.
  func testChannelChangesNobodyIsNamedForAreTheServersWord() {
    XCTAssertEqual(line(extras: #"{"event":"archived"}"#), .archived(maybeBy: nil))
    XCTAssertEqual(line(extras: #"{"event":"unarchived"}"#), .unarchived(maybeBy: nil))
    XCTAssertEqual(line(extras: #"{"event":"made_private"}"#), .madePrivate(maybeBy: nil))
    XCTAssertEqual(line(extras: #"{"event":"made_public"}"#), .madePublic(maybeBy: nil))
    XCTAssertEqual(
      line(extras: #"{"event":"moved","space":"Project B"}"#), .moved(maybeBy: nil, space: "Project B"))
  }

  func testUnknownEventsFallBackToAQuietLine() {
    XCTAssertEqual(line(event: .joined, extras: #"{"event":"split_in_two","by":"1"}"#), .joined(name: "Sam"))
    XCTAssertEqual(line(extras: #"{"event":"split_in_two","by":"1"}"#), .changed)
    XCTAssertEqual(line(), .changed)
  }

  func testWrittenAndDeletedMessagesAreBubbles() {
    XCTAssertNil(line(body: "Hello"))
    XCTAssertNil(SystemLine(message: message(body: ""), maybeExtras: extras(#""is_deleted":true"#), names: names))
  }

  /// In the language the simulator runs in: the names land in their places in either catalog.
  func testLinesInTheAppsLanguage() {
    let isGerman = Bundle.main.preferredLocalizations.first == "de"
    XCTAssertEqual(
      SystemLine.renamed(name: "Sam", channel: "Project A").text,
      isGerman ? "Sam hat den Kanal in Project A umbenannt." : "Sam renamed the channel to Project A.")
    XCTAssertEqual(
      SystemLine.memberAdded(name: "Sam", by: "Ada", sharesHistory: true).text,
      isGerman
        ? "Ada hat Sam hinzugefügt und den bisherigen Verlauf geteilt"
        : "Ada added Sam and shared the earlier history")
    XCTAssertEqual(
      SystemLine.moved(maybeBy: "Ada", space: "").text,
      isGerman ? "Ada hat diesen Kanal in einen anderen Bereich verschoben" : "Ada moved this channel to another space")
    XCTAssertEqual(
      SystemLine.archived(maybeBy: nil).text,
      isGerman ? "Dieser Kanal wurde archiviert" : "This channel was archived")
    XCTAssertEqual(
      SystemLine.moved(maybeBy: nil, space: "Project B").text,
      isGerman ? "Dieser Kanal wurde in Project B verschoben" : "This channel was moved to Project B")
    XCTAssertEqual(SystemLine.changed.text, isGerman ? "Unterhaltung geändert" : "Conversation updated")
  }

  func testWorkspaceEventsAreReadLeniently() {
    XCTAssertEqual(extras(#""maybe_workspace_event":{"event":"member_added"}"#)?.maybeWorkspaceEvent,
      .memberAdded(user: "", by: "", sharesHistory: false))
    XCTAssertEqual(extras(#""maybe_workspace_event":{"event":"later","by":7}"#)?.maybeWorkspaceEvent, .unknown)
  }

  func testReadStateAndItsRefresh() throws {
    XCTAssertEqual(extras(#""is_read":true"#)?.isRead, true)
    XCTAssertNil(extras("")?.isRead, "older cores don't say: one tick")
    let refresh = try JSONSerialization.jsonObject(
      with: Data(ChatAction.refreshReadReceipts(conversationId: "1").json.utf8)) as? [String: String]
    XCTAssertEqual(refresh, ["action": "refresh_read_receipts", "conversation": "1"])
  }

  func testAnAdminWhoJoinedSaysWhoApprovedTheRequest() {
    // admin.md §2.4: by themselves (`visible`), or let in by an owner (`owner_approval`, #799).
    XCTAssertEqual(
      line(event: .joined, extras: #"{"event":"admin_joined","user":"2"}"#),
      .adminJoined(name: "Sam", maybeApprovedBy: nil))
    XCTAssertEqual(
      line(extras: #"{"event":"admin_joined","user":"2","approved_by":"1"}"#),
      .adminJoined(name: "Sam", maybeApprovedBy: "Ada"))
    // Added without an approval to ask for: "added by", not "approved by" (#811).
    XCTAssertEqual(
      line(extras: #"{"event":"admin_joined","user":"2","added_by":"1"}"#),
      .adminAdded(name: "Sam", by: "Ada"))
  }

  private func line(
    event: SystemEventItem? = nil, extras workspace: String? = nil, body: String = "", names: [String: String]? = nil
  ) -> SystemLine? {
    let maybeExtras = workspace.map { extras(#""maybe_workspace_event":"# + $0) } ?? nil
    return SystemLine(message: message(body: body, event: event), maybeExtras: maybeExtras, names: names ?? self.names)
  }

  private func message(body: String, event: SystemEventItem? = nil) -> MessageItem {
    MessageItem(
      id: "9", conversationId: "c", authorId: "2", authorName: "Sam", sentAtMs: 0, body: body, isEdited: false,
      isPending: false, replyCount: 0, maybeThreadId: nil, reactions: [], maybeSystemEvent: event)
  }

  /// Extras with `fields` (JSON members after the message ID).
  private func extras(_ fields: String) -> MessageExtras? {
    Wire.decode(MessageExtras.self, from: #"{"message":"9""# + (fields.isEmpty ? "" : ",\(fields)") + "}")
  }
}
