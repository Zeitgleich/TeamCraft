import CryptoKit
import Foundation
import Security

/// Tags the conversation ID of a notification the extension built, with a per-install key in the
/// shared keychain. A notification the server showed without the extension (no `mutable-content`)
/// carries no valid tag, so reply and mark-as-read refuse it; the server never sees the key.
struct NotificationAuthenticator {
  static let tagKey = "conversation_tag"
  private static let keyName = "notification-tag-key"

  let keychain: KeychainStore

  init(keychain: KeychainStore = KeychainStore()) {
    self.keychain = keychain
  }

  /// The app creates the key (first sign-in); the extension only reads it.
  func ensureKey() throws {
    if try keychain.read(Self.keyName) != nil { return }
    var bytes = [UInt8](repeating: 0, count: 32)
    let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
    guard status == errSecSuccess else { throw KeychainError.randomFailed(status) }
    try keychain.write(Data(bytes), for: Self.keyName)
  }

  func removeKey() {
    try? keychain.delete(Self.keyName)
  }

  /// Hex tag of `conversationId`, or `nil` without a key.
  func tag(for conversationId: String) -> String? {
    guard let key = try? keychain.read(Self.keyName) else { return nil }
    return Self.tag(for: conversationId, key: key)
  }

  func isValid(tag: String?, for conversationId: String) -> Bool {
    guard let tag, let key = try? keychain.read(Self.keyName) else { return false }
    return Self.isValid(tag: tag, for: conversationId, key: key)
  }

  static func tag(for conversationId: String, key: Data) -> String {
    let code = HMAC<SHA256>.authenticationCode(
      for: Data(conversationId.utf8), using: SymmetricKey(data: key))
    return Data(code).hexString
  }

  /// Constant-time comparison (CryptoKit's `isValidAuthenticationCode`).
  static func isValid(tag: String, for conversationId: String, key: Data) -> Bool {
    guard let tagBytes = hexBytes(tag) else { return false }
    return HMAC<SHA256>.isValidAuthenticationCode(
      tagBytes, authenticating: Data(conversationId.utf8), using: SymmetricKey(data: key))
  }

  private static func hexBytes(_ text: String) -> Data? {
    guard text.count % 2 == 0 else { return nil }
    var data = Data()
    var index = text.startIndex
    while index < text.endIndex {
      let next = text.index(index, offsetBy: 2)
      guard let byte = UInt8(text[index..<next], radix: 16) else { return nil }
      data.append(byte)
      index = next
    }
    return data
  }
}
