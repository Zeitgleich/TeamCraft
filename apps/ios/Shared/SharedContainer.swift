import Foundation

/// Files and small settings the app and the notification service extension share through the App
/// Group (docs/e2ee.md §7, §9.2). Without the entitlement (unsigned simulator build) it falls back
/// to the process's own Application Support folder, so the app still runs; push is then app-only.
struct SharedContainer {
  private static let activeAccountKey = "activeAccount"
  private static let displayNameKey = "displayName"

  let environment: AppEnvironment
  private let fileManager: FileManager

  init(environment: AppEnvironment = .current, fileManager: FileManager = .default) {
    self.environment = environment
    self.fileManager = fileManager
  }

  /// The folder the Rust core keeps its sealed database and downloads in.
  var dataDirectory: URL {
    let url = containerRoot.appendingPathComponent("teamcraft", isDirectory: true)
    try? fileManager.createDirectory(at: url, withIntermediateDirectories: true)
    return url
  }

  /// The cross-process lock file (see `ProcessLock`).
  var lockFileURL: URL {
    dataDirectory.appendingPathComponent("process.lock")
  }

  /// Settings both processes read (which account is signed in).
  var defaults: UserDefaults {
    UserDefaults(suiteName: environment.appGroupId) ?? .standard
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

  private var containerRoot: URL {
    if let group = fileManager.containerURL(forSecurityApplicationGroupIdentifier: environment.appGroupId) {
      return group
    }
    let support = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    return support
  }
}
