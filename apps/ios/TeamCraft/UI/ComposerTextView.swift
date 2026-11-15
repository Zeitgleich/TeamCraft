import SwiftUI
import UIKit

/// What the toolbar and the edit menu read and do: the formats at the selection, and applying one.
@MainActor
final class ComposerFormatController: ObservableObject {
  /// The formats the selection has now (the buttons' pressed state).
  @Published fileprivate(set) var active: Set<ComposerFormat> = []
  fileprivate var applyHandler: ((ComposerFormat) -> Void)?
  /// Where the caret is in the text now (a UTF-16 offset), for the suggestions at the cursor (#2051).
  /// Nil until the field reported it: the end of the text.
  @Published fileprivate(set) var caret: Int?
  /// Where the caret goes the next time the text is set from outside (a suggestion picked).
  fileprivate var pendingCaret: Int?

  /// A text set by `pick` puts the caret at `offset` instead of the end.
  func place(caret offset: Int) {
    pendingCaret = offset
    caret = offset
  }

  func apply(_ format: ComposerFormat) { applyHandler?(format) }
}

/// The composer's text field (#1051): a `UITextView` holding the Markdown that is sent, styled as it
/// will render. Styling is attributes only, applied when the keyboard isn't composing; formatting and
/// the block keys edit the text through the core (`ComposerFormatting.swift`).
struct ComposerTextView: UIViewRepresentable {
  @Binding var text: String
  @Binding var isFocused: Bool
  let controller: ComposerFormatController
  let accessibilityLabel: String
  /// Pictures pasted into the field, or inserted by the keyboard (GIFs, stickers); the field has none of its own.
  var onPasteImages: ([PastedImage]) -> Void = { _ in }
  /// Hardware keyboard (#2075): Command-Return sends; Up in an empty field edits the last message.
  var onSend: () -> Void = {}
  var onEditLast: () -> Void = {}

  @Environment(\.colorScheme) private var colorScheme
  @Environment(\.dynamicTypeSize) private var dynamicTypeSize

  /// The field grows to this many lines, then scrolls.
  private static let maxLines: CGFloat = 6
  private static let verticalInset: CGFloat = 10

  func makeCoordinator() -> Coordinator { Coordinator(self) }

  func makeUIView(context: Context) -> UITextView {
    let view = PastingTextView()
    view.onPasteImages = onPasteImages
    view.onSend = onSend
    view.onEditLast = onEditLast
    view.delegate = context.coordinator
    view.backgroundColor = .clear
    view.textContainerInset = UIEdgeInsets(top: Self.verticalInset, left: 0, bottom: Self.verticalInset, right: 0)
    view.textContainer.lineFragmentPadding = 0
    view.isScrollEnabled = true
    view.accessibilityLabel = accessibilityLabel
    view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    context.coordinator.view = view
    context.coordinator.install(on: controller)
    view.text = text
    context.coordinator.restyle(view, force: true)
    return view
  }

  func updateUIView(_ view: UITextView, context: Context) {
    let coordinator = context.coordinator
    coordinator.parent = self
    coordinator.install(on: controller)
    (view as? PastingTextView)?.onPasteImages = onPasteImages
    (view as? PastingTextView)?.onSend = onSend
    (view as? PastingTextView)?.onEditLast = onEditLast
    // A text set from outside (a mention picked, the draft restored, the field cleared). Not while the
    // keyboard composes: that text is the keyboard's.
    if view.markedTextRange == nil, !(view.text as NSString).isEqual(to: text) {
      view.text = text
      let end = (text as NSString).length
      view.selectedRange = NSRange(location: min(controller.pendingCaret ?? end, end), length: 0)
      controller.pendingCaret = nil
      coordinator.restyle(view, force: true)
      coordinator.refreshActive(view)
    } else if coordinator.lookKey != "\(colorScheme)/\(dynamicTypeSize)" {
      coordinator.restyle(view, force: true)
    }
    if isFocused, !view.isFirstResponder {
      DispatchQueue.main.async { view.becomeFirstResponder() }
    } else if !isFocused, view.isFirstResponder {
      DispatchQueue.main.async { view.resignFirstResponder() }
    }
  }

  func sizeThatFits(_ proposal: ProposedViewSize, uiView view: UITextView, context: Context) -> CGSize? {
    let width = proposal.width ?? 300
    let line = view.font?.lineHeight ?? 20
    let content = view.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
    let maximum = line * Self.maxLines + Self.verticalInset * 2
    return CGSize(width: width, height: min(max(content, line + Self.verticalInset * 2), maximum))
  }

  // MARK: - Coordinator

  @MainActor
  final class Coordinator: NSObject, UITextViewDelegate {
    var parent: ComposerTextView
    weak var view: UITextView?
    /// What the styling was last made for, to restyle when the look changes (dark mode, text size).
    var lookKey = ""
    /// Whether the text was too long to style and the attributes are plain.
    private var isPlain = false
    private var isApplying = false

    init(_ parent: ComposerTextView) { self.parent = parent }

    func install(on controller: ComposerFormatController) {
      controller.applyHandler = { [weak self] format in self?.apply(format) }
    }

    // MARK: Delegate

    func textViewDidChange(_ view: UITextView) {
      // The binding follows every change, a composing one too (as a `TextField` does); the styling
      // waits until the keyboard has committed its text.
      if !(parent.text as NSString).isEqual(to: view.text) { parent.text = view.text }
      restyle(view, force: false)
      refreshActive(view)
    }

    func textViewDidChangeSelection(_ view: UITextView) {
      refreshActive(view)
      let end = view.selectedRange.location + view.selectedRange.length
      if parent.controller.caret != end { parent.controller.caret = end }
    }

    func textViewDidBeginEditing(_ view: UITextView) {
      if !parent.isFocused { parent.isFocused = true }
    }

    func textViewDidEndEditing(_ view: UITextView) {
      if parent.isFocused { parent.isFocused = false }
    }

    func textView(_ view: UITextView, shouldChangeTextIn range: NSRange, replacementText replacement: String) -> Bool {
      // Never while the keyboard composes (Japanese, Chinese, Korean): its text isn't ours to edit.
      guard !isApplying, view.markedTextRange == nil,
        let edit = ComposerKeys.edit(
          text: view.text, selection: view.selectedRange, range: range, replacement: replacement)
      else { return true }
      apply(edit, to: view)
      return false
    }

    func textView(
      _ view: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]
    ) -> UIMenu? {
      let active = parent.controller.active
      let actions = ComposerFormatting.formats.map { format in
        UIAction(
          title: ComposerFormatting.name(of: format),
          image: UIImage(systemName: ComposerFormatting.symbol(of: format)),
          state: active.contains(format) ? .on : .off
        ) { [weak self] _ in self?.apply(format) }
      }
      let menu = UIMenu(title: String(localized: "composer_format_menu"), children: actions)
      return UIMenu(children: suggestedActions + [menu])
    }

    // MARK: Formatting

    func apply(_ format: ComposerFormat) {
      // A press during the keyboard's composition does nothing: that text isn't ours to edit.
      guard let view, view.markedTextRange == nil else { return }
      let range = view.selectedRange
      let edit = composerApplyFormat(
        text: view.text, start: UInt32(range.location), end: UInt32(range.location + range.length), format: format)
      apply(edit, to: view)
    }

    /// Takes the core's edit as one undoable replacement of the piece that changed, then the selection.
    func apply(_ edit: ComposerEdit, to view: UITextView) {
      if !(view.text as NSString).isEqual(to: edit.text) {
        let change = ComposerDiff.change(from: view.text, to: edit.text)
        isApplying = true
        if let start = view.position(from: view.beginningOfDocument, offset: change.range.location),
          let end = view.position(from: start, offset: change.range.length),
          let textRange = view.textRange(from: start, to: end)
        {
          view.replace(textRange, withText: change.replacement)
        }
        isApplying = false
      }
      view.selectedRange = NSRange(
        location: Int(edit.selectionStart), length: Int(edit.selectionEnd) - Int(edit.selectionStart))
      if !(parent.text as NSString).isEqual(to: view.text) { parent.text = view.text }
      restyle(view, force: false)
      refreshActive(view)
    }

    // MARK: Styling

    func restyle(_ view: UITextView, force: Bool) {
      guard view.markedTextRange == nil else { return }
      let palette = makePalette()
      lookKey = "\(parent.colorScheme)/\(parent.dynamicTypeSize)"
      let spans = composerStyleSpans(text: view.text)
      // A text too long to style stays as plain as it already is.
      if spans.isEmpty, isPlain, !force, view.textStorage.length > 0 { return }
      isPlain = spans.isEmpty
      ComposerStyler.apply(spans, to: view.textStorage, palette: palette)
      view.font = palette.font
      view.typingAttributes = ComposerStyler.baseAttributes(palette)
    }

    func refreshActive(_ view: UITextView) {
      let range = view.selectedRange
      let active = Set(
        composerActiveFormats(text: view.text, start: UInt32(range.location), end: UInt32(range.location + range.length)))
      if active != parent.controller.active { parent.controller.active = active }
    }

    private func makePalette() -> ComposerPalette {
      let traits = UITraitCollection(userInterfaceStyle: parent.colorScheme == .dark ? .dark : .light)
      func color(_ color: Color) -> UIColor { UIColor(color).resolvedColor(with: traits) }
      return ComposerPalette(
        font: Self.baseFont(),
        text: color(Theme.text),
        marker: color(Theme.textMuted),
        quote: color(Theme.textMuted),
        link: color(Theme.accent),
        mention: color(Theme.accent),
        codeBackground: color(Theme.text).withAlphaComponent(0.08))
    }

    /// The body font in the chosen interface font, scaled with Dynamic Type.
    private static func baseFont() -> UIFont {
      let metrics = UIFontMetrics(forTextStyle: .body)
      if let family = AppFont.current.family, let font = UIFont(name: family, size: 17) {
        return metrics.scaledFont(for: font)
      }
      return UIFont.preferredFont(forTextStyle: .body)
    }
  }
}

/// A text view that takes pictures from the pasteboard as attachments (issue #2053): Paste is offered
/// for a picture, and a picture without text goes to `onPasteImages` instead of into the text.
final class PastingTextView: UITextView {
  var onPasteImages: ([PastedImage]) -> Void = { _ in }
  var onSend: () -> Void = {}
  var onEditLast: () -> Void = {}

  /// The shortcuts of the field (#2075), listed in the iPad's shortcut overlay. Up is the field's only
  /// while it is empty: otherwise it moves the caret.
  override var keyCommands: [UIKeyCommand]? {
    var commands = super.keyCommands ?? []
    let send = UIKeyCommand(input: "\r", modifierFlags: .command, action: #selector(sendFromKeyboard))
    send.discoverabilityTitle = String(localized: "shortcut_send")
    send.wantsPriorityOverSystemBehavior = true
    commands.append(send)
    if text.isEmpty {
      let edit = UIKeyCommand(input: UIKeyCommand.inputUpArrow, modifierFlags: [], action: #selector(editLastFromKeyboard))
      edit.discoverabilityTitle = String(localized: "shortcut_edit_last")
      edit.wantsPriorityOverSystemBehavior = true
      commands.append(edit)
    }
    return commands
  }

  @objc private func sendFromKeyboard() { onSend() }
  @objc private func editLastFromKeyboard() { onEditLast() }

  override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
    if action == #selector(paste(_:)), UIPasteboard.general.hasImages { return true }
    return super.canPerformAction(action, withSender: sender)
  }

  override func paste(_ sender: Any?) {
    let pasteboard = UIPasteboard.general
    if !pasteboard.hasStrings {
      let images = PastedImage.read(from: pasteboard)
      if !images.isEmpty {
        onPasteImages(images)
        return
      }
    }
    super.paste(sender)
  }
}

/// The formatting toolbar above the field: bold, italic, strikethrough, code, code block, quote,
/// lists and link, as on desktop and web. A button is selected where the caret has the format;
/// pressing it again takes the format off. VoiceOver reads each button's name and state.
struct ComposerFormatBar: View {
  @ObservedObject var controller: ComposerFormatController

  var body: some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 2) {
        ForEach(ComposerFormatting.formats, id: \.self) { format in
          let isActive = controller.active.contains(format)
          Button { controller.apply(format) } label: {
            Image(systemName: ComposerFormatting.symbol(of: format))
              .font(.app(.body))
              .foregroundStyle(isActive ? Theme.accentStrong : Theme.text)
              .frame(width: 44, height: 44)
              .background(isActive ? Theme.accentSoft : Color.clear, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
              .contentShape(Rectangle())
          }
          .buttonStyle(.plain)
          .accessibilityLabel(Text(ComposerFormatting.name(of: format)))
          .accessibilityAddTraits(isActive ? .isSelected : [])
        }
      }
      .padding(.horizontal, 4)
    }
    .accessibilityElement(children: .contain)
  }
}
