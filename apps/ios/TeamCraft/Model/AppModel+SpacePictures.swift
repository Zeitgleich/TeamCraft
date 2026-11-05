import ImageIO
import UIKit

/// The pictures of spaces (#284). Only a space on screen asks for its picture; it comes from the
/// disk cache when that holds the version the space list names, else from the core, and a failed
/// or missing download shows the letter tile as before.
extension AppModel {
  private static let spacePictureMaxBytes: UInt32 = 1_048_576
  private static let spacePicturePixels = 192

  /// The picture of the space in `version`; `nil` when there is none or it can't be loaded now.
  func spacePicture(spaceId: String, version: String) async -> UIImage? {
    if let stored = await spacePictureStore.data(spaceId: spaceId, version: version) {
      return Self.thumbnail(stored)
    }
    guard let core = try? core() else { return nil }
    do {
      guard let bytes = try await core.spacePicture(spaceId: spaceId, maxBytes: Self.spacePictureMaxBytes) else {
        await spacePictureStore.remove(spaceId: spaceId)
        return nil
      }
      guard let image = Self.thumbnail(bytes) else { return nil }
      await spacePictureStore.save(bytes, spaceId: spaceId, version: version)
      return image
    } catch {
      // Offline or refused: no picture for now, the view asks again when the connection changes.
      return nil
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
