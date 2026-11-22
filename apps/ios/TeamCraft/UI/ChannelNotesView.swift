import SwiftUI

/// A channel's shared notes, read-only (issue #1456, e2ee.md §4.11): the page as desktop and web show
/// it, built by the core from the updates this device holds. Editing stays on desktop and web for
/// now; the note under the pages says so. While the sheet is open the channel's older history is
/// read a page at a time, since the page's edits are messages in it.
struct ChannelNotesView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String

  @State private var maybePageId: String?
  @State private var maybePage: CanvasPageItem?
  @State private var history = OlderHistory.complete
  @State private var historyPages = 0
  @State private var files: [FileInfo] = []

  var body: some View {
    NavigationStack {
      ScrollView {
        LazyVStack(alignment: .leading, spacing: 10) {
          if let page = maybePage {
            content(page)
          } else {
            ProgressView().frame(maxWidth: .infinity).padding()
          }
        }
        .padding(.horizontal, 16)
        .padding(.bottom, 32)
      }
      .background(Theme.background)
      .navigationTitle(Text("channel_notes_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } } }
    }
    .task(id: ReadKey(changes: model.changeCount, pageId: maybePageId)) { await read() }
  }

  @ViewBuilder
  private func content(_ page: CanvasPageItem) -> some View {
    if !page.pages.isEmpty { pagePicker(page) }
    note("channel_notes_read_only_phone")
    if let key = ChannelNotes.statusKey(page, history: history) { note(key, isStatus: true) }
    if !page.isUnreadable {
      if page.blocks.isEmpty {
        empty
      } else {
        ForEach(page.blocks, id: \.id) { block in
          ChannelNoteBlock(block: block, maybeFile: ChannelNotes.file(of: block, in: files))
        }
      }
    }
  }

  /// The first page and the further ones the first page lists; the shown one selected.
  private func pagePicker(_ page: CanvasPageItem) -> some View {
    ScrollView(.horizontal, showsIndicators: false) {
      HStack(spacing: 8) {
        pageChip(title: String(localized: "channel_notes_title"), isSelected: page.maybePageId == nil) {
          maybePageId = nil
        }
        ForEach(page.pages, id: \.pageId) { entry in
          let title = entry.title.isEmpty ? String(localized: "channel_notes_page_untitled") : entry.title
          pageChip(title: title, isSelected: page.maybePageId == entry.pageId) { maybePageId = entry.pageId }
        }
      }
      .padding(.vertical, 4)
    }
    .accessibilityElement(children: .contain)
    .accessibilityLabel(Text("channel_notes_pages"))
  }

  private func pageChip(title: String, isSelected: Bool, action: @escaping () -> Void) -> some View {
    Button(action: action) {
      Text(verbatim: title)
        .font(.app(.subheadline))
        .lineLimit(1)
        .frame(maxWidth: 200)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .foregroundStyle(isSelected ? Theme.onAccent : Theme.text)
        .background(isSelected ? AnyShapeStyle(Theme.accent) : AnyShapeStyle(Theme.surface), in: Capsule())
        .overlay(Capsule().stroke(isSelected ? Color.clear : Theme.border))
    }
    .buttonStyle(.plain)
    .accessibilityAddTraits(isSelected ? .isSelected : [])
  }

  private var empty: some View {
    VStack(spacing: 4) {
      Text("channel_notes_empty_title").font(.app(.headline)).foregroundStyle(Theme.text)
      Text("channel_notes_empty").font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
        .multilineTextAlignment(.center)
    }
    .frame(maxWidth: .infinity)
    .padding(.vertical, 24)
  }

  private func note(_ key: String, isStatus: Bool = false) -> some View {
    Text(LocalizedStringKey(key))
      .font(.app(.footnote))
      .foregroundStyle(Theme.textMuted)
      .accessibilityAddTraits(isStatus ? .updatesFrequently : [])
  }

  /// The page afresh, the channel's files where the page shows some, and one more page of older
  /// history while there is more and the limit isn't reached.
  private func read() async {
    if let page = await model.canvasPage(conversationId, pageId: maybePageId) { maybePage = page }
    if maybePage?.blocks.contains(where: { $0.maybeFile != nil }) == true {
      files = await model.files(conversationId: conversationId)
    }
    if let older = await model.olderHistory(conversationId: conversationId) { history = older }
    if history == .available && historyPages < ChannelNotes.maxHistoryPages {
      historyPages += 1
      await model.perform(.loadOlderHistory(conversationId: conversationId))
    }
  }

  private struct ReadKey: Equatable {
    let changes: Int
    let pageId: String?
  }
}

/// One block: its marker (bullet, number, check mark, quote bar) and its formatted text.
private struct ChannelNoteBlock: View {
  let block: CanvasBlockItem
  let maybeFile: FileInfo?

  var body: some View {
    switch block.kind {
    case .heading:
      text(.title2).fontWeight(.bold).padding(.top, 8).accessibilityAddTraits(.isHeader)
    case .heading2:
      text(.title3).fontWeight(.semibold).padding(.top, 4).accessibilityAddTraits(.isHeader)
    case .heading3:
      text(.headline).accessibilityAddTraits(.isHeader)
    case .bullet:
      marked("•")
    case .numbered:
      marked("\(block.number).")
    case .check:
      HStack(alignment: .firstTextBaseline, spacing: 8) {
        Image(systemName: block.isChecked ? "checkmark.square.fill" : "square")
          .foregroundStyle(block.isChecked ? Theme.accent : Theme.textMuted)
          .accessibilityLabel(Text(block.isChecked ? "channel_notes_checked" : "channel_notes_unchecked"))
        text(.body).foregroundStyle(block.isChecked ? Theme.textMuted : Theme.text)
      }
    case .quote:
      HStack(spacing: 12) {
        RoundedRectangle(cornerRadius: 1.5).fill(Theme.border).frame(width: 3)
        text(.body).foregroundStyle(Theme.textMuted)
      }
      .fixedSize(horizontal: false, vertical: true)
    case .code:
      ScrollView(.horizontal, showsIndicators: false) {
        Text(verbatim: ChannelNotes.plain(block.runs))
          .font(.system(.callout, design: .monospaced))
          .foregroundStyle(Theme.text)
          .padding(12)
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(Theme.text.opacity(0.06), in: RoundedRectangle(cornerRadius: 8))
    case .file:
      fileBlock
    case .paragraph:
      text(.body)
    }
  }

  private func text(_ style: Font.TextStyle) -> Text {
    Text(ChannelNotes.attributed(block.runs)).font(.app(style))
  }

  private func marked(_ marker: String) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: 8) {
      Text(verbatim: marker).font(.app(.body)).foregroundStyle(Theme.textMuted).frame(minWidth: 18, alignment: .trailing)
        .accessibilityHidden(block.kind == .bullet)
      text(.body).foregroundStyle(Theme.text)
    }
  }

  /// A file of the channel, shown from the message it was sent with like in the chat; named as
  /// missing when this device holds no such message.
  @ViewBuilder
  private var fileBlock: some View {
    if let file = maybeFile {
      AttachmentView(file: file)
    } else {
      HStack(spacing: 12) {
        Image(systemName: "doc").foregroundStyle(Theme.textMuted).accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 2) {
          Text(verbatim: ChannelNotes.plain(block.runs)).font(.app(.body)).foregroundStyle(Theme.text).lineLimit(2)
          Text("channel_notes_file_missing").font(.app(.caption)).foregroundStyle(Theme.textMuted)
        }
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      .padding(12)
      .background(Theme.surface, in: RoundedRectangle(cornerRadius: Theme.radius))
      .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.border))
    }
  }
}
