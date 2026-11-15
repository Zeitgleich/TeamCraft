import UIKit

/// A picture the user pasted into the composer, or the keyboard inserted (a GIF, a sticker): its
/// bytes in the format it came in, so a GIF stays animated (issue #2053).
struct PastedImage: Equatable {
  let data: Data
  let fileExtension: String

  /// The formats read as they are, best first.
  static let formats: [(identifier: String, fileExtension: String)] = [
    ("com.compuserve.gif", "gif"), ("public.png", "png"), ("public.jpeg", "jpg"), ("public.heic", "heic"),
  ]

  /// The file name an attachment of this picture gets.
  func fileName(index: Int, now: Date = .now) -> String {
    "Pasted-\(Int(now.timeIntervalSince1970))-\(index + 1).\(fileExtension)"
  }

  /// The pictures on `pasteboard`, one for each item; none when it holds no picture.
  static func read(from pasteboard: UIPasteboard) -> [PastedImage] {
    var found: [PastedImage] = []
    for item in pasteboard.items {
      for format in formats {
        guard let value = item[format.identifier] else { continue }
        let data = (value as? Data) ?? (value as? UIImage)?.pngData()
        if let data, !data.isEmpty {
          found.append(PastedImage(data: data, fileExtension: format.fileExtension))
          break
        }
      }
    }
    if found.isEmpty, let images = pasteboard.images {
      found = images.compactMap { $0.jpegData(compressionQuality: 0.85) }.map { PastedImage(data: $0, fileExtension: "jpg") }
    }
    return found
  }
}
