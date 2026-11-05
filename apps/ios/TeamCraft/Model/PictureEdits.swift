import Foundation
import UIKit

/// Applies a crop and quarter turns to a picked picture before it is sent. The result is a new
/// picture: its metadata (camera, place) isn't carried over. Same rules as Android's.
enum PictureEdits {
  /// Longest side of an edited picture: sharp in the viewer, within a phone's memory.
  static let maxSidePx: CGFloat = 3072

  /// Media types the editor opens; animations and other formats are sent as they are.
  static func isEditable(_ mediaType: String) -> Bool {
    ["image/jpeg", "image/png", "image/heic", "image/heif", "image/webp"].contains(mediaType)
  }

  /// The picture upright (its orientation applied), at most `maxSide` on its longer side.
  static func upright(path: String, maxSide: CGFloat) -> UIImage? {
    guard let image = UIImage(contentsOfFile: path) else { return nil }
    let scale = min(1, maxSide / max(image.size.width, image.size.height))
    let size = CGSize(width: (image.size.width * scale).rounded(), height: (image.size.height * scale).rounded())
    let format = UIGraphicsImageRendererFormat.default()
    format.scale = 1
    return UIGraphicsImageRenderer(size: size, format: format).image { _ in
      image.draw(in: CGRect(origin: .zero, size: size))
    }
  }

  /// `image` turned `quarterTurnsLeft` quarters to the left.
  static func turned(_ image: UIImage, quarterTurnsLeft: Int) -> UIImage {
    let turns = (quarterTurnsLeft % 4 + 4) % 4
    guard turns != 0 else { return image }
    let isSideways = turns % 2 == 1
    let size = isSideways ? CGSize(width: image.size.height, height: image.size.width) : image.size
    let format = UIGraphicsImageRendererFormat.default()
    format.scale = 1
    return UIGraphicsImageRenderer(size: size, format: format).image { context in
      let cg = context.cgContext
      cg.translateBy(x: size.width / 2, y: size.height / 2)
      // Counter-clockwise on screen: UIKit's y axis points down, so the angle is negative.
      cg.rotate(by: -CGFloat(turns) * .pi / 2)
      image.draw(in: CGRect(x: -image.size.width / 2, y: -image.size.height / 2, width: image.size.width, height: image.size.height))
    }
  }

  /// Writes the edited picture next to the original and returns it as the file to send; `nil` when
  /// the picture can't be read.
  static func render(_ file: PickedFile, quarterTurnsLeft: Int, frame: CropFrame) -> PickedFile? {
    guard let source = upright(path: file.path, maxSide: maxSidePx), let cgSource = source.cgImage else { return nil }
    let original = frame.inOriginal(quarterTurnsLeft: quarterTurnsLeft)
    let width = CGFloat(cgSource.width)
    let height = CGFloat(cgSource.height)
    let rect = CGRect(
      x: (original.left * width).rounded(),
      y: (original.top * height).rounded(),
      width: max(1, (original.width * width).rounded()),
      height: max(1, (original.height * height).rounded()))
    guard let cropped = cgSource.cropping(to: rect) else { return nil }
    let edited = turned(UIImage(cgImage: cropped), quarterTurnsLeft: quarterTurnsLeft)
    let isPng = file.mediaType == "image/png"
    guard let data = isPng ? edited.pngData() : edited.jpegData(compressionQuality: 0.9) else { return nil }
    let folder = URL(fileURLWithPath: file.path).deletingLastPathComponent()
    let base = (file.fileName as NSString).deletingPathExtension
    let name = isPng ? file.fileName : "\(base.isEmpty ? "picture" : base).jpg"
    let target = folder.appendingPathComponent("edited-\(UUID().uuidString.prefix(8))-\(name)")
    do {
      try data.write(to: target, options: .atomic)
    } catch {
      return nil
    }
    return PickedFile(fileName: name, mediaType: isPng ? "image/png" : "image/jpeg", path: target.path)
  }
}
