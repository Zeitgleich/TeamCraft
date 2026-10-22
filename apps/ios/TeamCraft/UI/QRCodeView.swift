import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit

/// A QR code of `text` (the link code another device scans).
struct QRCodeView: View {
  let text: String

  var body: some View {
    if let image = Self.image(for: text) {
      Image(uiImage: image)
        .interpolation(.none)
        .resizable()
        .scaledToFit()
        .accessibilityLabel(Text("link_new_title"))
    }
  }

  static func image(for text: String) -> UIImage? {
    let filter = CIFilter.qrCodeGenerator()
    filter.message = Data(text.utf8)
    filter.correctionLevel = "M"
    guard let output = filter.outputImage else { return nil }
    let scaled = output.transformed(by: CGAffineTransform(scaleX: 10, y: 10))
    guard let cgImage = CIContext().createCGImage(scaled, from: scaled.extent) else { return nil }
    return UIImage(cgImage: cgImage)
  }
}
