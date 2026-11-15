import Foundation

/// The ways to attach something in the composer. The attach menu lists the one used last first
/// (issue #2062), so the thumb finds it where it already is.
enum AttachKind: String, CaseIterable, Sendable {
  case photo
  case camera
  case file

  static let storageKey = "composer.lastAttach"

  /// The kind stored under `raw`; the first one for an unknown or missing value.
  static func parse(_ raw: String?) -> AttachKind {
    raw.flatMap(AttachKind.init(rawValue:)) ?? .photo
  }

  /// All kinds with `last` first; the others keep their order.
  static func ordered(last: AttachKind) -> [AttachKind] {
    [last] + allCases.filter { $0 != last }
  }
}
