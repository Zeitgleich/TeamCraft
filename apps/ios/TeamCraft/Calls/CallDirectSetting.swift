import Foundation

/// "Connect directly in calls" (Settings → Chats, #127): a 1:1 call may send its media straight to
/// the other device where the workspace allows it, which then sees this device's IP addresses. On
/// by default; follows the account. The core reads it at every join and follows a change at once.
enum CallDirectSetting {
  static let key = "call_direct_connections"

  static var isAllowed: Bool {
    stored(in: .standard)
  }

  /// The stored choice; nothing stored is on.
  static func stored(in defaults: UserDefaults) -> Bool {
    defaults.object(forKey: key) as? Bool ?? true
  }
}
