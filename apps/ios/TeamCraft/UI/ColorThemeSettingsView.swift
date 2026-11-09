import SwiftUI

/// Settings > Colours (#1493): the server's default, the presets by group (a swatch and a name, the
/// chosen one outlined), or a custom theme of four colours with hex fields, a theme code to copy and
/// paste, and "Surprise me". The same choices as Android's picker. A choice applies at once, to the
/// open screens too, and follows the account (`SettingsSyncer`). The colours of every theme come from
/// the core; this only shows them. The brand decides what is on offer and what the default is
/// (#1495): a stored choice it doesn't allow is shown as the default and only ever replaced by the
/// person picking something else.
struct ColorThemeSettingsView: View {
  private let theme = ColorThemeModel.shared
  @Environment(\.colorScheme) private var colorScheme
  @State private var wantsCustom = ColorThemeModel.shared.inForce.hasPrefix(ColorThemeSetting.customPrefix)

  private var isCustom: Bool { wantsCustom && theme.canCustomize }

  var body: some View {
    Form {
      Section {
        if theme.canCustomize {
          Picker("color_theme", selection: $wantsCustom) {
            Text("color_theme_presets").tag(false)
            Text("color_theme_custom").tag(true)
          }
          .pickerStyle(.segmented)
          .labelsHidden()
          .accessibilityLabel(Text("color_theme"))
        }
      } footer: {
        VStack(alignment: .leading, spacing: 6) {
          Text("color_theme_hint")
          if theme.isWithheld { Text("color_theme_withheld") }
          if !theme.hasChoices { Text("color_theme_fixed") }
        }
      }
      if isCustom {
        CustomColorThemeSections(theme: theme)
      } else {
        presetSections
      }
    }
    .navigationTitle("color_theme")
    .navigationBarTitleDisplayMode(.inline)
    .onChange(of: wantsCustom) { _, wantsCustom in
      // Custom starts from what is on screen, so the switch itself changes nothing visible.
      guard wantsCustom, !theme.inForce.hasPrefix(ColorThemeSetting.customPrefix) else { return }
      let colors = colorThemeColors(value: theme.effectiveValue) ?? ColorThemeSetting.productColors
      if let value = colorThemeCustom(colors: colors) { theme.choose(value) }
    }
  }

  private static let groups: [(slug: String, title: LocalizedStringKey)] = [
    ("calm", "color_theme_group_calm"),
    ("bright", "color_theme_group_bright"),
    ("vivid", "color_theme_group_vivid"),
    ("accessible", "color_theme_group_accessible"),
  ]

  @ViewBuilder private var presetSections: some View {
    let presets = colorThemePresets().filter { theme.allowsPreset(id: $0.id) }
    let isDark = colorScheme == .dark
    let maybeDefaultTokens = theme.maybePolicy.flatMap { colorThemeTokens(value: $0.defaultValue, isDark: isDark) }
    Section {
      grid {
        ThemeSwatch(
          name: Text("color_theme_brand"),
          colors: maybeDefaultTokens.map(ThemeSwatch.Colors.init(tokens:)) ?? ThemeSwatch.productColors(isDark: isDark),
          isSelected: theme.inForce == ColorThemeSetting.defaultValue || !theme.isThemed
        ) { theme.choose(ColorThemeSetting.defaultValue) }
      }
    }
    ForEach(Self.groups, id: \.slug) { group in
      let inGroup = presets.filter { $0.group == group.slug }
      if !inGroup.isEmpty {
        Section(group.title) {
          grid {
            ForEach(inGroup, id: \.value) { preset in
              if let tokens = colorThemeTokens(value: preset.value, isDark: isDark) {
                ThemeSwatch(
                  name: ColorThemePresetName.text(for: preset.id),
                  colors: ThemeSwatch.Colors(tokens: tokens),
                  isSelected: theme.inForce == preset.value
                ) { theme.choose(preset.value) }
              }
            }
          }
        }
      }
    }
  }

  private func grid<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
    LazyVGrid(columns: [GridItem(.adaptive(minimum: 84), spacing: 12, alignment: .top)], spacing: 12) {
      content()
    }
    .padding(.vertical, 4)
  }
}

/// A tiny sidebar in the theme's colours (a selected row, the online dot, a badge) and its name.
private struct ThemeSwatch: View {
  struct Colors {
    let navigation: UInt32
    let selected: UInt32
    let presence: UInt32
    let badge: UInt32

    init(navigation: UInt32, selected: UInt32, presence: UInt32, badge: UInt32) {
      self.navigation = navigation
      self.selected = selected
      self.presence = presence
      self.badge = badge
    }

    init(tokens: AppThemeTokens) {
      self.init(navigation: tokens.chrome, selected: tokens.selected, presence: tokens.presence, badge: tokens.badge)
    }
  }

  let name: Text
  let colors: Colors
  let isSelected: Bool
  let action: () -> Void

  /// The app's own colours, for the server's default.
  static func productColors(isDark: Bool) -> Colors {
    Colors(
      navigation: isDark ? 0x141414 : ColorThemeSetting.productColors[0],
      selected: ColorThemeSetting.productColors[1], presence: ColorThemeSetting.productColors[2],
      badge: 0xC8441F)
  }

  var body: some View {
    Button(action: action) {
      VStack(spacing: 4) {
        ZStack {
          RoundedRectangle(cornerRadius: Theme.radius)
            .strokeBorder(isSelected ? Theme.accent : Theme.border, lineWidth: isSelected ? 2 : 1)
          RoundedRectangle(cornerRadius: Theme.radius - 2)
            .fill(color(colors.navigation))
            .padding(isSelected ? 4 : 2)
          VStack {
            RoundedRectangle(cornerRadius: 3).fill(color(colors.selected)).frame(height: 8)
            Spacer(minLength: 0)
            HStack {
              Circle().fill(color(colors.presence)).frame(width: 8, height: 8)
              Spacer(minLength: 0)
              Capsule().fill(color(colors.badge)).frame(width: 14, height: 8)
            }
          }
          .padding(10)
        }
        .frame(width: 76, height: 50)
        name
          .font(.app(isSelected ? .footnote : .caption).weight(isSelected ? .semibold : .regular))
          .foregroundStyle(Theme.text)
          .multilineTextAlignment(.center)
          .lineLimit(2)
      }
      .frame(maxWidth: .infinity)
      .contentShape(Rectangle())
    }
    .buttonStyle(.plain)
    .accessibilityAddTraits(isSelected ? .isSelected : [])
  }

  private func color(_ value: UInt32) -> Color { Color(uiColor: UIColor(hex: value)) }
}

/// The names of the presets, by the id the core gives (`color_theme_<id>`). A preset a newer core
/// adds shows its id until the app has its name.
enum ColorThemePresetName {
  static func text(for id: String) -> Text {
    switch id {
    case "harbor": return Text("color_theme_harbor")
    case "graphite": return Text("color_theme_graphite")
    case "pine": return Text("color_theme_pine")
    case "deep-sea": return Text("color_theme_deep_sea")
    case "heather": return Text("color_theme_heather")
    case "paper": return Text("color_theme_paper")
    case "dune": return Text("color_theme_dune")
    case "meadow": return Text("color_theme_meadow")
    case "lavender": return Text("color_theme_lavender")
    case "ember": return Text("color_theme_ember")
    case "orchid": return Text("color_theme_orchid")
    case "lagoon": return Text("color_theme_lagoon")
    case "indigo": return Text("color_theme_indigo")
    case "red-green-safe": return Text("color_theme_red_green_safe")
    case "blue-yellow-safe": return Text("color_theme_blue_yellow_safe")
    case "high-contrast": return Text("color_theme_high_contrast")
    default: return Text(verbatim: id)
    }
  }
}

/// Four hex fields; a valid set applies at once. A field shows its error once it was left (forms.md);
/// the theme code can be copied, pasted, or rolled with "Surprise me".
private struct CustomColorThemeSections: View {
  let theme: ColorThemeModel
  @State private var drafts: [String]
  @State private var wasLeft = [false, false, false, false]
  @State private var code = ""
  @State private var isCodeRefused = false
  @State private var copies = 0
  @FocusState private var focused: Int?

  private static let slots: [LocalizedStringKey] = [
    "color_theme_navigation", "color_theme_selection", "color_theme_presence", "color_theme_notification",
  ]

  init(theme: ColorThemeModel) {
    self.theme = theme
    _drafts = State(initialValue: Self.hexes(of: theme.value))
  }

  var body: some View {
    Group {
      sections
    }
    // The theme changed elsewhere (another device, the code, "Surprise me"): show its colours.
    .onChange(of: theme.value) { _, value in
      let colors = Self.hexes(of: value)
      for index in drafts.indices where ColorThemeSetting.parseHex(drafts[index]) != ColorThemeSetting.parseHex(colors[index]) {
        drafts[index] = colors[index]
      }
    }
    .onChange(of: focused) { old, _ in
      if let old, !drafts[old].isEmpty { wasLeft[old] = true }
    }
  }

  @ViewBuilder private var sections: some View {
    Section {
      ForEach(Self.slots.indices, id: \.self) { index in
        field(index)
      }
    } footer: {
      Text("color_theme_custom_hint")
    }
    Section {
      Button("color_theme_copy", systemImage: "doc.on.doc") {
        if let code = colorThemeShareCode(value: theme.value) {
          UIPasteboard.general.string = code
          copies += 1
        }
      }
      .disabled(colorThemeShareCode(value: theme.value) == nil)
      .sensoryFeedback(.success, trigger: copies)
      Button("color_theme_surprise", systemImage: "sparkles") {
        wasLeft = [false, false, false, false]
        theme.choose(colorThemeSurprise(hue: Double.random(in: 0..<360)))
      }
    }
    Section {
      VStack(alignment: .leading, spacing: 6) {
        HStack {
          TextField("color_theme_import", text: $code, prompt: Text(verbatim: "#1b2233,#2f66e0,#2eb67d,#d1453b"))
            .font(.system(.body, design: .monospaced))
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
            .submitLabel(.done)
            .onSubmit(applyCode)
            .onChange(of: code) { _, new in
              if new.count > 64 { code = String(new.prefix(64)) }
              isCodeRefused = false
            }
          Button("color_theme_import_apply", action: applyCode)
            .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.vertical, 2)
        FieldMessage(maybeError: isCodeRefused ? "color_theme_import_invalid" : nil)
      }
      .fieldError(isCodeRefused ? "color_theme_import_invalid" : nil)
    }
  }

  private func field(_ index: Int) -> some View {
    let maybeColor = ColorThemeSetting.parseHex(drafts[index])
    let isError = wasLeft[index] && maybeColor == nil
    let maybeError: LocalizedStringKey? = isError ? "color_theme_hex_invalid" : nil
    return VStack(alignment: .leading, spacing: 6) {
      HStack(spacing: 12) {
        Circle()
          .fill(maybeColor.map { Color(uiColor: UIColor(hex: $0)) } ?? .clear)
          .overlay(Circle().stroke(Theme.controlBorder))
          .frame(width: 22, height: 22)
          .accessibilityHidden(true)
        TextField(Self.slots[index], text: $drafts[index])
          .font(.system(.body, design: .monospaced))
          .textInputAutocapitalization(.never)
          .autocorrectionDisabled()
          .focused($focused, equals: index)
          .onChange(of: drafts[index]) { _, text in
            if text.count > 7 { drafts[index] = String(text.prefix(7)) }
            applyDrafts()
          }
      }
      .padding(.vertical, 2)
      FieldMessage(maybeError: maybeError)
    }
    .fieldError(maybeError)
  }

  /// Four valid colours make the theme.
  private func applyDrafts() {
    let colors = drafts.compactMap(ColorThemeSetting.parseHex)
    guard colors.count == drafts.count, let value = colorThemeCustom(colors: colors) else { return }
    theme.choose(value)
  }

  private func applyCode() {
    guard let value = colorThemeFromCode(code: code) else {
      isCodeRefused = true
      return
    }
    theme.choose(value)
    code = ""
    isCodeRefused = false
  }

  private static func hexes(of value: String) -> [String] {
    (colorThemeColors(value: value) ?? ColorThemeSetting.productColors).map(ColorThemeSetting.hex)
  }
}
