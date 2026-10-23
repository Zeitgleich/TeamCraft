import SwiftUI
import UIKit

/// The desktop UI's design tokens (crates/teamcraft-ui/style/main.css), the same values as the
/// Android theme, so the apps look alike. Each color has a light and a dark value.
enum Theme {
  static let accent = dynamic(light: 0x0B7F78, dark: 0x35C6BA)
  static let onAccent = dynamic(light: 0xFFFFFF, dark: 0x04211E)
  static let accentSoft = dynamic(light: 0xDFF3F1, dark: 0x123A38)
  static let accentStrong = dynamic(light: 0x096C66, dark: 0x5FD8CE)
  static let background = dynamic(light: 0xF6F7F9, dark: 0x0F141D)
  static let surface = dynamic(light: 0xFFFFFF, dark: 0x151B27)
  static let text = dynamic(light: 0x1B2230, dark: 0xE4E9F2)
  static let textMuted = dynamic(light: 0x566175, dark: 0x9AA6BA)
  /// Dividers and card edges.
  static let border = dynamic(light: 0xDCE0E7, dark: 0x28324A)
  /// Edges of fields and controls: 3:1 against the surface (WCAG 1.4.11).
  static let controlBorder = dynamic(light: 0x848EA0, dark: 0x626D86)
  static let danger = dynamic(light: 0xC0392B, dark: 0xFF7A6B)
  /// Unread and mention badges; white numbers on it need 4.5:1.
  static let badge = dynamic(light: 0xC8441F, dark: 0xC8441F)
  static let mentionBackground = dynamic(light: 0xFFF2CC, dark: 0x4A3B10)
  static let mentionText = dynamic(light: 0x6A4B00, dark: 0xFFD978)
  /// Own messages.
  static let ownBubble = accentSoft

  static let radius: CGFloat = 12
  static let spacing: CGFloat = 12

  private static func dynamic(light: UInt32, dark: UInt32) -> Color {
    Color(
      uiColor: UIColor { traits in
        UIColor(hex: traits.userInterfaceStyle == .dark ? dark : light)
      })
  }
}

extension UIColor {
  convenience init(hex: UInt32) {
    self.init(
      red: CGFloat((hex >> 16) & 0xFF) / 255,
      green: CGFloat((hex >> 8) & 0xFF) / 255,
      blue: CGFloat(hex & 0xFF) / 255,
      alpha: 1)
  }
}

/// A filled call-to-action button in the accent color.
struct PrimaryButtonStyle: ButtonStyle {
  @Environment(\.isEnabled) private var isEnabled

  func makeBody(configuration: Configuration) -> some View {
    configuration.label
      .font(.headline)
      .frame(maxWidth: .infinity)
      .padding(.vertical, 14)
      .background(Theme.accent.opacity(isEnabled ? (configuration.isPressed ? 0.8 : 1) : 0.4))
      .foregroundStyle(Theme.onAccent)
      .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
  }
}

/// A round avatar with the name's initials.
struct AvatarView: View {
  let name: String
  var size: CGFloat = 36

  var body: some View {
    Text(Self.initials(of: name))
      .font(.system(size: size * 0.4, weight: .semibold))
      .foregroundStyle(Theme.accentStrong)
      .frame(width: size, height: size)
      .background(Theme.accentSoft)
      .clipShape(Circle())
      .accessibilityHidden(true)
  }

  static func initials(of name: String) -> String {
    let words = name.split(separator: " ").prefix(2)
    let letters = words.compactMap { $0.first }.map { String($0).uppercased() }
    return letters.isEmpty ? "?" : letters.joined()
  }
}

/// An unread or mention counter.
struct CountBadge: View {
  let count: UInt32
  var isMention = false

  var body: some View {
    Text(count > 99 ? "99+" : "\(count)")
      .font(.caption2.weight(.bold))
      .foregroundStyle(.white)
      .padding(.horizontal, 7)
      .padding(.vertical, 2)
      .background(isMention ? Theme.badge : Theme.accent)
      .clipShape(Capsule())
  }
}
