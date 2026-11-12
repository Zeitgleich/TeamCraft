#if DEBUG
  import Foundation

  /// Sample chats for screenshots of the chat list without a server or an account (#1786): a Debug
  /// build started with `-chat-list-demo` shows the chats screen with these instead of the core's.
  /// Other launch arguments set stored settings the same way, for example `-chat_list_layout list`.
  /// Release builds contain none of this.
  enum ChatListDemo {
    static let isOn = ProcessInfo.processInfo.arguments.contains("-chat-list-demo")

    /// The tab the screen starts on: `-chat_list_demo_tab channels` (chats, channels, meetings,
    /// activity).
    static var initialTab: ChatTab? {
      switch UserDefaults.standard.string(forKey: "chat_list_demo_tab") {
      case "channels": return .channels
      case "meetings": return .meetings
      case "activity": return .activity
      default: return nil
      }
    }

    /// What the search field starts with: `-chat_list_demo_query des`.
    static var initialQuery: String? { UserDefaults.standard.string(forKey: "chat_list_demo_query") }

    static let account = Account(userId: "demo-me", displayName: "Jana Weber", identityFingerprint: "")

    static let spaces = [
      HomeSpace(id: "space-product", name: "Product"), HomeSpace(id: "space-ops", name: "Operations"),
    ]

    static let runningCalls = ["dm-tom": 2]

    static let conversations: [ConversationItem] = [
      direct("dm-notes", "", favorite: true, notes: true),
      direct("dm-lena", "Lena Hoffmann", presence: .online, favorite: true),
      direct("dm-max", "Max Schulz", presence: .busy, unread: 2),
      group("grp-launch", "Aylin Demir, Paul Wagner", unread: 1, mentions: 1),
      direct("dm-tom", "Tom Becker", presence: .away),
      direct("dm-sofia", "Sofia Rossi", presence: .doNotDisturb),
      direct("dm-deploy", "Deploy Bot", presence: .online, bot: true),
      group("grp-lunch", "Jonas Klein, Mia Braun, Lukas Vogel", muted: true),
      direct("dm-emre", "Emre Yilmaz", presence: .offline),
      direct("dm-clara", "Clara Fischer", presence: .beRightBack),
      direct("dm-noah", "Noah Weber", presence: .offline),
      channel("ch-general", "general", space: spaces[0], unread: 4),
      channel("ch-design", "design-review", space: spaces[0], mentions: 1, favorite: true),
      channel("ch-release", "release", space: spaces[0], kind: .private),
      channel("ch-standup", "standup", space: spaces[0]),
      channel("ch-hooks", "ci-alerts", space: spaces[0], kind: .integration, unread: 12, muted: true),
      channel("ch-ops", "on-call", space: spaces[1], unread: 1),
    ]

    /// The newest messages, minutes and days before `now`.
    static func previews(now: Date = .now) -> [String: ChatPreview] {
      let lines: [(String, ChatPreview.Sender, String, TimeInterval)] = [
        ("dm-notes", .me, "IMG_3760.png", 60 * 9),
        ("dm-lena", .none, "See you at the review, I'll bring the prints", 60 * 42),
        ("dm-max", .none, "Can you look at the draft before 3? https://example.com/drafts/q4", 60 * 3),
        ("grp-launch", .other("Aylin"), "@Jana the launch checklist is ready", 60 * 18),
        ("dm-tom", .me, "Joining the call in a minute", 60 * 75),
        ("dm-sofia", .none, "Thanks, that works for me", 60 * 60 * 5),
        ("dm-deploy", .none, "Deployed 1.0.32 to staging", 60 * 60 * 26),
        ("grp-lunch", .other("Mia"), "Pizza on Friday?", 60 * 60 * 30),
        ("dm-emre", .none, "The invoice is attached", 60 * 60 * 24 * 3),
        ("dm-clara", .me, "Great, talk tomorrow", 60 * 60 * 24 * 4),
        ("dm-noah", .none, "Happy birthday!", 60 * 60 * 24 * 12),
        ("ch-general", .other("Paul"), "Office is closed on Monday", 60 * 12),
        ("ch-design", .other("Lena"), "New icons are in Figma", 60 * 30),
        ("ch-release", .me, "Tagging 1.0.32 now", 60 * 60 * 2),
        ("ch-standup", .other("Tom"), "Notes from today", 60 * 60 * 20),
        ("ch-hooks", .none, "Build #4211 passed", 60 * 5),
        ("ch-ops", .other("Emre"), "Paging is back to normal", 60 * 60),
      ]
      var previews: [String: ChatPreview] = [:]
      for (index, (id, sender, text, age)) in lines.enumerated() {
        previews[id] = ChatPreview(
          sender: sender, content: .text(text), sentDate: now.addingTimeInterval(-age), messageId: "m\(index)")
      }
      return previews
    }

    private static func direct(
      _ id: String, _ name: String, presence: Presence? = nil, favorite: Bool = false, unread: UInt32 = 0,
      bot: Bool = false, notes: Bool = false
    ) -> ConversationItem {
      ConversationItem(
        id: id, maybeSpaceId: nil, maybeSpaceName: nil, kind: .direct, name: name, topic: "", unreadCount: unread,
        mentionCount: 0, isFavorite: favorite, isMuted: false, canPost: true, maybePeerPresence: notes ? nil : presence,
        maybePeerId: notes ? nil : "user-\(id)",
        maybePeer: notes ? nil : PeerInfo(userId: "user-\(id)", isBot: bot, avatarHue: hue(of: name)),
        isNotesToSelf: notes)
    }

    /// The same hue on every start (`hashValue` changes per process).
    private static func hue(of name: String) -> UInt16 {
      UInt16(name.unicodeScalars.reduce(0) { ($0 * 31 + Int($1.value)) % 360 })
    }

    private static func group(
      _ id: String, _ name: String, unread: UInt32 = 0, mentions: UInt32 = 0, muted: Bool = false
    ) -> ConversationItem {
      ConversationItem(
        id: id, maybeSpaceId: nil, maybeSpaceName: nil, kind: .group, name: name, topic: "", unreadCount: unread,
        mentionCount: mentions, isFavorite: false, isMuted: muted, canPost: true)
    }

    private static func channel(
      _ id: String, _ name: String, space: HomeSpace, kind: ConversationKind = .public, unread: UInt32 = 0,
      mentions: UInt32 = 0, favorite: Bool = false, muted: Bool = false
    ) -> ConversationItem {
      ConversationItem(
        id: id, maybeSpaceId: space.id, maybeSpaceName: space.name, kind: kind, name: name, topic: "",
        unreadCount: unread, mentionCount: mentions, isFavorite: favorite, isMuted: muted, canPost: true)
    }
  }
#endif
