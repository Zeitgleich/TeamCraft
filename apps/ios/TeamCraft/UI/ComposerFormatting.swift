import UIKit

// The composer's formatting (#1051). The field holds the Markdown that is sent, so a format is an
// edit of that text made by the core (teamcraft-client-api `composer_format`, the same code runs on
// Android); the styling shown while typing is attributes only. These are the pieces around the
// core's functions that don't need a view, so they can be tested alone.

/// The formats of the toolbar and the edit menu, in the order they show.
enum ComposerFormatting {
  static let formats: [ComposerFormat] = [
    .bold, .italic, .strike, .code, .codeBlock, .quote, .bulleted, .numbered, .link,
  ]

  /// What VoiceOver and the menu call a format.
  static func name(of format: ComposerFormat) -> String {
    switch format {
    case .bold: String(localized: "composer_format_bold")
    case .italic: String(localized: "composer_format_italic")
    case .strike: String(localized: "composer_format_strike")
    case .code: String(localized: "composer_format_code")
    case .codeBlock: String(localized: "composer_format_code_block")
    case .quote: String(localized: "composer_format_quote")
    case .bulleted: String(localized: "composer_format_bulleted")
    case .numbered: String(localized: "composer_format_numbered")
    case .link: String(localized: "composer_format_link")
    }
  }

  static func symbol(of format: ComposerFormat) -> String {
    switch format {
    case .bold: "bold"
    case .italic: "italic"
    case .strike: "strikethrough"
    case .code: "chevron.left.forwardslash.chevron.right"
    case .codeBlock: "curlybraces"
    case .quote: "text.quote"
    case .bulleted: "list.bullet"
    case .numbered: "list.number"
    case .link: "link"
    }
  }
}

/// The colors and the font of the field and its styling.
struct ComposerPalette {
  var font: UIFont
  var text: UIColor
  /// Syntax characters (`**`, `> `, a fence).
  var marker: UIColor
  var quote: UIColor
  var link: UIColor
  var mention: UIColor
  var codeBackground: UIColor
}

/// Styles the field's text as the core says it will render (`composerStyleSpans`). Only attributes
/// change: the characters, the selection and a keyboard's marked text stay as they are.
enum ComposerStyler {
  static func baseAttributes(_ palette: ComposerPalette) -> [NSAttributedString.Key: Any] {
    [.font: palette.font, .foregroundColor: palette.text]
  }

  static func apply(_ spans: [ComposerSpan], to storage: NSMutableAttributedString, palette: ComposerPalette) {
    let length = storage.length
    storage.beginEditing()
    storage.setAttributes(baseAttributes(palette), range: NSRange(location: 0, length: length))
    for span in spans {
      let start = Int(span.start)
      let end = min(Int(span.end), length)
      guard start < end else { continue }
      let range = NSRange(location: start, length: end - start)
      switch span.kind {
      case .marker: storage.addAttribute(.foregroundColor, value: palette.marker, range: range)
      case .bold, .heading: addTraits(.traitBold, in: range, of: storage, palette: palette)
      case .italic: addTraits(.traitItalic, in: range, of: storage, palette: palette)
      case .underline: storage.addAttribute(.underlineStyle, value: NSUnderlineStyle.single.rawValue, range: range)
      case .strike: storage.addAttribute(.strikethroughStyle, value: NSUnderlineStyle.single.rawValue, range: range)
      case .code, .codeBlock:
        setMonospaced(in: range, of: storage, palette: palette)
        storage.addAttribute(.backgroundColor, value: palette.codeBackground, range: range)
      case .quote: storage.addAttribute(.foregroundColor, value: palette.quote, range: range)
      case .link:
        storage.addAttributes([.foregroundColor: palette.link, .underlineStyle: NSUnderlineStyle.single.rawValue], range: range)
      case .mention:
        storage.addAttribute(.foregroundColor, value: palette.mention, range: range)
        addTraits(.traitBold, in: range, of: storage, palette: palette)
      }
    }
    storage.endEditing()
  }

  /// The fonts in `range` with `traits` added (bold inside italic stays italic).
  private static func addTraits(
    _ traits: UIFontDescriptor.SymbolicTraits, in range: NSRange, of storage: NSMutableAttributedString,
    palette: ComposerPalette
  ) {
    var changes: [(NSRange, UIFont)] = []
    storage.enumerateAttribute(.font, in: range) { value, sub, _ in
      changes.append((sub, with(traits, (value as? UIFont) ?? palette.font)))
    }
    for (sub, font) in changes { storage.addAttribute(.font, value: font, range: sub) }
  }

  private static func setMonospaced(in range: NSRange, of storage: NSMutableAttributedString, palette: ComposerPalette) {
    var changes: [(NSRange, UIFont)] = []
    storage.enumerateAttribute(.font, in: range) { value, sub, _ in
      let current = (value as? UIFont) ?? palette.font
      let mono = UIFont.monospacedSystemFont(ofSize: palette.font.pointSize * 0.95, weight: .regular)
      changes.append((sub, with(current.fontDescriptor.symbolicTraits.intersection([.traitBold, .traitItalic]), mono)))
    }
    for (sub, font) in changes { storage.addAttribute(.font, value: font, range: sub) }
  }

  static func with(_ traits: UIFontDescriptor.SymbolicTraits, _ font: UIFont) -> UIFont {
    let combined = font.fontDescriptor.symbolicTraits.union(traits)
    guard let descriptor = font.fontDescriptor.withSymbolicTraits(combined) else { return font }
    return UIFont(descriptor: descriptor, size: font.pointSize)
  }
}

/// One replaced piece of a text: what an edit of the whole text comes to.
struct ComposerChange: Equatable {
  var range: NSRange
  var replacement: String
}

enum ComposerDiff {
  /// The smallest single replacement (in UTF-16 units) that turns `old` into `new`, so the field can
  /// take the core's edit as an undoable change of that piece and keep the rest, and the caret, alone.
  static func change(from old: String, to new: String) -> ComposerChange {
    let before = Array(old.utf16)
    let after = Array(new.utf16)
    var prefix = 0
    while prefix < before.count, prefix < after.count, before[prefix] == after[prefix] { prefix += 1 }
    // Never between the two halves of a surrogate pair.
    if prefix > 0, UTF16.isLeadSurrogate(before[prefix - 1]) { prefix -= 1 }
    var suffix = 0
    while suffix < before.count - prefix, suffix < after.count - prefix,
      before[before.count - 1 - suffix] == after[after.count - 1 - suffix]
    {
      suffix += 1
    }
    if suffix > 0, UTF16.isTrailSurrogate(after[after.count - suffix]) { suffix -= 1 }
    let replaced = NSRange(location: prefix, length: before.count - prefix - suffix)
    let inserted = String(decoding: after[prefix..<(after.count - suffix)], as: UTF16.self)
    return ComposerChange(range: replaced, replacement: inserted)
  }
}

enum ComposerKeys {
  typealias Handler = (_ text: String, _ caret: UInt32) -> ComposerEdit?

  /// What the core makes of a keystroke the field is about to apply (`shouldChangeTextIn`): Return in
  /// a list or quote carries on or leaves it, Backspace right after a block's marker takes it off.
  /// `nil`: let the keystroke through. Only for a caret, never for a replaced selection.
  static func edit(
    text: String, selection: NSRange, range: NSRange, replacement: String,
    enter: Handler = { composerEnter(text: $0, start: $1, end: $1) },
    backspace: Handler = { composerBackspace(text: $0, start: $1, end: $1) }
  ) -> ComposerEdit? {
    guard selection.length == 0 else { return nil }
    let caret = UInt32(selection.location)
    if replacement == "\n", range.length == 0, range.location == selection.location {
      return enter(text, caret)
    }
    if replacement.isEmpty, range.length == 1, range.location + 1 == selection.location {
      return backspace(text, caret)
    }
    return nil
  }
}
