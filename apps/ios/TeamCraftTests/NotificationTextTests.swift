import XCTest

@testable import TeamCraft

final class NotificationTextTests: XCTestCase {
  private func text(
    conversation: String? = "General", sender: String? = "Alex", body: String? = "Hi",
    file: String? = nil, kind: NotificationAttachmentKind? = nil, isDirect: Bool = false,
    namesSender: Bool = true
  ) -> NotificationText {
    NotificationText(
      maybeConversationName: conversation, maybeSenderName: sender, maybeText: body, maybeFileName: file,
      maybeAttachmentKind: kind, isDirect: isDirect, fallbackSender: "TeamCraft", fallbackText: "New message",
      fileText: { "📎 \($0)" },
      kindText: { kind in
        switch kind {
        case .image: "📷 Photo"
        case .video: "🎬 Video"
        case .audio: "🎤 Audio"
        case .file: ""
        }
      },
      fromSenderText: namesSender ? { "New message from \($0)" } : nil)
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
    XCTAssertEqual(text(body: nil, file: "plan.pdf").text, "📎 plan.pdf")
    XCTAssertEqual(text(body: nil, file: "plan.pdf", kind: .file).text, "📎 plan.pdf")
    XCTAssertEqual(text(body: "  ", namesSender: false).text, "New message")
    XCTAssertEqual(text(conversation: nil, sender: nil, isDirect: true).title, "TeamCraft")
  }

  func testMediaMessagesShowALabelNotTheFileName() {
    XCTAssertEqual(text(body: nil, file: "IMG_0042.jpg", kind: .image).text, "📷 Photo")
    XCTAssertEqual(text(body: nil, file: "clip.mp4", kind: .video).text, "🎬 Video")
    XCTAssertEqual(text(body: nil, file: "voice.m4a", kind: .audio).text, "🎤 Audio")
  }

  func testACaptionWinsOverTheLabel() {
    XCTAssertEqual(text(body: "Look!", file: "IMG_0042.jpg", kind: .image).text, "Look!")
  }

  func testWhenOnlyTheSenderIsKnownTheTextNamesThem() {
    XCTAssertEqual(text(body: nil).text, "New message from Alex")
    XCTAssertEqual(text(conversation: "Alex", sender: nil, body: nil, isDirect: true).text, "New message")
  }
}
