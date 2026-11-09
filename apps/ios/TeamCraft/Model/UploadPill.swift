import Foundation

/// What the one upload control of a message shows (Signal-style): a pill in the corner of its
/// pictures, or a row under documents, with one ring for all its files instead of a ring and a
/// "Cancel" per file. The same rule as desktop and web (`teamcraft_client_api::upload_pill`) and
/// Android (`UploadPill.kt`), with the same test cases.
enum UploadPill: Equatable {
  /// Going up, but no byte has left yet: "Processing…" and a turning ring.
  case processing
  /// No byte has left yet and an attempt failed; the core tries again by itself (#742).
  case retrying
  /// Bytes are moving: "3.2 MB / 70 MB" and a ring filled to the share of all files together.
  case sending(doneBytes: Int64, totalBytes: Int64)
  /// A file stopped retrying at the usual pace: "Try again" restarts the message's upload.
  case failed(isStorageUnavailable: Bool)

  /// The control for a message's files; nil when nothing is going up (done, or never was). Files
  /// that finished count with their whole size, so the ring doesn't jump back when one is done; a
  /// file that failed for good (the message went out without it) doesn't count.
  static func of(_ files: [FileInfo]) -> UploadPill? {
    if let stalled = files.first(where: { $0.transfer.state == "upload_stalled" }) {
      return .failed(isStorageUnavailable: stalled.transfer.isStorageUnavailable)
    }
    guard files.contains(where: { $0.transfer.state == "uploading" }) else { return nil }
    var doneBytes: Int64 = 0
    var totalBytes: Int64 = 0
    var hasFailedAttempts = false
    for file in files {
      let transfer = file.transfer
      switch transfer.state {
      case "uploading":
        let total = transfer.totalBytes ?? 0
        doneBytes += min(transfer.doneBytes ?? 0, total)
        totalBytes += total
        hasFailedAttempts = hasFailedAttempts || (transfer.failedAttempts ?? 0) > 0
      case "upload_failed":
        break
      default:
        doneBytes += file.sizeBytes
        totalBytes += file.sizeBytes
      }
    }
    if doneBytes > 0 { return .sending(doneBytes: doneBytes, totalBytes: totalBytes) }
    return hasFailedAttempts ? .retrying : .processing
  }

  /// How far the files got, 0...1; nil while the ring turns without a value.
  var maybeFraction: Double? {
    guard case .sending(let done, let total) = self, total > 0 else { return nil }
    return min(1, max(0, Double(done) / Double(total)))
  }
}
