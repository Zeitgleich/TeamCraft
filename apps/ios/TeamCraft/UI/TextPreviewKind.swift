import Foundation

/// Which downloaded files open in the in-app text preview (#2342): plain text and delimited files.
/// Other text types are shown as the text they are, never interpreted. A binary file turns out as
/// such when the core reads it, and the preview offers Quick Look instead.
enum TextPreviewKind {
  private static let textExtensions: Set<String> = ["txt", "csv", "tsv", "log", "md", "text", "tab"]
  private static let delimitedExtensions: Set<String> = ["csv", "tsv"]
  private static let delimitedTypes: Set<String> = ["application/csv", "application/x-csv"]

  /// The largest part of a file the preview reads, like desktop and web (#821).
  static let maxBytes: UInt64 = 10 * 1024 * 1024

  static func isPreviewable(fileName: String, mediaType: String) -> Bool {
    let type = mediaType.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
    let ext = (fileName as NSString).pathExtension.lowercased()
    if type.hasPrefix("text/") || delimitedTypes.contains(type) { return true }
    if type == "application/vnd.ms-excel" { return delimitedExtensions.contains(ext) }
    let isGeneric = type.isEmpty || type == "application/octet-stream"
    return isGeneric && textExtensions.contains(ext)
  }

  /// The kind of file a remembered choice (view, separator, header, wrapping) belongs to.
  static func typeKey(fileName: String, mediaType: String) -> String {
    let ext = (fileName as NSString).pathExtension.lowercased()
    if !ext.isEmpty { return ext }
    return mediaType.split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
  }
}

/// What the preview shows: the file on disk and the name the sender gave it.
struct TextPreviewTarget: Identifiable, Equatable {
  let path: String
  let fileName: String
  let mediaType: String
  var id: String { path }
}
