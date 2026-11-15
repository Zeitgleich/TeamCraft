import Contacts
import ContactsUI
import SwiftUI
import UIKit

/// The part of `text` inside `range` (UTF-16 units, as `UITextView` reports a selection), trimmed;
/// all of the text when nothing, or only blanks, is selected (issue #2050).
func quotedPart(of text: String, range: NSRange) -> String {
  let ns = text as NSString
  let start = min(max(range.location, 0), ns.length)
  let end = min(max(range.location + range.length, start), ns.length)
  let part = ns.substring(with: NSRange(location: start, length: end - start)).trimmingCharacters(in: .whitespacesAndNewlines)
  return part.isEmpty ? text : part
}

/// A message's text where words can be selected, with "Quote selection" to reply to just those (#2050).
/// Nothing selected quotes the whole text.
struct SelectTextSheet: View {
  let text: String
  let onQuote: (String) -> Void

  @Environment(\.dismiss) private var dismiss
  @State private var selection = NSRange(location: 0, length: 0)

  var body: some View {
    NavigationStack {
      VStack(alignment: .leading, spacing: 12) {
        Text("select_text_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        SelectableTextView(text: text, selection: $selection)
      }
      .padding()
      .navigationTitle("select_text_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("cancel") { dismiss() } }
        ToolbarItem(placement: .confirmationAction) {
          Button("select_text_quote") {
            onQuote(quotedPart(of: text, range: selection))
            dismiss()
          }
        }
      }
    }
    .presentationDetents([.medium, .large])
  }
}

/// A read-only `UITextView`: the system's selection handles and menu, and the selection reported back.
private struct SelectableTextView: UIViewRepresentable {
  let text: String
  @Binding var selection: NSRange

  func makeCoordinator() -> Coordinator { Coordinator(self) }

  func makeUIView(context: Context) -> UITextView {
    let view = UITextView()
    view.isEditable = false
    view.isSelectable = true
    view.backgroundColor = .clear
    view.font = UIFont.preferredFont(forTextStyle: .body)
    view.adjustsFontForContentSizeCategory = true
    view.textContainerInset = .zero
    view.textContainer.lineFragmentPadding = 0
    view.delegate = context.coordinator
    view.text = text
    return view
  }

  func updateUIView(_ view: UITextView, context: Context) {
    context.coordinator.parent = self
    if view.text != text { view.text = text }
  }

  final class Coordinator: NSObject, UITextViewDelegate {
    var parent: SelectableTextView

    init(_ parent: SelectableTextView) { self.parent = parent }

    func textViewDidChangeSelection(_ view: UITextView) {
      let range = view.selectedRange
      // After the update that reported it: a binding is not set while the view updates.
      DispatchQueue.main.async { self.parent.selection = range }
    }
  }
}

/// The menu row's title for opening what was found: the app's words, not the URL scheme's.
func detectedOpenTitle(_ kind: DetectedKind) -> LocalizedStringKey {
  switch kind {
  case .web: "detected_open_web"
  case .phone: "detected_call"
  case .email: "detected_write"
  case .address: "detected_map"
  }
}

/// SF Symbols for the kinds (no emoji as icons).
func detectedSymbol(_ kind: DetectedKind) -> String {
  switch kind {
  case .web: "link"
  case .phone: "phone"
  case .email: "envelope"
  case .address: "map"
  }
}

/// A phone number or e-mail address to add to the contacts (#2047).
struct NewContactTarget: Identifiable {
  let id = UUID()
  let contact: CNMutableContact
}

/// The system's "Create New Contact / Add to Existing Contact" screen for one value. It needs no
/// permission to read the contacts: the system shows and saves it.
struct UnknownContactView: UIViewControllerRepresentable {
  let contact: CNMutableContact

  func makeUIViewController(context: Context) -> UINavigationController {
    let controller = CNContactViewController(forUnknownContact: contact)
    controller.contactStore = CNContactStore()
    controller.allowsActions = false
    return UINavigationController(rootViewController: controller)
  }

  func updateUIViewController(_ controller: UINavigationController, context: Context) {}
}
