import Foundation

/// The pictures of spaces on disk (#284), one file per space and version: a new version is a new
/// file and the old one is deleted, a space whose picture was removed loses its files. The folder is
/// in the system's caches (it may be cleared at any time, then the pictures are fetched again) and
/// holds at most `maxFiles` pictures, the least recently written going first.
actor SpacePictureStore {
  static let maxFiles = 64

  private let directory: URL
  private let fileManager = FileManager.default

  init(directory: URL) {
    self.directory = directory
  }

  /// The stored picture of `spaceId` in `version`, or `nil`.
  func data(spaceId: String, version: String) -> Data? {
    guard let url = fileURL(spaceId: spaceId, version: version) else { return nil }
    return try? Data(contentsOf: url)
  }

  /// Stores the picture, replacing older versions of the same space.
  func save(_ data: Data, spaceId: String, version: String) {
    guard let url = fileURL(spaceId: spaceId, version: version) else { return }
    do {
      try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
      try data.write(to: url, options: .atomic)
    } catch {
      // A full or unwritable cache: the picture is fetched again next time.
      return
    }
    removeFiles(of: spaceId, except: url)
    trim()
  }

  /// A space without a picture (any more) keeps no file.
  func remove(spaceId: String) {
    removeFiles(of: spaceId, except: nil)
  }

  /// Every picture goes (sign-out): they belong to the account that is gone.
  func clear() {
    try? fileManager.removeItem(at: directory)
  }

  /// Only numeric space IDs and plain versions name a file.
  private func fileURL(spaceId: String, version: String) -> URL? {
    guard !spaceId.isEmpty, spaceId.allSatisfy(\.isASCIIDigit) else { return nil }
    let safe = String(version.filter { $0.isLetter || $0.isNumber }.prefix(64))
    guard !safe.isEmpty else { return nil }
    return directory.appendingPathComponent("\(spaceId)-\(safe).png")
  }

  private func removeFiles(of spaceId: String, except kept: URL?) {
    for url in files() where url.lastPathComponent.hasPrefix("\(spaceId)-") && url != kept {
      try? fileManager.removeItem(at: url)
    }
  }

  private func trim() {
    let all = files().sorted { modified($0) < modified($1) }
    for url in all.dropLast(Self.maxFiles) { try? fileManager.removeItem(at: url) }
  }

  private func files() -> [URL] {
    (try? fileManager.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
  }

  private func modified(_ url: URL) -> Date {
    (try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
  }
}

extension Character {
  fileprivate var isASCIIDigit: Bool { isASCII && isNumber }
}
