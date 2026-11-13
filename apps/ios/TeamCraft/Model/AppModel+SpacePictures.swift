import ImageIO
import UIKit

/// What loading a space's picture gave.
enum SpacePictureLoad {
  case picture(UIImage)
  /// No picture, or one this device can't decode.
  case none
  /// The download failed for a reason that passes: keep what is shown and ask in the next round.
  case later
}

/// The pictures of spaces (#284). Only a space on screen asks for its picture; it comes from the
/// disk cache when that holds the version the space list names, else from the core, and a missing
/// download shows the letter tile as before. A download that failed during an outage is asked for
/// again in the next round of `pictureRetries` (#1617).
extension AppModel {
  private static let spacePictureMaxBytes: UInt32 = 1_048_576
  private static let spacePicturePixels = 192

  /// The picture of the space in `version`, none, or `.later` when the download failed for a
  /// reason that passes.
  func spacePicture(spaceId: String, version: String) async -> SpacePictureLoad {
    if let stored = await spacePictureStore.data(spaceId: spaceId, version: version) {
      return Self.thumbnail(stored).map(SpacePictureLoad.picture) ?? .none
    }
    guard let core = try? core() else { return .none }
    do {
      let maybeBytes = try await core.spacePicture(spaceId: spaceId, maxBytes: Self.spacePictureMaxBytes)
      pictureRetries.succeeded()
      guard let bytes = maybeBytes else {
        await spacePictureStore.remove(spaceId: spaceId)
        return .none
      }
      guard let image = Self.thumbnail(bytes) else { return .none }
      await spacePictureStore.save(bytes, spaceId: spaceId, version: version)
      return .picture(image)
    } catch {
      // Offline or a storage outage: asked for again in the next round. Refused (not found,
      // forbidden, signed out): not by itself, the view asks again at the next look.
      guard PictureRetries.isTransient(error) else { return .none }
      pictureRetries.failed()
      return .later
    }
  }

  /// A space that has no picture any more keeps no file.
  func forgetSpacePicture(spaceId: String) async {
    await spacePictureStore.remove(spaceId: spaceId)
  }

  func clearSpacePictures() async {
    await spacePictureStore.clear()
  }

  /// Decodes at the size the lists show it, whatever the picture's own size.
  private static func thumbnail(_ data: Data) -> UIImage? {
    guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
    let options: [CFString: Any] = [
      kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true,
      kCGImageSourceThumbnailMaxPixelSize: spacePicturePixels,
    ]
    return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary).map(UIImage.init(cgImage:))
  }
}
