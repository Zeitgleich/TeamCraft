import SwiftUI

/// The interface font (#1494): the system font or one of the fonts the app bundles, the same set as
/// Android (`apps/android/app/src/main/res/font`, built by `scripts/build-fonts.py`; registered with
/// `UIAppFonts`). The raw values are the setting's wire names on every client
/// (`teamcraft-client-api` `font.rs`). Font names are the fonts' own and are not translated.
///
/// Views take their text styles from `Font.app(_:)` instead of `Font.caption` and friends, so the
/// chosen font reaches the whole app with Dynamic Type intact. The choice is kept on this device;
/// iOS doesn't sync settings with the account yet.
enum AppFont: String, CaseIterable, Identifiable {
  case system
  case inter
  case googleSansFlex = "google-sans-flex"
  case ibmPlexSans = "ibm-plex-sans"
  case sourceSans3 = "source-sans-3"
  case notoSans = "noto-sans"
  case atkinsonHyperlegibleNext = "atkinson-hyperlegible-next"
  case lexend
  case openDyslexic = "opendyslexic"
  case jetBrainsMono = "jetbrains-mono"

  static let key = "ui_font"

  /// Posted when Settings closes, so the app shows a font chosen there (`RootView`).
  static let mayHaveChanged = Notification.Name("de.zeitgleich.teamcraft.app-font-may-have-changed")

  enum Group: CaseIterable {
    case suggested, sans, readability, monospace

    var title: LocalizedStringKey {
      switch self {
      case .suggested: return "font_group_suggested"
      case .sans: return "font_group_sans"
      case .readability: return "font_group_readability"
      case .monospace: return "font_group_monospace"
      }
    }
  }

  var id: String { rawValue }

  /// The font chosen on this device; an unknown value shows the system font.
  static var current: AppFont {
    AppFont(rawValue: UserDefaults.standard.string(forKey: key) ?? "") ?? .system
  }

  /// The family name inside the bundled file; `nil` for the system font.
  var family: String? {
    switch self {
    case .system: return nil
    case .inter: return "Inter"
    case .googleSansFlex: return "Google Sans Flex"
    case .ibmPlexSans: return "IBM Plex Sans Var"
    case .sourceSans3: return "SourceSans3VF"
    case .notoSans: return "Noto Sans"
    case .atkinsonHyperlegibleNext: return "Atkinson Hyperlegible Next"
    case .lexend: return "Lexend"
    case .openDyslexic: return "OpenDyslexic"
    case .jetBrainsMono: return "JetBrains Mono"
    }
  }

  /// The name as its makers spell it; `nil` for the system font (translated, `font_system`).
  var displayName: String? {
    switch self {
    case .system: return nil
    case .inter: return "Inter"
    case .googleSansFlex: return "Google Sans Flex"
    case .ibmPlexSans: return "IBM Plex Sans"
    case .sourceSans3: return "Source Sans 3"
    case .notoSans: return "Noto Sans"
    case .atkinsonHyperlegibleNext: return "Atkinson Hyperlegible Next"
    case .lexend: return "Lexend"
    case .openDyslexic: return "OpenDyslexic"
    case .jetBrainsMono: return "JetBrains Mono"
    }
  }

  var group: Group {
    switch self {
    case .system, .inter: return .suggested
    case .googleSansFlex, .ibmPlexSans, .sourceSans3, .notoSans: return .sans
    case .atkinsonHyperlegibleNext, .lexend, .openDyslexic: return .readability
    case .jetBrainsMono: return .monospace
    }
  }
}

extension Font {
  /// The text style `style` in the chosen interface font, scaled with Dynamic Type.
  static func app(_ style: Font.TextStyle) -> Font {
    app(style, in: AppFont.current)
  }

  /// The text style `style` in `font` (the font menu shows each font in its own face).
  static func app(_ style: Font.TextStyle, in font: AppFont) -> Font {
    guard let family = font.family else { return .system(style) }
    // The system styles' weights; a variable font's default instance may be another (Source Sans 3
    // starts at ExtraLight), so the weight is always set.
    let weight: Font.Weight = style == .headline ? .semibold : .regular
    return .custom(family, size: style.defaultPointSize, relativeTo: style).weight(weight)
  }
}

private extension Font.TextStyle {
  /// The point size at the default Dynamic Type size (Large), which `relativeTo:` scales from.
  var defaultPointSize: CGFloat {
    switch self {
    case .largeTitle: return 34
    case .title: return 28
    case .title2: return 22
    case .title3: return 20
    case .headline, .body: return 17
    case .callout: return 16
    case .subheadline: return 15
    case .footnote: return 13
    case .caption: return 12
    case .caption2: return 11
    @unknown default: return 17
    }
  }
}
