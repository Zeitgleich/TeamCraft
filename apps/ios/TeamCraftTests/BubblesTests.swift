import SwiftUI
import XCTest

@testable import TeamCraft

final class BubblesTests: XCTestCase {
  func testCornersJoinOnTheSendersSide() {
    let own = BubbleCorners(isMine: true, isContinuation: true, isGroupEnd: false)
    XCTAssertEqual(own.topTrailing, bubbleJoinedCornerRadius)
    XCTAssertEqual(own.bottomTrailing, bubbleJoinedCornerRadius)
    XCTAssertEqual(own.topLeading, bubbleCornerRadius)
    XCTAssertEqual(own.bottomLeading, bubbleCornerRadius)
    let other = BubbleCorners(isMine: false, isContinuation: false, isGroupEnd: false)
    XCTAssertEqual(other.topLeading, bubbleCornerRadius)
    XCTAssertEqual(other.bottomLeading, bubbleJoinedCornerRadius)
    XCTAssertEqual(other.topTrailing, bubbleCornerRadius)
    let single = BubbleCorners(isMine: false, isContinuation: false, isGroupEnd: true)
    XCTAssertEqual(single.bottomLeading, bubbleCornerRadius)
  }

  func testSendersOnlyOutsideOneToOneChats() {
    XCTAssertFalse(showsSenders(.direct))
    XCTAssertTrue(showsSenders(.group))
    XCTAssertTrue(showsSenders(.public))
  }

  func testLongLinksAreShortened() {
    XCTAssertEqual(shortLinkLabel("https://www.example.com/"), "example.com")
    XCTAssertEqual(
      shortLinkLabel("https://example.com/projects/project-a/reports/2026/quarterly-report.pdf?download=1"),
      "example.com/…/quarterly-report.pdf")
    let label = shortLinkLabel("https://example.com/" + String(repeating: "x", count: 80))
    XCTAssertEqual(label.count, maxLinkLabelLength)
    XCTAssertTrue(label.hasSuffix("…"))
  }

  func testFileNamesAloneAreNoCaption() {
    XCTAssertTrue(isOnlyFileLabel("a.png b.pdf", files: [file("a.png"), file("b.pdf")]))
    XCTAssertFalse(isOnlyFileLabel("Look at this", files: [file("a.png")]))
    XCTAssertFalse(isOnlyFileLabel("a.png", files: []))
  }

  func testAPersonKeepsTheirColour() {
    XCTAssertEqual(personHue("1234"), personHue("1234"))
    XCTAssertTrue((0..<1).contains(personHue("98765")))
  }

  func testMarkupStylesRunsAndShortensBareLinks() {
    let line = MarkupLine(runs: [
      run(.text, "Bold", isBold: true),
      run(.code, "code"),
      run(.link(url: "https://example.com/a/very/long/path/to/some/document-name.html", isLabeled: false), "x"),
      run(.link(url: "javascript:alert(1)", isLabeled: true), "label"),
      run(.mention(name: "alex"), "@alex"),
    ])
    let text = markupText([line], colors: colors, names: MentionNames(usernames: ["alex"], myUsername: "me"), permalinkLabel: "Link")
    let plain = String(text.characters)
    XCTAssertEqual(plain, "Bold code example.com/…/document-name.htmllabel@alex")
    XCTAssertEqual(text.runs.compactMap(\.link).map(\.absoluteString), ["https://example.com/a/very/long/path/to/some/document-name.html"])
    let strong = text.runs.filter { $0.inlinePresentationIntent?.contains(.stronglyEmphasized) == true }
      .map { String(text[$0.range].characters) }
    XCTAssertEqual(strong, ["Bold", "@alex"])
  }

  func testPermalinksShowTheirLabel() {
    let line = MarkupLine(runs: [run(.link(url: "teamcraft:/c/12/m/34", isLabeled: false), "teamcraft:/c/12/m/34")])
    let text = markupText([line], colors: colors, names: MentionNames(), permalinkLabel: "Link to a message")
    XCTAssertEqual(String(text.characters), "Link to a message")
    XCTAssertEqual(text.runs.compactMap(\.link).map(\.absoluteString), ["teamcraft:/c/12/m/34"])
  }

  func testTheCoreParsesTheBody() {
    let blocks = messageMarkup(body: "Use `cargo test`\n\n- one\n- two")
    XCTAssertEqual(blocks.count, 2)
    guard case .paragraph(let lines) = blocks[0], case .items(.bulleted, let items) = blocks[1] else {
      return XCTFail("unexpected blocks: \(blocks)")
    }
    XCTAssertTrue(lines[0].runs.contains { $0.kind == .code && $0.text == "cargo test" })
    XCTAssertEqual(items.count, 2)
  }

  private var colors: MarkupColors {
    MarkupColors(link: .blue, mention: .blue, mentionMe: .orange, mentionMeBackground: .yellow, codeBackground: .gray)
  }

  private func run(_ kind: MarkupRunKind, _ text: String, isBold: Bool = false) -> MarkupRun {
    MarkupRun(kind: kind, text: text, isBold: isBold, isItalic: false, isUnderline: false, isStrike: false)
  }

  private func file(_ name: String) -> FileInfo {
    FileInfo(
      conversation: "c", message: "m", fileId: name, fileName: name, mediaType: "application/octet-stream",
      sizeBytes: 1, maybeThumbnailPng: nil,
      transfer: FileTransfer(state: "remote", doneBytes: nil, totalBytes: nil, failedAttempts: nil, path: nil, reason: nil))
  }
}
