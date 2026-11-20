import ImageIO
import UniformTypeIdentifiers
import XCTest

@testable import TeamCraft

final class LivePhotoBundleTests: XCTestCase {
  private var folder: URL!

  override func setUpWithError() throws {
    folder = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
  }

  override func tearDownWithError() throws {
    try? FileManager.default.removeItem(at: folder)
  }

  func testALivePhotoBundleGoesAsItsStillPictureOnly() throws {
    let bundle = try makeBundle(stillName: "IMG_0001.JPG", type: .jpeg)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importPickedMedia(PickedMedia(url: bundle))

    XCTAssertEqual(file.fileName, "IMG_0001.JPG")
    XCTAssertEqual(file.mediaType, "image/jpeg")
    let source = try XCTUnwrap(CGImageSourceCreateWithURL(URL(fileURLWithPath: file.path) as CFURL, nil))
    XCTAssertEqual(CGImageSourceGetType(source) as String?, UTType.jpeg.identifier)
    // One file in the uploads folder: no motion video, no folder.
    let left = try FileManager.default.contentsOfDirectory(atPath: (file.path as NSString).deletingLastPathComponent)
    XCTAssertEqual(left, ["IMG_0001.JPG"])
    XCTAssertFalse(FileManager.default.fileExists(atPath: bundle.path))
  }

  func testAHeicInTheBundleBecomesAJpeg() throws {
    let types = CGImageDestinationCopyTypeIdentifiers() as? [String] ?? []
    guard types.contains(UTType.heic.identifier) else { throw XCTSkip("no HEIC encoder here") }
    let bundle = try makeBundle(stillName: "IMG_0002.HEIC", type: .heic)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importPickedMedia(PickedMedia(url: bundle))

    XCTAssertEqual(file.fileName, "IMG_0002.jpg")
    XCTAssertEqual(file.mediaType, "image/jpeg")
  }

  func testABundleWithoutAPictureIsRefusedWithAMessage() throws {
    let bundle = folder.appendingPathComponent("video.pvt", isDirectory: true)
    try FileManager.default.createDirectory(at: bundle, withIntermediateDirectories: true)
    try Data("motion".utf8).write(to: bundle.appendingPathComponent("IMG_0003.MOV"))
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    XCTAssertThrowsError(try importer.importPickedMedia(PickedMedia(url: bundle))) { error in
      XCTAssertEqual(error as? AttachmentError, .unsupportedBundle)
    }
  }

  func testAPlainVideoIsNotTouched() throws {
    let movie = folder.appendingPathComponent("clip.mov")
    try Data("motion".utf8).write(to: movie)
    let importer = AttachmentImporter(directory: folder.appendingPathComponent("uploads"), maybeLimitBytes: nil)

    let file = try importer.importPickedMedia(PickedMedia(url: movie))

    XCTAssertEqual(file.fileName, "clip.mov")
    XCTAssertEqual(file.mediaType, "video/quicktime")
  }

  /// A `.pvt`-style folder: the still picture next to the motion video.
  private func makeBundle(stillName: String, type: UTType) throws -> URL {
    let bundle = folder.appendingPathComponent("photo.pvt", isDirectory: true)
    try FileManager.default.createDirectory(at: bundle, withIntermediateDirectories: true)
    try Data("motion".utf8).write(to: bundle.appendingPathComponent("IMG_0001.MOV"))
    try writePicture(type: type, to: bundle.appendingPathComponent(stillName))
    return bundle
  }

  private func writePicture(type: UTType, to url: URL) throws {
    let context = try XCTUnwrap(
      CGContext(
        data: nil, width: 64, height: 32, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue))
    context.setFillColor(red: 0.9, green: 0.3, blue: 0.1, alpha: 1)
    context.fill(CGRect(x: 0, y: 0, width: 64, height: 32))
    let image = try XCTUnwrap(context.makeImage())
    let destination = try XCTUnwrap(
      CGImageDestinationCreateWithURL(url as CFURL, type.identifier as CFString, 1, nil))
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { throw XCTSkip("\(type.identifier) not written here") }
  }
}
