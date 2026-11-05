import ImageIO
import Observation
import SwiftUI
import UIKit

/// When animated images in the conversation move (#201). Kept on this device, like the app's other
/// settings. Phones have no hover to wait for.
enum AnimatedImagesSetting: String, CaseIterable, Identifiable {
  case autoplay
  case onTap = "on_tap"

  static let key = "animated_images"
  static let fallback = AnimatedImagesSetting.autoplay

  /// The stored choice; nothing (or something unknown) plays automatically.
  static func stored(_ maybeRaw: String?) -> AnimatedImagesSetting {
    maybeRaw.flatMap(AnimatedImagesSetting.init(rawValue:)) ?? fallback
  }

  var id: String { rawValue }

  /// The system's Reduce Motion changes the default: nothing starts by itself then.
  func effective(isMotionReduced: Bool) -> AnimatedImagesSetting {
    isMotionReduced ? .onTap : self
  }
}

/// What the user did to one image in the current view; it wins over the setting.
enum PlayChoice: Equatable {
  case unset
  case playing
  case paused

  func wantsToMove(_ setting: AnimatedImagesSetting) -> Bool {
    switch self {
    case .playing: return true
    case .paused: return false
    case .unset: return setting == .autoplay
    }
  }
}

/// What the media type says about motion.
enum AnimationKind {
  /// Largest animated file a tile fetches by itself; a bigger one moves in the viewer (as on the web).
  static let maxAutomaticBytes: Int64 = 16 * 1024 * 1024
  /// The longest side of a viewer's frames: each one is decoded as it comes, so less than a still
  /// picture's limit.
  static let viewerMaxPixels: CGFloat = 2048
  /// A file over these isn't animated: it reaches the decoder from others, without a tap.
  static let maxFramePixels = 16_000_000
  static let maxFrames = 3000
  /// What tiles fetch by themselves per run of the app; after it a tap fetches, so a channel can't
  /// fill the cache.
  static let maxAutomaticBytesPerRun: Int64 = 256 * 1024 * 1024
  /// How long a tile has to stay on screen before it fetches by itself: fast scrolling fetches nothing.
  static let fetchSettle: Duration = .milliseconds(300)

  /// Formats that always move: the tile offers play and fetches the file when it is to move.
  static func isAnimation(_ mediaType: String) -> Bool {
    ["image/gif", "image/apng"].contains(essence(mediaType))
  }

  /// Formats that may move: only the decoder can tell, once the file is here.
  static func mayMove(_ mediaType: String) -> Bool {
    isAnimation(mediaType) || ["image/webp", "image/avif"].contains(essence(mediaType))
  }

  /// The short label on a still animation, like the other platforms': GIF, WEBP, APNG, AVIF.
  static func label(_ mediaType: String) -> String {
    (essence(mediaType).split(separator: "/").last.map(String.init) ?? "").uppercased()
  }

  private static func essence(_ mediaType: String) -> String {
    (mediaType.split(separator: ";").first.map(String.init) ?? "")
      .trimmingCharacters(in: .whitespaces).lowercased()
  }
}

/// What tiles may still fetch by themselves.
struct AutoFetchBudget {
  let maxBytes: Int64
  private var spentBytes: Int64 = 0

  init(maxBytes: Int64) {
    self.maxBytes = maxBytes
  }

  /// Takes `bytes` from the budget; `false` when they don't fit, and then nothing is taken.
  mutating func take(_ bytes: Int64) -> Bool {
    guard spentBytes + bytes <= maxBytes else { return false }
    spentBytes += bytes
    return true
  }

  @MainActor static var shared = AutoFetchBudget(maxBytes: AnimationKind.maxAutomaticBytesPerRun)
}

/// Reads the frames of an animated file with ImageIO, one at a time and scaled down while decoding:
/// memory holds the frame on screen, whatever the file's length.
final class AnimationFrames: @unchecked Sendable {
  /// Delays this short mean "as fast as you can" in old files; browsers slow them down the same way.
  private static let shortestDelay: TimeInterval = 0.02
  private static let delayOfShortFrames: TimeInterval = 0.1

  private let source: CGImageSource
  private let maxPixels: CGFloat
  let count: Int
  /// How many times the file plays; `nil` is forever.
  let maybeLoops: Int?

  init?(path: String, maxPixels: CGFloat) {
    let url = URL(fileURLWithPath: path)
    guard let source = CGImageSourceCreateWithURL(url as CFURL, [kCGImageSourceShouldCache: false] as CFDictionary)
    else { return nil }
    self.source = source
    self.maxPixels = maxPixels
    count = CGImageSourceGetCount(source)
    let first = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any] ?? [:]
    guard Self.isWithinLimits(properties: first, count: count) else { return nil }
    let container = CGImageSourceCopyProperties(source, nil) as? [CFString: Any] ?? [:]
    maybeLoops = Self.loops(in: container)
  }

  /// Reads the size from the file's metadata, before any frame is decoded. No size counts as too large.
  static func isWithinLimits(properties: [CFString: Any], count: Int) -> Bool {
    guard count <= AnimationKind.maxFrames,
      let width = properties[kCGImagePropertyPixelWidth] as? Int,
      let height = properties[kCGImagePropertyPixelHeight] as? Int,
      width > 0, height > 0,
      width <= AnimationKind.maxFramePixels / height
    else { return false }
    return true
  }

  func frame(at index: Int) -> (image: UIImage, delay: TimeInterval)? {
    // A later frame can name another size than the first.
    let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any] ?? [:]
    guard Self.isWithinLimits(properties: properties, count: count) else { return nil }
    let options: [CFString: Any] = [
      kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true,
      kCGImageSourceShouldCacheImmediately: true,
      kCGImageSourceThumbnailMaxPixelSize: maxPixels,
    ]
    guard let image = CGImageSourceCreateThumbnailAtIndex(source, index, options as CFDictionary) else { return nil }
    return (UIImage(cgImage: image), Self.delay(in: properties))
  }

  /// A frame's delay from its format's dictionary; the unclamped value when there is one.
  static func delay(in properties: [CFString: Any]) -> TimeInterval {
    let formats: [(CFString, CFString, CFString)] = [
      (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFUnclampedDelayTime, kCGImagePropertyGIFDelayTime),
      (kCGImagePropertyPNGDictionary, kCGImagePropertyAPNGUnclampedDelayTime, kCGImagePropertyAPNGDelayTime),
      (kCGImagePropertyWebPDictionary, kCGImagePropertyWebPUnclampedDelayTime, kCGImagePropertyWebPDelayTime),
      (kCGImagePropertyHEICSDictionary, kCGImagePropertyHEICSUnclampedDelayTime, kCGImagePropertyHEICSDelayTime),
    ]
    for (dictionary, unclamped, clamped) in formats {
      guard let values = properties[dictionary] as? [CFString: Any] else { continue }
      let delay = (values[unclamped] as? Double) ?? (values[clamped] as? Double) ?? 0
      return delay < shortestDelay ? delayOfShortFrames : delay
    }
    return delayOfShortFrames
  }

  /// The container's loop count, where 0 means forever. A GIF without one plays once.
  static func loops(in container: [CFString: Any]) -> Int? {
    let formats: [(CFString, CFString)] = [
      (kCGImagePropertyGIFDictionary, kCGImagePropertyGIFLoopCount),
      (kCGImagePropertyPNGDictionary, kCGImagePropertyAPNGLoopCount),
      (kCGImagePropertyWebPDictionary, kCGImagePropertyWebPLoopCount),
      (kCGImagePropertyHEICSDictionary, kCGImagePropertyHEICSLoopCount),
    ]
    for (dictionary, loopCount) in formats {
      guard let values = container[dictionary] as? [CFString: Any] else { continue }
      let loops = (values[loopCount] as? Int) ?? 1
      return loops == 0 ? nil : loops
    }
    return nil
  }
}

/// One image's animation: the frame on screen and where it stands. Only runs inside `run`, which a
/// view's `.task` drives, so it ends when the view leaves the screen or stops wanting it.
@MainActor
@Observable
final class AnimationPlayer {
  private(set) var maybeFrame: UIImage?
  /// The file holds more than one frame; `nil` until it was read.
  private(set) var maybeMoves: Bool?
  private(set) var isRunning = false
  private(set) var hasEnded = false
  private var index = 0
  private var finishedLoops = 0

  /// Reads the header only: whether the file moves.
  func probe(path: String) async {
    let count = await Task.detached(priority: .utility) {
      AnimationFrames(path: path, maxPixels: 1)?.count ?? 0
    }.value
    maybeMoves = count > 1
  }

  /// Shows frames until cancelled or the file's loops are done. Paused, the last frame stays.
  func run(path: String, maxPixels: CGFloat) async {
    guard let frames = await Task.detached(priority: .utility, operation: {
      // The core keeps the most recently used downloads when it trims its folder.
      try? FileManager.default.setAttributes([.modificationDate: Date()], ofItemAtPath: path)
      return AnimationFrames(path: path, maxPixels: maxPixels)
    }).value else {
      maybeMoves = false
      return
    }
    maybeMoves = frames.count > 1
    guard frames.count > 1 else { return }
    hasEnded = false
    isRunning = true
    defer { isRunning = false }
    while !Task.isCancelled {
      let at = index
      let maybeNext = await Task.detached(priority: .utility) { frames.frame(at: at) }.value
      guard let next = maybeNext, !Task.isCancelled else { return }
      maybeFrame = next.image
      try? await Task.sleep(for: .seconds(next.delay))
      guard !Task.isCancelled else { return }
      index += 1
      if index >= frames.count {
        index = 0
        finishedLoops += 1
        if let loops = frames.maybeLoops, finishedLoops >= loops {
          finishedLoops = 0
          hasEnded = true
          return
        }
      }
    }
  }
}

/// A conversation is covered (the image viewer is open): its images don't move behind it.
private struct AnimationsCoveredKey: EnvironmentKey {
  static let defaultValue = false
}

extension EnvironmentValues {
  var areAnimationsCovered: Bool {
    get { self[AnimationsCoveredKey.self] }
    set { self[AnimationsCoveredKey.self] = newValue }
  }
}

/// The button that stops or starts an animated image: a 44 pt target, over the picture.
struct AnimationButton: View {
  let isMoving: Bool
  let action: () -> Void

  var body: some View {
    Button(action: action) {
      Image(systemName: isMoving ? "pause.fill" : "play.fill")
        .font(.footnote.weight(.semibold))
        .foregroundStyle(.white)
        .frame(width: 32, height: 32)
        .background(.ultraThinMaterial, in: Circle())
        .environment(\.colorScheme, .dark)
        .frame(width: 44, height: 44)
        .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityLabel(isMoving ? Text("animation_pause") : Text("animation_play"))
    .accessibilityValue(isMoving ? Text("animation_state_playing") : Text("animation_state_paused"))
  }
}

/// Says there is motion behind a still frame.
struct AnimationBadge: View {
  let text: Text

  var body: some View {
    text
      .font(.caption2.weight(.bold))
      .foregroundStyle(.white)
      .padding(.horizontal, 6).padding(.vertical, 1)
      .background(Color.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 4, style: .continuous))
      .padding(6)
      .accessibilityHidden(true)
  }
}
