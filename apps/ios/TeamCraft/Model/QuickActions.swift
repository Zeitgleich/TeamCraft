import Observation
import UIKit

/// The actions of the app icon's long-press menu (issue #2061): where they lead.
enum QuickAction: String, CaseIterable, Sendable {
  case newMessage = "de.zeitgleich.teamcraft.quickaction.new_message"
  case search = "de.zeitgleich.teamcraft.quickaction.search"

  /// The action a shortcut item asks for; nil for any other type.
  init?(item: UIApplicationShortcutItem) { self.init(rawValue: item.type) }

  var symbol: String {
    switch self {
    case .newMessage: "square.and.pencil"
    case .search: "magnifyingglass"
    }
  }

  var title: String {
    switch self {
    case .newMessage: String(localized: "quick_action_new_message")
    case .search: String(localized: "quick_action_search")
    }
  }

  var shortcutItem: UIApplicationShortcutItem {
    UIApplicationShortcutItem(
      type: rawValue, localizedTitle: title, localizedSubtitle: nil,
      icon: UIApplicationShortcutIcon(systemImageName: symbol), userInfo: nil)
  }
}

/// A recent conversation in the app icon's menu (#2061): its name and nothing of its messages.
struct RecentConversationShortcut: Equatable, Sendable {
  /// Where the quick actions of a conversation are told apart from "New message" and "Search".
  static let type = "de.zeitgleich.teamcraft.quickaction.conversation"
  /// How many the menu gets: the system shows four items, and the two fixed ones come first.
  static let maxCount = 2
  private static let idKey = "conversation"

  let conversationId: String
  let title: String

  init(conversationId: String, title: String) {
    self.conversationId = conversationId
    self.title = title
  }

  /// The conversation a shortcut item of this kind opens; nil for any other item.
  init?(item: UIApplicationShortcutItem) {
    guard item.type == Self.type, let id = item.userInfo?[Self.idKey] as? String, !id.isEmpty else { return nil }
    self.init(conversationId: id, title: item.localizedTitle)
  }

  var shortcutItem: UIApplicationShortcutItem {
    UIApplicationShortcutItem(
      type: Self.type, localizedTitle: title, localizedSubtitle: nil,
      icon: UIApplicationShortcutIcon(systemImageName: "bubble.left"), userInfo: [Self.idKey: conversationId as NSString])
  }
}

/// The quick action the chat list has not taken yet. Set by the scene when the user picks one,
/// also at a cold start, before the list is there.
@MainActor
@Observable
final class QuickActions {
  static let shared = QuickActions()

  var pending: QuickAction?

  /// The conversation a recent-conversation shortcut asked for, until the list opens it.
  var pendingConversation: String?

  /// Puts the actions into the icon's menu; the system keeps them between launches.
  func publish() {
    publish(recent: [])
  }

  /// The fixed actions and then the newest conversations (#2061), at most [RecentConversationShortcut.maxCount].
  /// Called when the app goes to the background, so the menu names what was newest when it left.
  func publish(recent: [RecentConversationShortcut]) {
    UIApplication.shared.shortcutItems =
      QuickAction.allCases.map(\.shortcutItem) + recent.prefix(RecentConversationShortcut.maxCount).map(\.shortcutItem)
  }
}

/// Takes the quick action of a launch or of a running app (the SwiftUI lifecycle has no hook for it).
final class QuickActionSceneDelegate: NSObject, UIWindowSceneDelegate {
  func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options connectionOptions: UIScene.ConnectionOptions) {
    guard let item = connectionOptions.shortcutItem else { return }
    _ = take(item)
  }

  func windowScene(
    _ windowScene: UIWindowScene, performActionFor shortcutItem: UIApplicationShortcutItem,
    completionHandler: @escaping (Bool) -> Void
  ) {
    completionHandler(take(shortcutItem))
  }

  /// Hands the item to the chat list; false for an item that is none of ours.
  private func take(_ item: UIApplicationShortcutItem) -> Bool {
    if let action = QuickAction(item: item) {
      Task { @MainActor in QuickActions.shared.pending = action }
      return true
    }
    if let conversation = RecentConversationShortcut(item: item) {
      Task { @MainActor in QuickActions.shared.pendingConversation = conversation.conversationId }
      return true
    }
    return false
  }
}
