import XCTest

@testable import TeamCraft

/// The feed and threads read the core's `Message` JSON (issue #1623): the mirror decodes what the
/// core writes and names the authors as the typed records did.
final class MessageWireTests: XCTestCase {
  private let deletedName = String(localized: "user_deleted_name")

  func testAFeedPageDecodesAndNamesItsAuthors() throws {
    let json = """
      {
        "conversation": "5", "has_older": true, "undecrypted": 0,
        "gaps": [{"after": "10", "index": 1, "status": "loading"}],
        "maybe_me": {"id": "1", "username": "me", "display_name": "Me Myself", "presence": "Online",
          "maybe_custom_status": null, "avatar_hue": 3, "is_guest": false, "is_bot": false},
        "authors": [
          {"id": "2", "username": "sam", "display_name": "Sam Doe", "presence": "Offline",
            "maybe_custom_status": null, "avatar_hue": 1, "is_guest": false, "is_bot": false},
          {"id": "3", "username": "", "display_name": "Tombstone", "presence": "Offline",
            "maybe_custom_status": null, "avatar_hue": 1, "is_guest": false, "is_bot": false, "is_deleted": true}
        ],
        "messages": [
          {"id": "10", "conversation": "5", "author": "2", "kind": "User", "sent_at": 1000, "body": "Hi @accounting",
            "is_edited": true, "reactions": [{"emoji": "👍", "count": 2, "reacted_by_me": true}], "reply_count": 3,
            "maybe_thread": null, "group_mentions": ["accounting"], "mentions_me": true},
          {"id": "11", "conversation": "5", "author": "1", "kind": {"System": "Joined"}, "sent_at": 2000, "body": "",
            "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": "10"},
          {"id": "12", "conversation": "5", "author": "3", "kind": {"System": {"Renamed": {"name": "Project A"}}},
            "sent_at": 3000, "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null},
          {"id": "\(Int64(1) << 62 | 7)", "conversation": "5", "author": "1", "kind": "User", "sent_at": 4000,
            "body": "on its way", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null}
        ]
      }
      """
    let page = try XCTUnwrap(Wire.decode(MessageTail.self, from: json)).page
    XCTAssertTrue(page.hasOlder)
    XCTAssertEqual(page.gaps, [HistoryGap(after: "10", index: 1, status: .loading)])
    XCTAssertEqual(page.messages.map(\.id).count, 4)

    let written = page.messages[0]
    XCTAssertEqual(written.authorName, "Sam Doe")
    XCTAssertEqual(written.conversationId, "5")
    XCTAssertEqual(written.sentAtMs, 1000)
    XCTAssertTrue(written.isEdited)
    XCTAssertEqual(written.replyCount, 3)
    XCTAssertEqual(written.reactions, [Reaction(emoji: "👍", count: 2, reactedByMe: true)])
    XCTAssertEqual(written.groupMentions, ["accounting"])
    XCTAssertTrue(written.mentionsMe)
    XCTAssertNil(written.maybeSystemEvent)
    XCTAssertFalse(written.isPending)

    XCTAssertEqual(page.messages[1].authorName, "Me Myself")
    XCTAssertEqual(page.messages[1].maybeSystemEvent, .joined)
    XCTAssertEqual(page.messages[1].maybeThreadId, "10")
    XCTAssertEqual(page.messages[2].maybeSystemEvent, .renamed(name: "Project A"))
    XCTAssertEqual(page.messages[2].authorName, deletedName)
    XCTAssertTrue(page.messages[2].isAuthorDeleted)
    XCTAssertTrue(page.messages[3].isPending)
  }

  /// docs/notifications.md, "Calls in the timeline".
  func testWorkspaceEmojiDecodeInTextAndReactions() throws {
    let hash = String(repeating: "a", count: 64)
    let json = """
      {"id": "10", "conversation": "5", "author": "2", "kind": "User", "sent_at": 1000, "body": "ship :ship_it:",
        "is_edited": false, "reply_count": 0,
        "custom_emoji": [{"shortcode": "ship_it", "sha256": "\(hash)"}],
        "reactions": [
          {"emoji": ":ship_it:", "count": 1, "reacted_by_me": false, "custom": {"shortcode": "ship_it", "sha256": "\(hash)"}},
          {"emoji": "👍", "count": 2, "reacted_by_me": true}
        ]}
      """
    let message = try XCTUnwrap(Wire.decode(Message.self, from: json))
    let reference = CustomEmojiRef(shortcode: "ship_it", sha256: hash)
    XCTAssertEqual(message.customEmoji, [reference])
    XCTAssertEqual(message.reactions[0].maybeCustom, reference)
    XCTAssertNil(message.reactions[1].maybeCustom)
  }

  func testAnOlderCoreSendsNoWorkspaceEmoji() throws {
    let json = """
      {"id": "10", "conversation": "5", "author": "2", "kind": "User", "sent_at": 1000, "body": "Hi",
        "is_edited": false, "reply_count": 0, "reactions": []}
      """
    let message = try XCTUnwrap(Wire.decode(Message.self, from: json))
    XCTAssertTrue(message.customEmoji.isEmpty)
  }

  func testCallTilesDecodeFromTheMessageKind() throws {
    let json = """
      {
        "conversation": "5", "has_older": false, "undecrypted": 0, "gaps": [], "authors": [],
        "messages": [
          {"id": "20", "conversation": "5", "author": "2", "kind": {"Call": {"call": "20", "state": {"state": "ongoing"}}},
            "sent_at": 1000, "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null},
          {"id": "21", "conversation": "5", "author": "2",
            "kind": {"Call": {"call": "21", "state": {"state": "ended", "duration_ms": 185000}, "is_missed": true}},
            "sent_at": 2000, "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null},
          {"id": "22", "conversation": "5", "author": "2", "kind": {"Call": {"call": "22", "state": {"state": "ended"}}},
            "sent_at": 3000, "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null}
        ]
      }
      """
    let messages = try XCTUnwrap(Wire.decode(MessageTail.self, from: json)).page.messages
    XCTAssertEqual(messages[0].maybeCall, CallTile(call: "20", state: .ongoing))
    XCTAssertEqual(
      messages[1].maybeCall, CallTile(call: "21", state: .ended(maybeDurationMs: 185_000), isMissed: true))
    XCTAssertEqual(messages[2].maybeCall?.state, .ended(maybeDurationMs: nil))
    XCTAssertNil(messages[0].maybeSystemEvent)
    XCTAssertFalse(MessageSelection.isSelectable(messages[0]))
    XCTAssertEqual(callDuration(185_000), "3:05")
    XCTAssertEqual(callDuration(3_723_000), "1:02:03")
  }

  func testPlaceholdersSourcesAndImportedAuthors() throws {
    let json = """
      {"answer": "messages", "messages": [
        {"id": "1", "conversation": "5", "author": "9", "kind": "User", "sent_at": 1, "body": "", "is_edited": false,
          "reactions": [], "reply_count": 0, "maybe_thread": null, "placeholder": "not_available"},
        {"id": "2", "conversation": "5", "author": "9", "kind": "User", "sent_at": 2, "body": "", "is_edited": false,
          "reactions": [], "reply_count": 0, "maybe_thread": null, "placeholder": "some_newer_state"},
        {"id": "3", "conversation": "5", "author": "8", "kind": {"Log": "Info"}, "sent_at": 3, "body": "build green",
          "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null,
          "source": {"name": "CI", "picture_sha256": "ab12"}},
        {"id": "4", "conversation": "5", "author": "7", "kind": "Change", "sent_at": 4, "body": "old", "is_edited": false,
          "reactions": [], "reply_count": 0, "maybe_thread": null,
          "imported": {"source": "synology_chat", "author": {"kind": "named", "name": "Old Bot"}, "importer": "6"}},
        {"id": "5", "conversation": "5", "author": "7", "kind": "User", "sent_at": 5, "body": "older", "is_edited": false,
          "reactions": [], "reply_count": 0, "maybe_thread": null,
          "imported": {"source": "synology_chat", "author": {"kind": "deleted"}, "importer": "6"}},
        {"id": "6", "conversation": "5", "author": "9", "kind": {"System": {"TopicChanged": {"topic": "Launch"}}},
          "sent_at": 6, "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null}
      ]}
      """
    let read = try XCTUnwrap(Wire.decode(ChatAnswer.self, from: json)?.messages)
    let authors = ["8": MessageAuthor(id: "8", displayName: "CI Bot"), "9": MessageAuthor(id: "9", displayName: "Ada")]
    let messages = read.map { $0.named(authors) }

    XCTAssertEqual(messages.map(\.maybePlaceholder), [.notAvailable, .notAvailable, nil, nil, nil, nil])
    XCTAssertEqual(messages[0].authorName, "Ada")
    XCTAssertTrue(messages[2].isAuthorSource)
    XCTAssertEqual(messages[2].authorName, "CI")
    XCTAssertEqual(messages[2].maybeSourcePictureSha256, "ab12")
    XCTAssertNil(messages[2].maybeSystemEvent, "a log line is no system event")
    XCTAssertEqual(messages[3].authorName, "Old Bot")
    XCTAssertFalse(messages[3].isAuthorDeleted)
    XCTAssertEqual(messages[4].authorName, deletedName, "imported without an account here")
    XCTAssertTrue(messages[4].isAuthorDeleted)
    XCTAssertEqual(messages[5].maybeSystemEvent, .topicChanged(topic: "Launch"))
  }

  func testAnUnknownAuthorStaysUnnamed() {
    let message = Message(id: "1", conversationId: "5", authorId: "42", sentAtMs: 0, body: "Hi")
    XCTAssertEqual(message.named([:]).authorName, "")
  }

  func testAThreadQueryNamesItsThread() {
    XCTAssertEqual(
      ChatQuery.thread(conversationId: "5", threadId: "10").json, #"{"conversation":"5","kind":"thread","thread":"10"}"#)
  }

  /// Issue #1876: the chat list's lines come as the feed's messages with only their authors.
  func testTheChatListLinesNameTheirAuthors() throws {
    let json = """
      {"messages": [
        {"id": "7", "conversation": "5", "author": "4", "kind": {"Log": "Info"}, "sent_at": 9, "body": "", "is_edited": true,
          "reactions": [{"emoji": "🎉", "count": 1, "reacted_by_me": false}], "reply_count": 1, "maybe_thread": null,
          "placeholder": "waiting", "source": {"name": "CI", "picture_sha256": "cd"}, "group_mentions": ["ops"],
          "mentions_me": true},
        {"id": "8", "conversation": "6", "author": "3", "kind": {"System": {"Renamed": {"name": "Ops"}}}, "sent_at": 10,
          "body": "", "is_edited": false, "reactions": [], "reply_count": 0, "maybe_thread": null},
        {"id": "9", "conversation": "7", "author": "2", "kind": "User", "sent_at": 11, "body": "Hi", "is_edited": false,
          "reactions": [], "reply_count": 0, "maybe_thread": null}
      ], "authors": [
        {"id": "3", "username": "", "display_name": "Tombstone", "presence": "Offline", "maybe_custom_status": null,
          "avatar_hue": 1, "is_guest": false, "is_bot": false, "is_deleted": true},
        {"id": "2", "username": "sam", "display_name": "Sam Doe", "presence": "Offline", "maybe_custom_status": null,
          "avatar_hue": 1, "is_guest": false, "is_bot": false}
      ]}
      """
    let messages = try XCTUnwrap(Wire.decode(ChatPreviews.self, from: json)).named
    XCTAssertEqual(messages.map(\.conversationId), ["5", "6", "7"])
    let source = messages[0]
    XCTAssertEqual(source.authorName, "CI")
    XCTAssertTrue(source.isAuthorSource)
    XCTAssertEqual(source.maybeSourcePictureSha256, "cd")
    XCTAssertEqual(source.maybePlaceholder, .waiting)
    XCTAssertEqual(source.reactions, [Reaction(emoji: "🎉", count: 1)])
    XCTAssertEqual(source.groupMentions, ["ops"])
    XCTAssertTrue(source.mentionsMe)
    XCTAssertEqual(messages[1].maybeSystemEvent, .renamed(name: "Ops"))
    XCTAssertEqual(messages[1].authorName, deletedName)
    XCTAssertTrue(messages[1].isAuthorDeleted)
    XCTAssertEqual(messages[2].authorName, "Sam Doe")
  }

  /// Issue #2176: a reaction names who reacted; the viewer is left out of the names, and people
  /// this device doesn't know only count.
  func testAReactionNamesItsReactorsAndNeverShowsAnId() throws {
    let json = """
      {"emoji": "👍", "count": 4, "reacted_by_me": true, "reactors": ["1", "8", "99", "9"]}
      """
    let reaction = try XCTUnwrap(Wire.decode(Reaction.self, from: json))
    XCTAssertEqual(reaction.reactors, ["1", "8", "99", "9"])
    let authors = ["8": MessageAuthor(id: "8", displayName: "Alex Example"), "9": MessageAuthor(id: "9", displayName: "Sam Example")]
    let named = reaction.named(authors)
    XCTAssertEqual(named.otherNames, ["Alex Example", "Sam Example"])
    XCTAssertEqual(named.unnamedCount, 1)
    XCTAssertEqual(Reaction(emoji: "👍", count: 2).unnamedCount, 2)
  }
}
