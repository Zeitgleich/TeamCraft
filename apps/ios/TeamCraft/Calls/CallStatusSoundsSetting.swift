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
    stored(UserDefaults.standard.string(forKey: key))
  }

  /// The stored choice; nothing (or something unknown) is `all`.
  static func stored(_ maybeRaw: String?) -> CallStatusSoundsSetting {
    maybeRaw.flatMap(CallStatusSoundsSetting.init(rawValue:)) ?? .all
  }

  /// The stored name of a choice of the core's type (the account's settings sync, #1483).
  static func raw(_ sounds: CallStatusSounds) -> String {
    switch sounds {
    case .all: return CallStatusSoundsSetting.all.rawValue
    case .ownOnly: return CallStatusSoundsSetting.ownOnly.rawValue
    case .off: return CallStatusSoundsSetting.off.rawValue
    }
  }

  var sounds: CallStatusSounds {
    switch self {
    case .all: return .all
    case .ownOnly: return .ownOnly
    case .off: return .off
    }
  }
}
