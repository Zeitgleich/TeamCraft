import Foundation
import ImageIO
import UniformTypeIdentifiers

/// HEIC and HEIF pictures turned into JPEG when they are picked (#1588). iPhones store photos as
/// HEIC, which the client core can't decode: no thumbnail, no size, and many receivers (Android,
/// Linux, Windows, the web) can't show the picture. ImageIO re-encodes the still picture; its
/// metadata comes along, the orientation tag included, except the GPS position.
enum HeicConversion {
  /// JPEG quality of a converted photo: close to the original, about the size of the HEIC times two.
  static let jpegQuality = 0.9

  /// Whether the file at `url` is a single HEIC or HEIF picture. Image sequences (`.heics`, bursts)
  /// stay as they are: a JPEG would keep only their first picture.
  static func isConvertible(_ url: URL) -> Bool {
    guard let byName = UTType(filenameExtension: url.pathExtension), byName.conforms(to: .image),
      let source = CGImageSourceCreateWithURL(url as CFURL, nil),
      let identifier = CGImageSourceGetType(source) as String?,
      let type = UTType(identifier)
    else { return false }
    return isHeif(type) && CGImageSourceGetCount(source) == 1
  }

  /// The name of the converted file: the extension becomes `.jpg` (`IMG_0001.HEIC` is
  /// `IMG_0001.jpg`).
  static func jpegName(for name: String) -> String {
    let stem = (name as NSString).deletingPathExtension
    return (stem.isEmpty ? "photo" : stem) + ".jpg"
  }

  /// Writes the picture at `source` as a JPEG to `target`. The pixels stay as stored and the EXIF
  /// orientation goes along (the core and receivers turn the picture by it); the GPS position is
  /// dropped. `false` when ImageIO can't read or write it; `target` may then hold a partial file.
  static func writeJpeg(from source: URL, to target: URL) -> Bool {
    guard let picture = CGImageSourceCreateWithURL(source as CFURL, nil),
      CGImageSourceGetCount(picture) == 1,
      let destination = CGImageDestinationCreateWithURL(
        target as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
    else { return false }
    let properties: [CFString: Any] = [
      kCGImageDestinationLossyCompressionQuality: jpegQuality,
      // kCFNull removes a property the source has.
      kCGImagePropertyGPSDictionary: kCFNull as Any,
    ]
    CGImageDestinationAddImageFromSource(destination, picture, 0, properties as CFDictionary)
    return CGImageDestinationFinalize(destination)
  }

  private static func isHeif(_ type: UTType) -> Bool {
    type.conforms(to: .heic) || type.conforms(to: .heif)
  }
}
