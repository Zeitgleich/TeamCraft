import CryptoKit
import Foundation
import Security

/// The device's key for DPoP proofs as the keychain holds it (docs/e2ee.md §3.1.2, issue #120). The
/// private half is either the Secure Enclave's wrapped key, which only this device's enclave can
/// open, or, where there is no enclave or it made no key, a software key. Item: `dpop-key.<storage
/// key>` in the keychain group the app shares with its notification extension, readable after the
/// first unlock and never leaving the device (`ThisDeviceOnly`), so a restored phone has none of it.
struct StoredDeviceKey: Codable, Equatable {
  enum Kind: String, Codable, Equatable {
    case secureEnclave, software
  }

  let kind: Kind
  /// The Secure Enclave's wrapped key, or a software key's private scalar.
  let keyData: Data
  /// The public half as an uncompressed SEC1 point, so that reading it needs no enclave.
  let publicPoint: Data

  /// A new key in the Secure Enclave; `nil` where there is none. It signs without asking the user
  /// (`.privateKeyUsage` and nothing else), and also while the phone is locked after its first unlock,
  /// as the notification extension needs.
  static func enclaveKey() throws -> StoredDeviceKey? {
    guard SecureEnclave.isAvailable else { return nil }
    var failure: Unmanaged<CFError>?
    guard
      let access = SecAccessControlCreateWithFlags(
        nil, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, .privateKeyUsage, &failure)
    else {
      throw failure?.takeRetainedValue() ?? DeviceKeyStoreError.noEnclaveAccessControl
    }
    let key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access)
    return StoredDeviceKey(
      kind: .secureEnclave, keyData: key.dataRepresentation, publicPoint: key.publicKey.x963Representation)
  }

  /// The ES256 signature of `message`: 64 bytes, `r || s`, as a JWS carries it.
  func signature(of message: Data) throws -> Data {
    switch kind {
    case .secureEnclave:
      return try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: keyData)
        .signature(for: message).rawRepresentation
    case .software:
      return try P256.Signing.PrivateKey(rawRepresentation: keyData)
        .signature(for: message).rawRepresentation
    }
  }

  static func softwareKey() -> StoredDeviceKey {
    let key = P256.Signing.PrivateKey()
    return StoredDeviceKey(
      kind: .software, keyData: key.rawRepresentation, publicPoint: key.publicKey.x963Representation)
  }
}

// security.md: never print a key, also not by accident through `String(describing:)`.
extension StoredDeviceKey: CustomStringConvertible, CustomDebugStringConvertible {
  var description: String { "StoredDeviceKey(\(kind.rawValue), [redacted])" }
  var debugDescription: String { description }
}

enum DeviceKeyStoreError: Error, Equatable {
  /// The Secure Enclave's access control couldn't be made.
  case noEnclaveAccessControl
}

/// The key of the active server: one per server, gone at sign-out with the session it was made for.
/// The app makes it (`keyOrNew`); the notification extension only reads it.
struct DeviceKeyStore: @unchecked Sendable {
  private static let accountPrefix = "dpop-key."

  let keychain: any SecretItems
  let servers: ServerStore
  let makeEnclaveKey: @Sendable () throws -> StoredDeviceKey?

  init(
    keychain: any SecretItems = KeychainStore(), servers: ServerStore = ServerStore(),
    makeEnclaveKey: @escaping @Sendable () throws -> StoredDeviceKey? = StoredDeviceKey.enclaveKey
  ) {
    self.keychain = keychain
    self.servers = servers
    self.makeEnclaveKey = makeEnclaveKey
  }

  /// The active server's key. `nil` only if the keychain was read and has none (a damaged item
  /// counts as none); a keychain that can't be read throws, which says nothing about the session.
  func read() throws -> StoredDeviceKey? {
    guard let account = activeItem else { return nil }
    guard let data = try keychain.read(account) else { return nil }
    return try? JSONDecoder().decode(StoredDeviceKey.self, from: data)
  }

  /// The active server's key, made on first use: in the Secure Enclave where that works, otherwise
  /// in software (the Simulator, or an enclave that refuses). A read that fails throws, so that a
  /// locked keychain can't overwrite a key that is there.
  func keyOrNew() throws -> StoredDeviceKey {
    guard let account = activeItem else { throw SessionVaultError.noServer }
    if let existing = try read() { return existing }
    let key = Self.newKey(makeEnclaveKey: makeEnclaveKey)
    try keychain.write(JSONEncoder().encode(key), for: account)
    return key
  }

  /// Sign-out: nothing to do about a failed delete, the next sign-in overwrites the item or reuses it.
  func delete() {
    guard let account = activeItem else { return }
    try? keychain.delete(account)
  }

  /// The active server: a key is that server's and no other's.
  var activeServerKey: String? {
    servers.active?.storageKey
  }

  private var activeItem: String? {
    activeServerKey.map { Self.accountPrefix + $0 }
  }

  private static func newKey(makeEnclaveKey: () throws -> StoredDeviceKey?) -> StoredDeviceKey {
    do {
      if let key = try makeEnclaveKey() { return key }
      AppLog.setup.info("no Secure Enclave here, the device key is a software key")
    } catch {
      // Only the error's type: nothing else is known to be free of what the enclave was asked.
      let type = String(reflecting: Swift.type(of: error))
      AppLog.setup.warning(
        "the Secure Enclave made no key (\(type, privacy: .public)), the device key is a software key")
    }
    return .softwareKey()
  }
}

/// The active server's key as the Rust core signs with it. Read anew for each use, so the core
/// never holds a key that a sign-out removed. Called on the core's threads.
final class VaultDeviceKey: DeviceProofKey, @unchecked Sendable {
  private let store: DeviceKeyStore

  init(store: DeviceKeyStore) {
    self.store = store
  }

  func publicKey() throws -> Data {
    try stored().publicPoint
  }

  func sign(message: Data) throws -> Data {
    let key = try stored()
    do {
      return try key.signature(of: message)
    } catch {
      // The keychain had the key; signing failed for now. Nothing about the session is decided by
      // an enclave that doesn't answer (see README.md, "Device key").
      throw DeviceProofError.KeyUnavailable
    }
  }

  private func stored() throws -> StoredDeviceKey {
    let maybeKey: StoredDeviceKey?
    do {
      maybeKey = try store.read()
    } catch {
      throw DeviceProofError.KeyUnavailable
    }
    guard let maybeKey else { throw DeviceProofError.KeyLost }
    return maybeKey
  }
}
