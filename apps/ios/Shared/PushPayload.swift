import Foundation

/// How the sealed wake-up (docs/e2ee.md §7) arrives in an APNs notification: the server puts the
/// XChaCha20-Poly1305 ciphertext, base64url without padding, under the custom key `w` next to a
/// generic `aps` alert with `mutable-content: 1`, so the extension can rewrite it. Same encoding as
/// the FCM data message of the Android app.
enum PushPayload {
  static let sealedKey = "w"

  /// The sealed bytes, or `nil` if the payload carries none or they are malformed.
  static func sealedBytes(from userInfo: [AnyHashable: Any]) -> Data? {
    guard let encoded = userInfo[sealedKey] as? String else { return nil }
    return decodeBase64URL(encoded)
  }

  static func decodeBase64URL(_ text: String) -> Data? {
    var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
    base64 = base64.trimmingCharacters(in: CharacterSet(charactersIn: "="))
    while base64.count % 4 != 0 { base64.append("=") }
    guard let data = Data(base64Encoded: base64), !data.isEmpty else { return nil }
    return data
  }
}

extension Data {
  /// Lowercase hex, the form of an APNs device token the server expects.
  var hexString: String {
    map { String(format: "%02x", $0) }.joined()
  }
}
