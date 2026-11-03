import Foundation

/// The corner radius of a chat bubble.
let bubbleCornerRadius: CGFloat = 18
/// The corner on the sender's side where a bubble joins the next or previous one of a group.
let bubbleJoinedCornerRadius: CGFloat = 5

/// The corners of one bubble, leading and trailing as the layout direction reads them.
struct BubbleCorners: Equatable {
  let topLeading: CGFloat
  let topTrailing: CGFloat
  let bottomTrailing: CGFloat
  let bottomLeading: CGFloat

  /// Round all over, except on the sender's side (trailing for own messages, leading for others)
  /// where the bubble joins the one above (`isContinuation`) or below (not `isGroupEnd`), as in the
  /// common messengers and the Android app (#713).
  init(isMine: Bool, isContinuation: Bool, isGroupEnd: Bool) {
    let top = isContinuation ? bubbleJoinedCornerRadius : bubbleCornerRadius
    let bottom = isGroupEnd ? bubbleCornerRadius : bubbleJoinedCornerRadius
    if isMine {
      topLeading = bubbleCornerRadius
      topTrailing = top
      bottomTrailing = bottom
      bottomLeading = bubbleCornerRadius
    } else {
      topLeading = top
      topTrailing = bubbleCornerRadius
      bottomTrailing = bubbleCornerRadius
      bottomLeading = bottom
    }
  }
}

/// Whether bubbles carry the sender's name and avatar: in channels and group messages, not in a
/// conversation with one other person, where it is always the same.
func showsSenders(_ kind: ConversationKind) -> Bool {
  kind != .direct
}

/// The core's body of a file message is the file names; with the files shown, the body is noise.
func isOnlyFileLabel(_ body: String, files: [FileInfo]) -> Bool {
  !files.isEmpty && body.trimmingCharacters(in: .whitespacesAndNewlines) == files.map(\.fileName).joined(separator: " ")
}

/// A bare link longer than this shows shortened (`example.com/…/page`); the whole URL stays its target.
let maxLinkLabelLength = 40

/// A bare link's label: without the scheme and `www.`, and when still long, the host plus the end
/// of the path (`example.com/…/report.pdf`), cut with an ellipsis at `maxLength`.
func shortLinkLabel(_ url: String, maxLength: Int = maxLinkLabelLength) -> String {
  var bare = url
  if let schemeEnd = bare.range(of: "://") { bare = String(bare[schemeEnd.upperBound...]) }
  if bare.hasPrefix("www.") { bare = String(bare.dropFirst(4)) }
  while bare.hasSuffix("/") { bare = String(bare.dropLast()) }
  guard bare.count > maxLength else { return bare }
  let host = bare.split(separator: "/", maxSplits: 1).first.map(String.init) ?? bare
  let lastPart = bare.split(separator: "/").last.map(String.init) ?? ""
  let tail = lastPart.split(whereSeparator: { $0 == "?" || $0 == "#" }).first.map(String.init) ?? ""
  let shortened = !tail.isEmpty && host != bare && tail != host ? "\(host)/…/\(tail)" : bare
  return shortened.count <= maxLength ? shortened : String(shortened.prefix(maxLength - 1)) + "…"
}

/// A stable hue (0..<1) for a person, so their name and avatar keep one colour on every device.
func personHue(_ userId: String) -> Double {
  // FNV-1a over the ID: stable across launches, unlike `hashValue`.
  var hash: UInt32 = 2_166_136_261
  for byte in userId.utf8 {
    hash = (hash ^ UInt32(byte)) &* 16_777_619
  }
  return Double(hash % 360) / 360
}
