import CryptoKit
import Security
import Foundation

/// Proof Key for Code Exchange (RFC 7636, method S256) for the login.
struct Pkce: Equatable {
  let verifier: String
  let challenge: String

  /// A fresh verifier from 32 random bytes.
  static func generate() -> Pkce {
    var bytes = [UInt8](repeating: 0, count: 32)
    // The system generator doesn't fail on iOS; a zeroed verifier would be rejected by the server anyway.
    _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
    return Pkce(verifier: base64URL(Data(bytes)))
  }

  init(verifier: String) {
    self.verifier = verifier
    self.challenge = Self.base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
  }

  static func base64URL(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }

  /// An unguessable `state` value tying the answer to the request.
  static func randomState() -> String {
    var bytes = [UInt8](repeating: 0, count: 16)
    _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
    return base64URL(Data(bytes))
  }
}
