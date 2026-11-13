import Foundation
import SwiftUI
import UniformTypeIdentifiers

/// Why a picked file isn't attached.
enum AttachmentError: Error, Equatable {
  case tooLarge(limitBytes: Int64)
  case unreadable
  /// The core refused the message with its files.
  case sendFailed
  /// A file of the draft was held by the server too long and is gone (#1220): nothing is sent.
  case draftFilesExpired
}

/// Copies files the user picked (photo library, Files app) into the app's uploads folder, so the
/// core can read them by path while it encrypts and uploads them in chunks (the picker's own
/// temporary copy and security scope may be gone by then). HEIC photos become JPEG on the way
/// (`HeicConversion`).
struct AttachmentImporter {
  let directory: URL
  /// The server's upload limit, if known; larger files are refused before copying.
  var maybeLimitBytes: Int64?

  /// A file from the Files app (security-scoped URL).
  func importDocument(at url: URL) throws -> PickedFile {
    let isScoped = url.startAccessingSecurityScopedResource()
    defer { if isScoped { url.stopAccessingSecurityScopedResource() } }
    return try copy(from: url, move: false)
  }

  /// A photo or video the picker exported to a temporary file; that file is moved.
  func importPickedMedia(_ media: PickedMedia) throws -> PickedFile {
    try copy(from: media.url, move: true)
  }

  private func copy(from source: URL, move: Bool) throws -> PickedFile {
    let size = (try? source.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map { Int64($0) } ?? 0
    if let limit = maybeLimitBytes, size > limit { throw AttachmentError.tooLarge(limitBytes: limit) }
    let name = Self.safeName(source.lastPathComponent)
    // A folder per file keeps the original name while names never collide.
    let folder = directory.appendingPathComponent(UUID().uuidString, isDirectory: true)
    let target = folder.appendingPathComponent(name)
    do {
      try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
      if move {
        try FileManager.default.moveItem(at: source, to: target)
      } else {
        try FileManager.default.copyItem(at: source, to: target)
      }
    } catch {
      throw AttachmentError.unreadable
    }
    let file = compatiblePicture(at: target) ?? target
    return PickedFile(fileName: file.lastPathComponent, mediaType: Self.mediaType(of: file), path: file.path)
  }

  /// A HEIC or HEIF picture in the uploads folder as a JPEG next to it, which replaces it (#1588);
  /// `nil` for any other file, or when the conversion fails or the JPEG is over the upload limit
  /// (the picture then goes as it is). Only the app's own copy is replaced, never the original.
  private func compatiblePicture(at copy: URL) -> URL? {
    guard HeicConversion.isConvertible(copy) else { return nil }
    let jpeg = copy.deletingLastPathComponent()
      .appendingPathComponent(HeicConversion.jpegName(for: copy.lastPathComponent))
    // A HEIC already named `.jpg` would be read and written at once.
    guard jpeg.lastPathComponent != copy.lastPathComponent else { return nil }
    let isWritten = HeicConversion.writeJpeg(from: copy, to: jpeg)
    let maybeSize = (try? jpeg.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map { Int64($0) }
    guard isWritten, let size = maybeSize, size > 0, maybeLimitBytes.map({ size <= $0 }) ?? true else {
      try? FileManager.default.removeItem(at: jpeg)
      return nil
    }
    try? FileManager.default.removeItem(at: copy)
    return jpeg
  }

  /// The file name without path parts, never empty.
  static func safeName(_ name: String) -> String {
    let last = name.split(whereSeparator: { $0 == "/" || $0 == "\\" }).last.map(String.init) ?? ""
    return last.isEmpty || last == "." || last == ".." ? "file" : last
  }

  static func mediaType(of url: URL) -> String {
    UTType(filenameExtension: url.pathExtension)?.preferredMIMEType ?? "application/octet-stream"
  }
}

/// A photo or video from `PhotosPicker`, exported to a file without loading it into memory.
struct PickedMedia: Transferable {
  let url: URL

  static var transferRepresentation: some TransferRepresentation {
    FileRepresentation(contentType: .item) { media in
      SentTransferredFile(media.url)
    } importing: { received in
      let copy = FileManager.default.temporaryDirectory
        .appendingPathComponent(UUID().uuidString, isDirectory: true)
      try FileManager.default.createDirectory(at: copy, withIntermediateDirectories: true)
      let target = copy.appendingPathComponent(received.file.lastPathComponent)
      try FileManager.default.copyItem(at: received.file, to: target)
      return PickedMedia(url: target)
    }
  }
}
