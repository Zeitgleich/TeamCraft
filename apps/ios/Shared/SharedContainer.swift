import Foundation

/// Files and small settings the app and the notification service extension share through the App
/// Group (docs/e2ee.md §7, §9.2). Without the entitlement (unsigned simulator build) it falls back
/// to the process's own Application Support folder, so the app still runs; push is then app-only.
struct SharedContainer {
  private static let activeAccountKey = "activeAccount"
  private static let displayNameKey = "displayName"
  private static let appLockKey = "appLockEnabled"

  let environment: AppEnvironment
  private let fileManager: FileManager

  init(environment: AppEnvironment = .current, fileManager: FileManager = .default) {
    self.environment = environment
    self.fileManager = fileManager
  }

  /// The folder the Rust core keeps its sealed database and downloads in. Not in backups (the
  /// database key never leaves the device, so a restored copy is useless); the database is
  /// readable after the first unlock, because the extension opens it while the phone is locked.
  var dataDirectory: URL {
    var url = containerRoot.appendingPathComponent("teamcraft", isDirectory: true)
    prepare(&url, protection: .completeUntilFirstUserAuthentication)
    return url
  }

  /// Where the core writes decrypted downloads (`downloads` below the data directory). Plaintext,
  /// so: unreadable while the phone is locked, not in backups, wiped on sign-out.
  var downloadsDirectory: URL {
    var url = dataDirectory.appendingPathComponent("downloads", isDirectory: true)
    prepare(&url, protection: .complete)
    return url
  }

  /// Files picked for sending, copied here so the core can read them while it uploads (plaintext:
  /// not in backups, readable after the first unlock so an upload goes on with the phone locked).
  var uploadsDirectory: URL {
    var url = dataDirectory.appendingPathComponent("uploads", isDirectory: true)
    prepare(&url, protection: .completeUntilFirstUserAuthentication)
    return url
  }

  /// Removes the copies of picked files older than `age` (uploads that long ago are done), or all
  /// of them with `age` 0 (sign-out).
  func pruneUploads(olderThan age: TimeInterval) {
    let directory = dataDirectory.appendingPathComponent("uploads", isDirectory: true)
    let cutoff = Date().addingTimeInterval(-age)
    let files = (try? fileManager.contentsOfDirectory(at: directory, includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
    for file in files {
      let modified = (try? file.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
      if age <= 0 || (modified ?? .distantPast) < cutoff {
        try? fileManager.removeItem(at: file)
      }
    }
  }

  /// Removes every decrypted download (sign-out).
  func wipeDownloads() {
    try? fileManager.removeItem(at: dataDirectory.appendingPathComponent("downloads", isDirectory: true))
  }

  /// The cross-process lock file (see `ProcessLock`).
  var lockFileURL: URL {
    dataDirectory.appendingPathComponent("process.lock")
  }

  /// Settings both processes read (which account is signed in).
  var defaults: UserDefaults {
    UserDefaults(suiteName: environment.appGroupId) ?? .standard
  }

  /// Whether app lock is on. The notification extension reads it: with app lock on, notifications
  /// say only that something arrived, never who or what (as on Android).
  var isAppLockOn: Bool {
    get { defaults.bool(forKey: Self.appLockKey) }
    nonmutating set { defaults.set(newValue, forKey: Self.appLockKey) }
  }

  /// The OIDC subject of the signed-in account; separates databases on one device.
  var activeAccount: String? {
    get { defaults.string(forKey: Self.activeAccountKey) }
    nonmutating set { defaults.set(newValue, forKey: Self.activeAccountKey) }
  }

  /// The user's name, for notifications that can't ask the core.
  var displayName: String? {
    get { defaults.string(forKey: Self.displayNameKey) }
    nonmutating set { defaults.set(newValue, forKey: Self.displayNameKey) }
  }

  /// Creates `url` with a data protection class (new files inside inherit it) and keeps it out of backups.
  private func prepare(_ url: inout URL, protection: FileProtectionType) {
    try? fileManager.createDirectory(
      at: url, withIntermediateDirectories: true, attributes: [.protectionKey: protection])
    try? fileManager.setAttributes([.protectionKey: protection], ofItemAtPath: url.path)
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? url.setResourceValues(values)
  }

  private var containerRoot: URL {
    if let group = fileManager.containerURL(forSecurityApplicationGroupIdentifier: environment.appGroupId) {
      return group
    }
    let support = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    return support
  }
}
