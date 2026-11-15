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

/// The quick action the chat list has not taken yet. Set by the scene when the user picks one,
/// also at a cold start, before the list is there.
@MainActor
@Observable
final class QuickActions {
  static let shared = QuickActions()

  var pending: QuickAction?

  /// Puts the actions into the icon's menu; the system keeps them between launches.
  func publish() {
    UIApplication.shared.shortcutItems = QuickAction.allCases.map(\.shortcutItem)
  }
}

/// Takes the quick action of a launch or of a running app (the SwiftUI lifecycle has no hook for it).
final class QuickActionSceneDelegate: NSObject, UIWindowSceneDelegate {
  func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options connectionOptions: UIScene.ConnectionOptions) {
    guard let item = connectionOptions.shortcutItem, let action = QuickAction(item: item) else { return }
    Task { @MainActor in QuickActions.shared.pending = action }
  }

  func windowScene(
    _ windowScene: UIWindowScene, performActionFor shortcutItem: UIApplicationShortcutItem,
    completionHandler: @escaping (Bool) -> Void
  ) {
    guard let action = QuickAction(item: shortcutItem) else {
      completionHandler(false)
      return
    }
    Task { @MainActor in QuickActions.shared.pending = action }
    completionHandler(true)
  }
}
