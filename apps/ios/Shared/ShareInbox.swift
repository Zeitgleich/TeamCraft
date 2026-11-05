import Foundation

/// A share the share extension put into the App Group, waiting for the app and a conversation
/// (issue #208). Nothing of it leaves the device before the user pressed send in the app.
struct StagedShare: Codable, Equatable, Identifiable {
  let id: String
  var text: String
  var files: [StagedShareFile]
  /// Files that couldn't be taken (unreadable, disk full, more than the extension takes).
  var leftOut: Int

  var isEmpty: Bool { text.isEmpty && files.isEmpty }
}

/// One copied file, as `<number>/<cleaned name>` below its share's folder: the name stays readable
/// for the composer, and a sender's name never decides where the file lands.
struct StagedShareFile: Codable, Equatable {
  let storedAs: String
}

/// The shares waiting on this device, oldest first, one folder each. Compiled into the extension,
/// which has no core and stays well inside its memory limit: it copies files with the file system
/// and never reads them. The list is written last, so a folder without one was cut short.
struct ShareInbox {
  /// A share nobody picked a conversation for goes after this long (plaintext at rest).
  private static let keptFor: TimeInterval = 7 * 24 * 3600
  private static let unfinishedKeptFor: TimeInterval = 3600
  private static let listName = "share.json"

  let root: URL
  private let fileManager: FileManager

  init(root: URL, fileManager: FileManager = .default) {
    self.root = root
    self.fileManager = fileManager
  }

  /// The inbox in the App Group. `nil` without the entitlement (an unsigned simulator build): the
  /// app and the extension then share nothing, and the extension says that it failed.
  static func shared(environment: AppEnvironment = .current, fileManager: FileManager = .default) -> ShareInbox? {
    guard let group = fileManager.containerURL(forSecurityApplicationGroupIdentifier: environment.appGroupId) else {
      return nil
    }
    return ShareInbox(root: group.appendingPathComponent("share-inbox", isDirectory: true), fileManager: fileManager)
  }

  /// A new, empty folder for a share. Readable after the first unlock like the upload copies: a
  /// share can be made while the phone is locked in another app's flow. Not in backups.
  func begin(now: Date = Date()) throws -> StagedShare {
    // The time first, so the folders sort by age.
    let id = String(format: "%016lld-%@", Int64(now.timeIntervalSince1970 * 1000), UUID().uuidString)
    var directory = folder(of: id)
    try fileManager.createDirectory(
      at: directory, withIntermediateDirectories: true,
      attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? directory.setResourceValues(values)
    return StagedShare(id: id, text: "", files: [], leftOut: 0)
  }

  /// Copies a file into the share without reading it into memory.
  func add(file source: URL, named name: String, to share: inout StagedShare) throws {
    let storedAs = "\(share.files.count)/\(Self.cleanName(name))"
    let target = folder(of: share.id).appendingPathComponent(storedAs)
    try fileManager.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
    try fileManager.copyItem(at: source, to: target)
    share.files.append(StagedShareFile(storedAs: storedAs))
  }

  /// The share is complete: from now on the app sees it.
  func commit(_ share: StagedShare) throws {
    let list = folder(of: share.id).appendingPathComponent(Self.listName)
    try JSONEncoder().encode(share).write(to: list, options: .atomic)
  }

  /// The oldest share not in `except`.
  func oldest(except: Set<String> = []) -> StagedShare? {
    for folder in folders() where !except.contains(folder.lastPathComponent) {
      if let share = read(folder) { return share }
    }
    return nil
  }

  func url(of file: StagedShareFile, in share: StagedShare) -> URL {
    folder(of: share.id).appendingPathComponent(file.storedAs)
  }

  func delete(_ id: String) {
    try? fileManager.removeItem(at: folder(of: id))
  }

  /// Everything goes (sign-out): shared content doesn't outlive the session on this device.
  func wipe() {
    try? fileManager.removeItem(at: root)
  }

  /// Drops shares past their time and copies that were cut short.
  func prune(now: Date = Date()) {
    let nowMs = now.timeIntervalSince1970 * 1000
    for folder in folders() {
      let startedMs = Double(folder.lastPathComponent.prefix { $0 != "-" }) ?? 0
      let age = (nowMs - startedMs) / 1000
      if age > Self.keptFor || (read(folder) == nil && age > Self.unfinishedKeptFor) {
        try? fileManager.removeItem(at: folder)
      }
    }
  }

  /// The last path segment of a sender's name, without control and direction characters (which can
  /// make `fdp.exe` read as `exe.pdf`), never empty.
  static func cleanName(_ name: String) -> String {
    let last = name.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
    let isHidden: (Unicode.Scalar) -> Bool = { scalar in
      scalar.properties.generalCategory == .control
        || [0x200E, 0x200F].contains(scalar.value)
        || (0x202A...0x202E).contains(scalar.value)
        || (0x2066...0x2069).contains(scalar.value)
    }
    let kept = String(String.UnicodeScalarView(last.unicodeScalars.filter { !isHidden($0) }))
    let clean = kept.trimmingCharacters(in: .whitespaces)
    return clean.isEmpty || clean == "." || clean == ".." ? "file" : clean
  }

  private func folder(of id: String) -> URL {
    root.appendingPathComponent(id, isDirectory: true)
  }

  private func folders() -> [URL] {
    let found = (try? fileManager.contentsOfDirectory(at: root, includingPropertiesForKeys: nil)) ?? []
    return found.sorted { $0.lastPathComponent < $1.lastPathComponent }
  }

  private func read(_ folder: URL) -> StagedShare? {
    guard let list = try? Data(contentsOf: folder.appendingPathComponent(Self.listName)) else { return nil }
    return try? JSONDecoder().decode(StagedShare.self, from: list)
  }
}
