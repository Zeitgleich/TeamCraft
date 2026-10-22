import Foundation
import SwiftUI
import UniformTypeIdentifiers

/// Why a picked file isn't attached.
enum AttachmentError: Error, Equatable {
  case tooLarge(limitBytes: Int64)
  case unreadable
  /// The core refused the message with its files.
  case sendFailed
}

/// Copies files the user picked (photo library, Files app) into the app's uploads folder, so the
/// core can read them by path while it encrypts and uploads them in chunks (the picker's own
/// temporary copy and security scope may be gone by then).
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
    return PickedFile(fileName: name, mediaType: Self.mediaType(of: target), path: target.path)
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
