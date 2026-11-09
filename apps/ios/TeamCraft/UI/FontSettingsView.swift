import SwiftUI

/// Settings > Chats > Font (#1494): the fonts by group, each name in its own face, the chosen one
/// checked. It applies once Settings closes (RootView).
struct FontSettingsView: View {
  @AppStorage(AppFont.key) private var appFontRaw = AppFont.system.rawValue

  var body: some View {
    List {
      ForEach(AppFont.Group.allCases, id: \.self) { group in
        Section(group.title) {
          ForEach(AppFont.allCases.filter { $0.group == group }) { font in
            row(font)
          }
        }
      }
    }
    .navigationTitle("font")
    .navigationBarTitleDisplayMode(.inline)
  }

  private func row(_ font: AppFont) -> some View {
    let isChosen = font.rawValue == appFontRaw
    return Button {
      appFontRaw = font.rawValue
    } label: {
      HStack {
        FontName(font: font).foregroundStyle(Theme.text)
        Spacer()
        if isChosen {
          Image(systemName: "checkmark").foregroundStyle(Theme.accent)
        }
      }
      .contentShape(Rectangle())
    }
    .accessibilityAddTraits(isChosen ? .isSelected : [])
  }
}

/// A font's name in that font: its own name, or "System font" in the system font.
struct FontName: View {
  let font: AppFont

  var body: some View {
    Group {
      if let name = font.displayName {
        Text(verbatim: name)
      } else {
        Text("font_system")
      }
    }
    .font(.app(.body, in: font))
  }
}
