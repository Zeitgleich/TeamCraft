import Foundation
import Observation
import SwiftUI
import UIKit

/// The colour theme (#1493, #202): the server's default or a preset or custom theme of four colours.
/// The stored value is the synced setting `display.color_theme` (`brand`, `preset:<id>`,
/// `custom:#rrggbb,…`), kept as the account has it; the colours come from the core
/// (`colorThemeTokens`), which derives them with the contrast rules every client shares.
enum ColorThemeSetting {
  /// The same key `AppSettingsStores` carried the synced value under before iOS had the setting.
  static let key = "settings_sync_color_theme"
  /// The server's default: the app's own colours.
  static let defaultValue = "brand"
  static let customPrefix = "custom:"
  /// The app's own colours (navigation, selected items, online, notifications), where a custom
  /// theme starts when the current one has none.
  static let productColors: [UInt32] = [0x1A2434, 0x0064FC, 0x2EB67D, 0xC8441D]

  /// The value on this device; nothing stored is the default. An unknown value is returned as it is
  /// (it shows the default colours, and is only replaced when the user chooses something else).
  static func stored(in defaults: UserDefaults = .standard) -> String {
    defaults.string(forKey: key) ?? defaultValue
  }

  /// `#rgb` or `#rrggbb`, the `#` optional, as `0xRRGGBB`; `nil` for anything else.
  static func parseHex(_ text: String) -> UInt32? {
    var digits = text.trimmingCharacters(in: .whitespaces)
    if digits.hasPrefix("#") { digits.removeFirst() }
    guard digits.allSatisfy({ $0.isASCII && $0.isHexDigit }) else { return nil }
    switch digits.count {
    case 3:
      guard let short = UInt32(digits, radix: 16) else { return nil }
      let (r, g, b) = ((short >> 8) & 0xF, (short >> 4) & 0xF, short & 0xF)
      return (r * 17) << 16 | (g * 17) << 8 | b * 17
    case 6:
      return UInt32(digits, radix: 16)
    default:
      return nil
    }
  }

  /// `#rrggbb`, for a hex field.
  static func hex(_ value: UInt32) -> String {
    "#" + String(format: "%06x", value & 0xFFFFFF)
  }
}

/// The colours of the chosen theme in light and dark; what the views read through `Theme`. It is
/// observable, so a view that read a colour while building its body is built again when the theme
/// changes (open screens included, no restart); light/dark is resolved by the colour itself.
@Observable
final class ColorThemeModel {
  static let shared = ColorThemeModel()

  /// The stored value, as it is.
  private(set) var value: String
  /// The colours of `value`; `nil` for the server's default and for a value this build doesn't know.
  private(set) var maybeColors: (light: AppThemeTokens, dark: AppThemeTokens)?

  @ObservationIgnored private let defaults: UserDefaults
  @ObservationIgnored private var maybeObserver: NSObjectProtocol?

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
    let stored = ColorThemeSetting.stored(in: defaults)
    value = stored
    maybeColors = Self.colors(of: stored)
    // The sync (or another screen) writes the value into the defaults.
    maybeObserver = NotificationCenter.default.addObserver(
      forName: UserDefaults.didChangeNotification, object: defaults, queue: .main
    ) { [weak self] _ in self?.reload() }
  }

  deinit {
    if let maybeObserver { NotificationCenter.default.removeObserver(maybeObserver) }
  }

  /// Whether the theme gives the app colours of its own (not the server's default).
  var isThemed: Bool { maybeColors != nil }

  /// Takes the value from the defaults, if it moved.
  func reload() {
    let stored = ColorThemeSetting.stored(in: defaults)
    guard stored != value else { return }
    value = stored
    maybeColors = Self.colors(of: stored)
  }

  /// The user's choice: stored (the sync takes it from there) and shown at once.
  func choose(_ newValue: String) {
    guard newValue != value else { return }
    defaults.set(newValue, forKey: ColorThemeSetting.key)
    reload()
  }

  /// A colour of the theme, or `fallback` (light, dark) under the default theme.
  func color(
    _ token: KeyPath<AppThemeTokens, UInt32>, fallback: (light: UInt32, dark: UInt32)
  ) -> Color {
    guard let colors = maybeColors else { return Self.dynamic(light: fallback.light, dark: fallback.dark) }
    return Self.dynamic(light: colors.light[keyPath: token], dark: colors.dark[keyPath: token])
  }

  static func colors(of value: String) -> (light: AppThemeTokens, dark: AppThemeTokens)? {
    guard let light = colorThemeTokens(value: value, isDark: false),
      let dark = colorThemeTokens(value: value, isDark: true)
    else { return nil }
    return (light, dark)
  }

  static func dynamic(light: UInt32, dark: UInt32) -> Color {
    Color(
      uiColor: UIColor { traits in
        UIColor(hex: traits.userInterfaceStyle == .dark ? dark : light)
      })
  }
}
