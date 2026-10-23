import Foundation

/// Which call status sounds play on this device (Settings → Chats): a soft repeating tone while
/// the connection is lost, short tones when it is back or the call ended, and a quieter one when
/// someone else drops. The core mixes them into the call's playback, so they play in the active
/// call audio session and follow its route.
enum CallStatusSoundsSetting: String {
  case all
  case ownOnly = "own_only"
  case off

  static let key = "call_status_sounds"

  static var current: CallStatusSoundsSetting {
    UserDefaults.standard.string(forKey: key).flatMap(CallStatusSoundsSetting.init(rawValue:)) ?? .all
  }

  var sounds: CallStatusSounds {
    switch self {
    case .all: return .all
    case .ownOnly: return .ownOnly
    case .off: return .off
    }
  }
}
