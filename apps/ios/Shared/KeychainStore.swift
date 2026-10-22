import Foundation
import Security

/// Failures of the Keychain wrapper. Carries the OSStatus for logs; never the value.
enum KeychainError: Error, Equatable {
  case unexpectedStatus(OSStatus)
  case randomFailed(OSStatus)
}

/// Generic-password items in the keychain access group the app shares with its notification
/// extension. Items are readable after the first unlock and never leave the device, so the
/// extension can open the store while the phone is locked (docs/e2ee.md §7, §9.2).
struct KeychainStore {
  let service: String
  /// Empty: the process's default group (unsigned simulator builds have no shared one).
  let accessGroup: String

  init(service: String = "com.example.teamcraft", accessGroup: String = AppEnvironment.current.keychainAccessGroup) {
    self.service = service
    self.accessGroup = accessGroup
  }

  func read(_ account: String) throws -> Data? {
    var query = baseQuery(account, grouped: true)
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var status = copy(query)
    if status.status == errSecMissingEntitlement, !accessGroup.isEmpty {
      query = baseQuery(account, grouped: false)
      query[kSecReturnData as String] = true
      query[kSecMatchLimit as String] = kSecMatchLimitOne
      status = copy(query)
    }
    switch status.status {
    case errSecSuccess: return status.data
    case errSecItemNotFound: return nil
    default: throw KeychainError.unexpectedStatus(status.status)
    }
  }

  func write(_ data: Data, for account: String) throws {
    let attributes: [String: Any] = [
      kSecValueData as String: data,
      kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
    ]
    for grouped in [true, false] {
      var status = SecItemUpdate(baseQuery(account, grouped: grouped) as CFDictionary, attributes as CFDictionary)
      if status == errSecItemNotFound {
        var item = baseQuery(account, grouped: grouped)
        item.merge(attributes) { _, new in new }
        status = SecItemAdd(item as CFDictionary, nil)
      }
      if status == errSecSuccess { return }
      // Unsigned builds have no shared group: use the process's own.
      if status == errSecMissingEntitlement, grouped, !accessGroup.isEmpty { continue }
      throw KeychainError.unexpectedStatus(status)
    }
  }

  func delete(_ account: String) throws {
    for grouped in [true, false] {
      let status = SecItemDelete(baseQuery(account, grouped: grouped) as CFDictionary)
      if status == errSecSuccess || status == errSecItemNotFound { return }
      if status == errSecMissingEntitlement, grouped, !accessGroup.isEmpty { continue }
      throw KeychainError.unexpectedStatus(status)
    }
  }

  private func baseQuery(_ account: String, grouped: Bool) -> [String: Any] {
    var query: [String: Any] = [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service,
      kSecAttrAccount as String: account,
    ]
    if grouped, !accessGroup.isEmpty {
      query[kSecAttrAccessGroup as String] = accessGroup
    }
    return query
  }

  private func copy(_ query: [String: Any]) -> (status: OSStatus, data: Data?) {
    var result: AnyObject?
    let status = SecItemCopyMatching(query as CFDictionary, &result)
    return (status, result as? Data)
  }
}

/// The 256-bit key of the core's sealed database (docs/e2ee.md §9.2), one per account, generated on
/// first use and kept in the keychain. The extension reads the same item.
struct DatabaseKeyStore {
  static let keyLength = 32

  let keychain: KeychainStore

  init(keychain: KeychainStore = KeychainStore()) {
    self.keychain = keychain
  }

  func key(for account: String) throws -> Data {
    let name = "database-key.\(account)"
    if let existing = try keychain.read(name), existing.count == Self.keyLength {
      return existing
    }
    var bytes = [UInt8](repeating: 0, count: Self.keyLength)
    let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
    guard status == errSecSuccess else { throw KeychainError.randomFailed(status) }
    let fresh = Data(bytes)
    try keychain.write(fresh, for: name)
    return fresh
  }

  /// Existing key only; the extension never creates one (no key: not signed in, show a generic text).
  func existingKey(for account: String) throws -> Data? {
    let data = try keychain.read("database-key.\(account)")
    return data?.count == Self.keyLength ? data : nil
  }

  func delete(for account: String) throws {
    try keychain.delete("database-key.\(account)")
  }
}
