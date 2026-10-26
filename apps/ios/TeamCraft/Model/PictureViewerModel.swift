import Foundation
import ImageIO
import Observation
import UIKit

/// The picture the image viewer opens on.
struct PictureTarget: Identifiable, Equatable {
  let file: FileInfo

  var id: String { "\(file.message):\(file.fileId)" }
}

/// When the image viewer fetches a picture's original by itself (Settings → Chats). Kept on this
/// device: the data plan is the phone's, not the account's. The thumbnail shows at once either way.
enum OriginalDownloadSetting: String, CaseIterable, Identifiable {
  case always
  case unmeteredOnly = "unmetered_only"
  case never

  static let key = "original_download"

  static var current: OriginalDownloadSetting {
    UserDefaults.standard.string(forKey: key).flatMap(OriginalDownloadSetting.init(rawValue:)) ?? .always
  }

  var id: String { rawValue }

  var setting: OriginalDownload {
    switch self {
    case .always: return .always
    case .unmeteredOnly: return .unmeteredOnly
    case .never: return .never
    }
  }
}

/// Where a page's original stands.
enum OriginalState: Equatable {
  /// Decoded and on screen.
  case shown
  /// Coming: downloading (the share of the bytes once known) or being decoded.
  case loading(Double?)
  /// Waiting for "Load original" (the setting, or a metered network with "Wi-Fi only").
  case onRequest
  /// The download failed; "Try again".
  case failed
  /// Downloaded, but this device can't decode the format.
  case unreadable
  /// Nothing to fetch (an own picture still going up): the thumbnail is all there is.
  case thumbnailOnly
}

/// The state of an open image viewer. Which pictures it pages through and which page of the
/// conversation it reads next come from the core (`MediaViewer`, shared with Android, desktop and
/// web); this keeps what the screen needs: the files of the picture on screen and its neighbours,
/// and their decoded originals. Nothing further away stays decoded, so memory stays bounded
/// however long the album or the conversation is.
@MainActor
@Observable
final class PictureViewerModel {
  let target: PictureTarget
  private(set) var items: [MediaViewerItem] = []
  /// The opened message has been read: an empty list now means its pictures are gone.
  private(set) var isLoaded = false
  private(set) var scope = MediaScope.conversation
  private(set) var canSwitchScope = false
  /// The picture on screen (the pager's position).
  var currentKey: String?
  /// The files of the picture on screen and its neighbours, by item key.
  private(set) var files: [String: FileInfo] = [:]
  /// Their decoded originals.
  private(set) var originals: [String: UIImage] = [:]
  private(set) var unreadable: Set<String> = []

  @ObservationIgnored private let maybeViewer: MediaViewer?
  @ObservationIgnored private let limits = mediaViewerLimits()
  /// Originals the user asked for although the setting waits for a tap.
  @ObservationIgnored private var requested: Set<String> = []
  /// Downloads asked for, so a picture isn't asked for again while the core starts it.
  @ObservationIgnored private var fetching: Set<String> = []
  @ObservationIgnored private var decoding: Set<String> = []

  init(target: PictureTarget) {
    self.target = target
    maybeViewer = try? MediaViewer(
      conversationId: target.file.conversation, messageId: target.file.message, fileId: target.file.fileId)
    currentKey = target.id
  }

  /// The longest side an original is decoded at; the screen sets it from its size.
  var maxDecodePixels: CGFloat = 4096

  /// The same zoom limits as on every platform.
  var maxZoom: CGFloat { CGFloat(limits.maxZoom) }
  var doubleTapZoom: CGFloat { CGFloat(limits.doubleTapZoom) }

  /// The opened picture's message or all its pictures are gone: the viewer closes.
  var isGone: Bool { maybeViewer == nil || (isLoaded && items.isEmpty) }

  var currentIndex: Int? { items.firstIndex { $0.key == currentKey } }

  var currentFile: FileInfo? { currentKey.flatMap { files[$0] } }

  /// A download in the window is under way: its progress is read again while it runs.
  var isDownloading: Bool { files.values.contains { $0.transfer.state == "downloading" } }

  /// Reads what the viewer needs (the opened message, then pages of the conversation near the
  /// picture on screen) until it needs nothing more.
  func load(app: AppModel) async {
    guard let viewer = maybeViewer, let core = try? app.core() else { return }
    while let query = viewer.nextQuery() {
      // A read that fails isn't asked again; the viewer keeps what it has.
      guard let answer = try? await core.query(queryJson: query) else { break }
      _ = try? viewer.addAnswer(queryJson: query, answerJson: answer)
      apply()
    }
    apply()
    await advance(app: app)
  }

  /// After a change in the core: the files on screen again (a download's progress or end, a
  /// deleted message).
  func refresh(app: AppModel) async {
    guard isLoaded, let viewer = maybeViewer, let core = try? app.core(), let query = viewer.refreshQuery(),
      let answer = try? await core.query(queryJson: query)
    else { return }
    if (try? viewer.addAnswer(queryJson: query, answerJson: answer)) == true { apply() }
    await advance(app: app)
  }

  /// The user paged to `key`; `load` then reads further pages if it came near an end.
  func show(_ key: String?) {
    guard maybeViewer != nil, key != nil else { return }
    apply()
  }

  func setScope(_ scope: MediaScope, app: AppModel) async {
    maybeViewer?.setScope(scope: scope)
    apply()
    await load(app: app)
  }

  /// Where the original of `key` stands.
  func originalState(_ key: String) -> OriginalState {
    if originals[key] != nil { return .shown }
    if unreadable.contains(key) { return .unreadable }
    guard let file = files[key] else { return .loading(nil) }
    switch file.transfer.state {
    case "local": return .loading(nil)
    case "downloading": return .loading(file.transfer.maybeProgress)
    case "failed": return .failed
    case "remote": return isFetched(key) ? .loading(nil) : .onRequest
    default: return .thumbnailOnly
    }
  }

  /// "Load original" or "Try again" on a page.
  func fetchOriginal(_ key: String, app: AppModel) async {
    requested.insert(key)
    fetching.remove(key)
    await advance(app: app)
  }

  /// Starts what the pictures in the window need: the download of the one on screen when the setting
  /// allows it (or the user asked), and decoding of originals that are here.
  private func advance(app: AppModel) async {
    for (key, file) in files {
      if file.transfer.state == "local", let path = file.transfer.path {
        decode(key, path: path)
        continue
      }
      // Only the picture on screen is fetched; a failed one waits for "Try again".
      let isWanted =
        (file.transfer.state == "remote" && isFetched(key))
        || (file.transfer.state == "failed" && requested.contains(key))
      guard isWanted, key == currentKey, !fetching.contains(key) else { continue }
      fetching.insert(key)
      await app.perform(
        .downloadAttachment(conversationId: file.conversation, messageId: file.message, fileId: file.fileId))
    }
  }

  /// Whether the original of `key` is fetched without a tap: the setting on this network, or the
  /// user asked.
  private func isFetched(_ key: String) -> Bool {
    requested.contains(key)
      || fetchesOriginalAutomatically(
        setting: OriginalDownloadSetting.current.setting, isMetered: NetworkMonitor.shared.isMetered)
  }

  /// Reads the list and the window's files from the core, and lets go of what left the window.
  private func apply() {
    guard let viewer = maybeViewer else { return }
    // The pager is the truth for where the user is: a read that answers after a swipe keeps it.
    if let key = currentKey, let item = items.first(where: { $0.key == key }) {
      _ = viewer.show(messageId: item.messageId, fileId: item.fileId)
    }
    let state = viewer.state()
    items = state.items
    isLoaded = state.isLoaded
    scope = state.scope
    canSwitchScope = state.canSwitchScope
    let index = Int(state.index)
    guard items.indices.contains(index) else {
      files = [:]
      originals = [:]
      return
    }
    if currentKey != items[index].key { currentKey = items[index].key }
    let radius = Int(limits.decodedNeighbors)
    let window = items[max(0, index - radius)...min(items.count - 1, index + radius)]
    var fresh: [String: FileInfo] = [:]
    for item in window {
      if let json = viewer.fileJson(messageId: item.messageId, fileId: item.fileId),
        let file = Wire.decode(FileInfo.self, from: json)
      {
        fresh[item.key] = file
      }
    }
    if fresh != files { files = fresh }
    let kept = Set(fresh.keys)
    if originals.keys.contains(where: { !kept.contains($0) }) { originals = originals.filter { kept.contains($0.key) } }
    unreadable.formIntersection(kept)
  }

  /// Decodes a downloaded original off the main thread, at about twice the screen's size (sharp
  /// while zooming in for a while, bounded in memory).
  private func decode(_ key: String, path: String) {
    guard originals[key] == nil, !unreadable.contains(key), !decoding.contains(key) else { return }
    decoding.insert(key)
    let maxPixels = maxDecodePixels
    Task {
      let maybeImage = await Task.detached(priority: .userInitiated) {
        PictureDecoder.decode(path: path, maxPixels: maxPixels)
      }.value
      decoding.remove(key)
      // Paged away meanwhile: not kept.
      guard files[key] != nil else { return }
      if let image = maybeImage {
        originals[key] = image
      } else {
        unreadable.insert(key)
      }
    }
  }
}

/// Decodes pictures with ImageIO: scaled down while decoding (never the full bitmap of a 50 MP
/// photo) and turned upright by its orientation.
enum PictureDecoder {
  /// The longest side a decoded original gets: twice the screen, at most 4096 px (GPU limits).
  static func maxPixels(screen: CGSize, scale: CGFloat) -> CGFloat {
    min(4096, max(screen.width, screen.height) * scale * 2)
  }

  static func decode(path: String, maxPixels: CGFloat) -> UIImage? {
    let url = URL(fileURLWithPath: path)
    guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary)
    else { return nil }
    let options: [CFString: Any] = [
      kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true,
      kCGImageSourceShouldCacheImmediately: true,
      kCGImageSourceThumbnailMaxPixelSize: maxPixels,
    ]
    guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else { return nil }
    return UIImage(cgImage: image)
  }
}
