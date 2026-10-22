import XCTest

@testable import TeamCraft

final class NotificationTextTests: XCTestCase {
  private func text(
    conversation: String? = "General", sender: String? = "Alex", body: String? = "Hi",
    file: String? = nil, isDirect: Bool = false
  ) -> NotificationText {
    NotificationText(
      maybeConversationName: conversation, maybeSenderName: sender, maybeText: body, maybeFileName: file,
      isDirect: isDirect, fallbackSender: "TeamCraft", fallbackText: "New message",
      fileText: { "Sent a file: \($0)" })
  }

  func testChannelMessagesAreTitledByTheConversation() {
    let result = text()
    XCTAssertEqual(result.title, "General")
    XCTAssertEqual(result.maybeSubtitle, "Alex")
    XCTAssertEqual(result.text, "Hi")
  }

  func testDirectMessagesAreTitledBySender() {
    let result = text(conversation: "Alex", isDirect: true)
    XCTAssertEqual(result.title, "Alex")
    XCTAssertNil(result.maybeSubtitle)
  }

  func testFilesAndUnreadableMessagesFallBack() {
    XCTAssertEqual(text(body: nil, file: "plan.pdf").text, "Sent a file: plan.pdf")
    XCTAssertEqual(text(body: "  ").text, "New message")
    XCTAssertEqual(text(conversation: nil, sender: nil, isDirect: true).title, "TeamCraft")
  }
}
