import AVFoundation
import Photos
import PhotosUI
import SwiftUI

/// The motion of a Live Photo picked in the photo picker (#2181): its paired video, written out of
/// the Photos framework and converted from HEVC to H.264 MP4 (every browser and phone plays that),
/// staged beside the picture so the core sends it as the picture's motion part.
enum LivePhotoMotion {
  /// The user's choice: Live Photos go with their motion unless turned off (less data).
  static let settingKey = "send_live_motion"

  /// Whether the user sends Live Photos with their motion (on until turned off).
  static var isSendingMotion: Bool {
    UserDefaults.standard.object(forKey: settingKey) as? Bool ?? true
  }

  /// The motion for `picture`, picked as `item`; `nil` when it is no Live Photo, the user or the
  /// server doesn't send motion, or reading or converting fails (the picture then goes still).
  ///
  /// A clip already over the limit before converting is not converted: it goes along as it is, and
  /// the core sends the picture alone and shows the sender that the motion was not sent.
  static func motion(of item: PhotosPickerItem, beside picture: PickedFile, limits: MotionLimitsItem) async -> PickedMotion? {
    guard isSendingMotion, limits.isAllowed, picture.mediaType.hasPrefix("image/"),
      let livePhoto = try? await item.loadTransferable(type: PHLivePhoto.self)
    else { return nil }
    let folder = URL(fileURLWithPath: picture.path).deletingLastPathComponent()
    let stem = (picture.fileName as NSString).deletingPathExtension
    let source = folder.appendingPathComponent("\(stem)-motion.mov")
    guard await writePairedVideo(of: livePhoto, to: source) else { return nil }
    return await prepared(PickedMotion(path: source.path, mediaType: "video/quicktime"), limits: limits)
  }

  /// A Live Photo's motion as it came (a QuickTime clip, picked or taken out of a shared bundle)
  /// made ready to send: converted to H.264 MP4 beside it. `nil`, and the clip deleted, when the
  /// user or the server doesn't send motion or the conversion fails. A clip already over the limit
  /// is not converted: it goes along as it is, and the core sends the picture alone and shows the
  /// sender that the motion was not sent.
  static func prepared(_ motion: PickedMotion, limits: MotionLimitsItem) async -> PickedMotion? {
    let source = URL(fileURLWithPath: motion.path)
    guard isSendingMotion, limits.isAllowed else {
      try? FileManager.default.removeItem(at: source)
      return nil
    }
    if fileSize(source) > Int64(limits.maxBytes) { return motion }
    let target = source.deletingPathExtension().appendingPathExtension("mp4")
    let maybeDuration = await exportH264(from: source, to: target)
    if target != source { try? FileManager.default.removeItem(at: source) }
    guard let duration = maybeDuration else {
      try? FileManager.default.removeItem(at: target)
      return nil
    }
    return PickedMotion(path: target.path, mediaType: "video/mp4", maybeDurationMs: duration)
  }

  /// Writes the Live Photo's paired video to `url`; false when it has none or writing fails.
  private static func writePairedVideo(of livePhoto: PHLivePhoto, to url: URL) async -> Bool {
    let resources = PHAssetResource.assetResources(for: livePhoto)
    guard let video = resources.first(where: { $0.type == .pairedVideo }) else { return false }
    try? FileManager.default.removeItem(at: url)
    let options = PHAssetResourceRequestOptions()
    // A Live Photo kept in iCloud only: the clip comes down like the picture did.
    options.isNetworkAccessAllowed = true
    return await withCheckedContinuation { continuation in
      PHAssetResourceManager.default().writeData(for: video, toFile: url, options: options) { error in
        continuation.resume(returning: error == nil)
      }
    }
  }

  /// Converts the clip at `source` into an H.264 MP4 at `target` (at most 1920×1080, as the
  /// camera's clip is); its length in milliseconds, or `nil` when the conversion fails.
  static func exportH264(from source: URL, to target: URL) async -> Int64? {
    let asset = AVURLAsset(url: source)
    guard let session = AVAssetExportSession(asset: asset, presetName: AVAssetExportPreset1920x1080) else { return nil }
    try? FileManager.default.removeItem(at: target)
    session.shouldOptimizeForNetworkUse = true
    do {
      if #available(iOS 18, *) {
        try await session.export(to: target, as: .mp4)
      } else {
        session.outputURL = target
        session.outputFileType = .mp4
        await session.export()
        guard session.status == .completed else { return nil }
      }
    } catch {
      return nil
    }
    guard let duration = try? await AVURLAsset(url: target).load(.duration), duration.isNumeric else { return nil }
    return Int64((duration.seconds * 1000).rounded())
  }

  private static func fileSize(_ url: URL) -> Int64 {
    (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map { Int64($0) } ?? 0
  }
}
