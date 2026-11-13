import ImageIO
import UniformTypeIdentifiers
import XCTest

@testable import TeamCraft

final class HeicConversionTests: XCTestCase {
  private var folder: URL!

  override func setUpWithError() throws {
    folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
  }

  override func tearDownWithError() throws {
    try? FileManager.default.removeItem(at: folder)
  }

  func testTheNameGetsAJpgExtension() {
    XCTAssertEqual(HeicConversion.jpegName(for: "IMG_0001.HEIC"), "IMG_0001.jpg")
    XCTAssertEqual(HeicConversion.jpegName(for: "holiday.photo.heif"), "holiday.photo.jpg")
    XCTAssertEqual(HeicConversion.jpegName(for: "plain"), "plain.jpg")
  }

  func testAPickedHeicGoesAsJpegWithItsOrientationAndWithoutItsPosition() throws {
    let picked = try heic(named: "IMG_0001.HEIC", orientation: 6)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importPickedMedia(PickedMedia(url: picked))

    XCTAssertEqual(file.fileName, "IMG_0001.jpg")
    XCTAssertEqual(file.mediaType, "image/jpeg")
    let source = try XCTUnwrap(CGImageSourceCreateWithURL(URL(fileURLWithPath: file.path) as CFURL, nil))
    XCTAssertEqual(CGImageSourceGetType(source) as String?, UTType.jpeg.identifier)
    let properties = try XCTUnwrap(CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any])
    XCTAssertEqual(properties[kCGImagePropertyOrientation] as? Int, 6)
    XCTAssertEqual(properties[kCGImagePropertyPixelWidth] as? Int, 64)
    XCTAssertEqual(properties[kCGImagePropertyPixelHeight] as? Int, 32)
    XCTAssertNil(properties[kCGImagePropertyGPSDictionary])
    // Only the JPEG is left in the uploads folder.
    let left = try FileManager.default.contentsOfDirectory(atPath: (file.path as NSString).deletingLastPathComponent)
    XCTAssertEqual(left, ["IMG_0001.jpg"])
  }

  func testADocumentHeicIsConvertedAndTheOriginalStays() throws {
    let picked = try heic(named: "scan.heic", orientation: 1)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importDocument(at: picked)

    XCTAssertEqual(file.fileName, "scan.jpg")
    XCTAssertEqual(file.mediaType, "image/jpeg")
    XCTAssertTrue(FileManager.default.fileExists(atPath: picked.path))
  }

  func testOtherPicturesStayAsTheyAre() throws {
    let png = folder.appendingPathComponent("drawing.png")
    try write(type: .png, to: png, orientation: 1)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importDocument(at: png)

    XCTAssertEqual(file.fileName, "drawing.png")
    XCTAssertEqual(file.mediaType, "image/png")
  }

  /// A 64 × 32 HEIC with the orientation and a GPS position, as a phone camera writes it.
  private func heic(named name: String, orientation: Int) throws -> URL {
    let url = folder.appendingPathComponent(name)
    let types = CGImageDestinationCopyTypeIdentifiers() as? [String] ?? []
    guard types.contains(UTType.heic.identifier) else { throw XCTSkip("no HEIC encoder here") }
    try write(type: .heic, to: url, orientation: orientation)
    return url
  }

  private func write(type: UTType, to url: URL, orientation: Int) throws {
    let context = try XCTUnwrap(
      CGContext(
        data: nil, width: 64, height: 32, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
    context.setFillColor(red: 0.9, green: 0.3, blue: 0.1, alpha: 1)
    context.fill(CGRect(x: 0, y: 0, width: 64, height: 32))
    let image = try XCTUnwrap(context.makeImage())
    let destination = try XCTUnwrap(
      CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil))
    let properties: [CFString: Any] = [
      kCGImagePropertyOrientation: orientation,
      kCGImagePropertyGPSDictionary: [
        kCGImagePropertyGPSLatitude: 52.52, kCGImagePropertyGPSLatitudeRef: "N",
        kCGImagePropertyGPSLongitude: 13.405, kCGImagePropertyGPSLongitudeRef: "E",
      ],
    ]
    CGImageDestinationAddImage(destination, image, properties as CFDictionary)
    guard CGImageDestinationFinalize(destination) else { throw XCTSkip("\(type.identifier) not written here") }
  }
}
