import Foundation
import ImageIO
import UniformTypeIdentifiers
import UserNotifications

/// The picture of a photo or video message as a notification attachment: the sender's small
/// preview the core decrypted (`PushNotification.maybePicture`), decoded here and encoded again, so
/// the system only ever decodes a picture the extension made, never the sender's bytes. The system
/// moves the file into its own store and deletes it with the notification; the lock screen hides it
/// as it hides the text ("Show Previews"). Pictures follow the text: the core leaves them out where
/// the policy or the device setting hides the content.
enum NotificationPictureAttachment {
  /// Senders make previews of at most 640 px; a picture larger than this is no preview.
  private static let maxSidePixels = 4096
  /// About what a notification shows of a picture at most; keeps the extension's memory low.
  private static let shownSidePixels = 1024
  private static let jpegQuality = 0.85

  /// The attachment, or `nil` when the picture doesn't decode, is out of bounds or can't be written.
  static func make(from picture: NotificationPicture) -> UNNotificationAttachment? {
    guard let image = decoded(picture.bytes) else { return nil }
    let hasAlpha = Self.hasAlpha(image)
    let type: UTType = hasAlpha ? .png : .jpeg
    let url = FileManager.default.temporaryDirectory
      .appendingPathComponent(UUID().uuidString)
      .appendingPathExtension(hasAlpha ? "png" : "jpg")
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil) else {
      return nil
    }
    let properties = hasAlpha ? nil : [kCGImageDestinationLossyCompressionQuality: jpegQuality] as CFDictionary
    CGImageDestinationAddImage(destination, image, properties)
    guard CGImageDestinationFinalize(destination) else {
      try? FileManager.default.removeItem(at: url)
      return nil
    }
    do {
      return try UNNotificationAttachment(
        identifier: "picture", url: url, options: [UNNotificationAttachmentOptionsTypeHintKey: type.identifier])
    } catch {
      // The notification shows the text alone.
      try? FileManager.default.removeItem(at: url)
      return nil
    }
  }

  private static func decoded(_ bytes: Data) -> CGImage? {
    let options = [kCGImageSourceShouldCache: false] as CFDictionary
    guard let source = CGImageSourceCreateWithData(bytes as CFData, options),
      let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
      let width = properties[kCGImagePropertyPixelWidth] as? Int,
      let height = properties[kCGImagePropertyPixelHeight] as? Int,
      width > 0, height > 0, width <= maxSidePixels, height <= maxSidePixels
    else { return nil }
    let thumbnail: [CFString: Any] = [
      kCGImageSourceCreateThumbnailFromImageAlways: true,
      kCGImageSourceCreateThumbnailWithTransform: true,
      kCGImageSourceThumbnailMaxPixelSize: min(max(width, height), shownSidePixels),
      kCGImageSourceShouldCacheImmediately: true,
    ]
    return CGImageSourceCreateThumbnailAtIndex(source, 0, thumbnail as CFDictionary)
  }

  private static func hasAlpha(_ image: CGImage) -> Bool {
    switch image.alphaInfo {
    case .none, .noneSkipFirst, .noneSkipLast: false
    default: true
    }
  }
}
