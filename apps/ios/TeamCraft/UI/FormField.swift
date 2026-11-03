import SwiftUI

// forms.md on iOS: a field's message sits directly below it, VoiceOver hears it as the field's
// hint, and a refused submit moves keyboard and VoiceOver focus to the first invalid field.

/// The line below a field: its error when it has one, else `maybeHint` (a rule such as the
/// minimum length), else nothing.
struct FieldMessage: View {
  let maybeError: LocalizedStringKey?
  var maybeHint: LocalizedStringKey?

  var body: some View {
    if let maybeError {
      Label {
        Text(maybeError)
      } icon: {
        Image(systemName: "exclamationmark.circle.fill").accessibilityHidden(true)
      }
      .font(.footnote)
      .foregroundStyle(Theme.danger)
      .frame(maxWidth: .infinity, alignment: .leading)
    } else if let maybeHint {
      Text(maybeHint)
        .font(.footnote)
        .foregroundStyle(Theme.textMuted)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
  }
}

extension View {
  /// Names the field's error for VoiceOver, in the words shown below it.
  func fieldError(_ maybeError: LocalizedStringKey?) -> some View {
    accessibilityHint(maybeError.map { Text($0) } ?? Text(verbatim: ""))
  }

  /// The edge of a field drawn on the surface: red while it has an error.
  func fieldEdge(isError: Bool, cornerRadius: CGFloat = Theme.radius) -> some View {
    overlay(
      RoundedRectangle(cornerRadius: cornerRadius).stroke(
        isError ? Theme.danger : Theme.controlBorder))
  }
}

/// Scrolls the field with `id` to the middle of its scroller: smooth, or at once with Reduce Motion.
@MainActor
func scrollToMiddle<ID: Hashable>(_ id: ID, in proxy: ScrollViewProxy, reduceMotion: Bool) {
  if reduceMotion {
    proxy.scrollTo(id, anchor: .center)
  } else {
    withAnimation(.smooth) { proxy.scrollTo(id, anchor: .center) }
  }
}
