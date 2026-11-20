import SwiftUI
import UIKit

/// Workspace emoji (issue #207) on the phone: the core verifies the set's signatures and keeps the
/// pictures; this decodes them, draws them next to text and offers them in reactions and the `:`
/// suggestions. Where this device doesn't hold a picture the `:shortcode:` shows, as in older apps.
@MainActor
enum CustomEmojiPictures {
  /// How often the app asks the core to read the set again; the core paces its own reads too.
  private static let wantInterval: TimeInterval = 30
  /// Emoji the reaction menus list.
  static let menuLimit: UInt32 = 60

  private static let decoded = NSCache<NSString, UIImage>()
  private static let scaled = NSCache<NSString, UIImage>()
  private static var lastWanted = Date.distantPast

  /// The set the reaction menus list, as the chat pane last read it. Menus build when opened, so
  /// they read this without observing it.
  static var workspace: [CustomEmojiItem] = []

  static func cached(_ sha256: String) -> UIImage? { decoded.object(forKey: sha256 as NSString) }

  /// Forgets what the account showed: pictures, and the list.
  static func clear() {
    decoded.removeAllObjects()
    scaled.removeAllObjects()
    workspace = []
  }

  /// The picture, decoded once; nil until the core holds it (it downloads after being asked).
  static func picture(_ sha256: String, core: TeamCraft) async -> UIImage? {
    if let image = cached(sha256) { return image }
    guard let bytes = try? await core.customEmojiPicture(sha256: sha256), let image = UIImage(data: bytes) else {
      return nil
    }
    decoded.setObject(image, forKey: sha256 as NSString)
    return image
  }

  /// Asks the core to read the set unless that was asked lately. Returns when its read is done.
  static func want(core: TeamCraft) async {
    let now = Date()
    guard now.timeIntervalSince(lastWanted) >= wantInterval else { return }
    lastWanted = now
    // The core retries when asked again; a picture is not worth an error.
    try? await core.wantCustomEmoji()
  }

  /// The picture at `height` points, drawn once per size: inside text it can't be resized.
  static func image(_ sha256: String, height: CGFloat) -> UIImage? {
    let key = "\(sha256)@\(Int(height.rounded()))" as NSString
    if let image = scaled.object(forKey: key) { return image }
    guard let source = cached(sha256), source.size.height > 0 else { return nil }
    let size = CGSize(width: height * source.size.width / source.size.height, height: height)
    let image = UIGraphicsImageRenderer(size: size).image { _ in
      source.draw(in: CGRect(origin: .zero, size: size))
    }
    scaled.setObject(image, forKey: key)
    return image
  }

  /// Reads the set for the reaction menus and holds their pictures.
  static func refreshWorkspace(core: TeamCraft) async {
    workspace = (try? await core.customEmoji(query: "", limit: menuLimit)) ?? []
    for item in workspace { _ = await picture(item.sha256, core: core) }
  }
}

/// `text` with the `:shortcode:` words of `refs` drawn as the pictures in `pictures`, at the height
/// of `size`-point text. A word whose picture isn't there stays text. Text the core marked up keeps
/// its styling: only the emoji's words are swapped.
@MainActor
func emojiText(
  _ text: AttributedString, refs: [CustomEmojiRef], pictures: [String: UIImage], size: CGFloat
) -> Text {
  guard !refs.isEmpty else { return Text(text) }
  let items = refs.map { CustomEmojiRefItem(shortcode: $0.shortcode, sha256: $0.sha256) }
  var result = Text(verbatim: "")
  var start = text.startIndex
  for piece in customEmojiSegments(text: String(text.characters), refs: items) {
    let length: Int
    var maybePicture: Text?
    switch piece {
    case .text(let part):
      length = part.unicodeScalars.count
    case .emoji(let shortcode, let sha256):
      length = ":\(shortcode):".unicodeScalars.count
      if pictures[sha256] != nil, let image = CustomEmojiPictures.image(sha256, height: size * 1.15) {
        maybePicture = Text(Image(uiImage: image)).baselineOffset(-size * 0.2)
      }
    }
    let end = text.unicodeScalars.index(start, offsetBy: length)
    result = result + (maybePicture ?? Text(AttributedString(text[start..<end])))
    start = end
  }
  return result
}

/// One workspace emoji: its picture at `height` points, or its `:shortcode:` while this device
/// lacks it. Looks again when the store changes, as the picture arrives after the core asked.
struct CustomEmojiGlyph: View {
  @Environment(AppModel.self) private var model
  let reference: CustomEmojiRef
  var height: CGFloat = 18
  @State private var maybePicture: UIImage?

  var body: some View {
    Group {
      if let image = CustomEmojiPictures.image(reference.sha256, height: height) ?? maybePicture {
        Image(uiImage: image).accessibilityLabel(Text(verbatim: ":\(reference.shortcode):"))
      } else {
        Text(verbatim: ":\(reference.shortcode):").font(.app(.caption))
      }
    }
    .task(id: maybePicture == nil ? model.changeCount : -1) {
      guard maybePicture == nil, let core = try? await model.core() else { return }
      maybePicture = await CustomEmojiPictures.picture(reference.sha256, core: core)
      if maybePicture == nil { await CustomEmojiPictures.want(core: core) }
    }
  }
}
