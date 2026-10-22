import Foundation

/// The rules for a new recovery passphrase (shown before the core is asked).
enum PassphraseRules {
  static let minimumLength = 12

  enum Problem: Equatable {
    case tooShort, mismatch
  }

  static func problem(passphrase: String, repeated: String) -> Problem? {
    if passphrase.count < minimumLength { return .tooShort }
    if passphrase != repeated { return .mismatch }
    return nil
  }
}
