import SwiftUI

/// A conversation's shared notes (issue #1456, e2ee.md §4.11): the page as desktop and web show it,
/// built by the core from the updates this device holds, and edited here as there (#2361): tap a
/// block to type in it, Return makes the next block, the keyboard's bar changes the block's kind,
/// moves or removes it; undo, redo and the version history sit in the toolbar. Who may not post
/// reads it. While the sheet is open the conversation's older history is read a page at a time,
/// since the page's edits are messages in it.
struct ChannelNotesView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  @Environment(\.scenePhase) private var scenePhase
  @AppStorage("share_typing") private var isSharingTyping = true
  let conversationId: String
  /// Opened from a mention (#2361): scrolled to the user's first mention once the page shows it.
  private let seeksMention: Bool

  @State private var maybePageId: String?
  @State private var hasSoughtMention = false
  @State private var maybePage: CanvasPageItem?
  @State private var history = OlderHistory.complete
  @State private var historyPages = 0
  @State private var files: [FileInfo] = []
  @State private var maybePresence: CanvasPresenceItem?
  /// The editor of the shown page; a new one for every page.
  @State private var maybeEditor: NotesEditor?
  @State private var isShowingHistory = false

  /// `maybeStartPageId`: the page to show first (`nil`: the channel's first page).
  init(conversationId: String, maybeStartPageId: String? = nil, seeksMention: Bool = false) {
    self.conversationId = conversationId
    self.seeksMention = seeksMention
    _maybePageId = State(initialValue: maybeStartPageId)
  }

  var body: some View {
    NavigationStack {
      ScrollViewReader { proxy in
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
        .onChange(of: maybePage) { _, page in scrollToMention(page, proxy: proxy) }
      }
      .background(Theme.background)
      .navigationTitle(Text("channel_notes_title"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { toolbar }
    }
    .sheet(isPresented: $isShowingHistory) {
      NotesHistoryView(
        conversationId: conversationId, pageId: maybePageId, files: files,
        canRestore: maybePage?.canEdit == true
      ) { version in
        maybeEditor?.edit(.restore(version: version))
      }
    }
    .task(id: maybePageId) { openEditor() }
    .task(id: ReadKey(changes: model.changeCount, pageId: maybePageId, editor: maybeEditor.map(ObjectIdentifier.init))) {
      await read()
    }
    .task(id: PresenceKey(changes: model.changeCount, pageId: maybePageId, isActive: scenePhase == .active)) {
      await followPresence()
    }
    .onChange(of: maybePageId) { left, _ in
      maybePresence = nil
      leave(left)
    }
    .onChange(of: scenePhase) { _, phase in
      if phase != .active {
        leave(maybePageId)
        maybeEditor?.flushNow()
      }
    }
    .onDisappear {
      leave(maybePageId)
      maybeEditor?.close()
    }
  }

  @ToolbarContentBuilder
  private var toolbar: some ToolbarContent {
    ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
    ToolbarItemGroup(placement: .topBarLeading) {
      if maybePage?.canEdit == true, let editor = maybeEditor {
        Button { editor.edit(.undo) } label: { Image(systemName: "arrow.uturn.backward") }
          .disabled(!editor.canUndo)
          .accessibilityLabel(Text("channel_notes_undo"))
        Button { editor.edit(.redo) } label: { Image(systemName: "arrow.uturn.forward") }
          .disabled(!editor.canRedo)
          .accessibilityLabel(Text("channel_notes_redo"))
      }
      Button {
        maybeEditor?.stopEditing()
        isShowingHistory = true
      } label: {
        Image(systemName: "clock.arrow.circlepath")
      }
      .accessibilityLabel(Text("channel_notes_history"))
    }
    ToolbarItemGroup(placement: .keyboard) {
      if let editor = maybeEditor, let focus = editor.maybeFocus,
        let block = maybePage?.blocks.first(where: { $0.id == focus.blockId })
      {
        NoteBlockMenu(block: block, blocks: maybePage?.blocks ?? [], editor: editor)
        Spacer()
        Button("channel_notes_done") { editor.stopEditing() }
      }
    }
  }

  /// A new editor for the shown page; the one before sends its waiting edits and is let go.
  private func openEditor() {
    maybeEditor?.close()
    let editor = NotesEditor(model: model, conversationId: conversationId, pageId: maybePageId)
    editor.onPage = { page in maybePage = page }
    maybeEditor = editor
  }

  /// Once, when the page first shows the user's mention: older edits may still be loading.
  private func scrollToMention(_ maybePage: CanvasPageItem?, proxy: ScrollViewProxy) {
    guard seeksMention, !hasSoughtMention, let page = maybePage, let me = model.maybeUserId,
      let blockId = ChannelNotes.firstMention(of: me, in: page)
    else { return }
    hasSoughtMention = true
    if UIAccessibility.isReduceMotionEnabled {
      proxy.scrollTo(blockId, anchor: .center)
    } else {
      withAnimation { proxy.scrollTo(blockId, anchor: .center) }
    }
  }

  @ViewBuilder
  private func content(_ page: CanvasPageItem) -> some View {
    if !page.pages.isEmpty { pagePicker(page) }
    if let presence = maybePresence, !presence.openBy.isEmpty || !presence.editing.isEmpty {
      presenceLine(presence)
    }
    note(page.canEdit ? "channel_notes_shared" : "channel_notes_read_only")
    if let key = ChannelNotes.statusKey(page, history: history) { note(key, isStatus: true) }
    if let failure = maybeEditor?.maybeFailure {
      Text(failure).font(.app(.footnote)).foregroundStyle(Theme.danger)
        .accessibilityAddTraits(.updatesFrequently)
    }
    if !page.isUnreadable {
      if page.blocks.isEmpty { empty(canEdit: page.canEdit) }
      let editor = page.canEdit ? maybeEditor : nil
      ForEach(page.blocks, id: \.id) { block in
        ChannelNoteBlock(
          block: block, maybeFile: ChannelNotes.file(of: block, in: files), maybeMeId: model.maybeUserId,
          maybeEditor: editor
        )
        .id(block.id)
      }
      if let editor {
        Button {
          editor.edit(.addBlock(maybeAfterId: page.blocks.last?.id, kind: .paragraph))
        } label: {
          Label("channel_notes_add_block", systemImage: "plus")
        }
        .font(.app(.body))
        .padding(.top, 4)
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

  private func empty(canEdit: Bool) -> some View {
    VStack(spacing: 4) {
      Text("channel_notes_empty_title").font(.app(.headline)).foregroundStyle(Theme.text)
      Text(canEdit ? "channel_notes_empty_editable" : "channel_notes_empty").font(.app(.subheadline))
        .foregroundStyle(Theme.textMuted)
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
    let read: CanvasPageItem?
    if let editor = maybeEditor?.maybeEditor {
      read = await model.canvasEditorPage(editor)
    } else {
      read = await model.canvasPage(conversationId, pageId: maybePageId)
    }
    if let page = read {
      maybePage = page
      // A page deleted meanwhile falls back to the first one, which then gets an editor of its own.
      if page.maybePageId != maybePageId { maybePageId = page.maybePageId }
    }
    if maybePage?.blocks.contains(where: { $0.maybeFile != nil }) == true {
      files = await model.files(conversationId: conversationId)
    }
    if let older = await model.olderHistory(conversationId: conversationId) { history = older }
    if history == .available && historyPages < ChannelNotes.maxHistoryPages {
      historyPages += 1
      await model.perform(.loadOlderHistory(conversationId: conversationId))
    }
  }

  /// The avatars and names of the others on the page. The avatars are decorative: the text names them.
  private func presenceLine(_ presence: CanvasPresenceItem) -> some View {
    HStack(spacing: 8) {
      if !presence.openBy.isEmpty {
        HStack(spacing: -6) {
          ForEach(presence.openBy.prefix(ChannelNotes.maxPresenceAvatars), id: \.userId) { person in
            SenderAvatar(name: person.displayName, userId: person.userId, size: 24)
          }
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
      }
      Text(verbatim: ChannelNotes.presenceText(presence))
        .font(.app(.footnote))
        .foregroundStyle(Theme.textMuted)
        .accessibilityAddTraits(.updatesFrequently)
    }
  }

  /// Who else has the page open, asked now, every `ChannelNotes.presenceCheck` and on every change
  /// while the app is active; the core says "open" when due. Leaving the page, the sheet or the
  /// foreground says "closed" (`leave`).
  private func followPresence() async {
    guard scenePhase == .active else { return }
    while !Task.isCancelled {
      if let presence = await model.canvasPresence(conversationId, pageId: maybePageId, isShared: isSharingTyping) {
        maybePresence = presence
      }
      try? await Task.sleep(for: ChannelNotes.presenceCheck)
    }
  }

  private func leave(_ pageId: String?) {
    let conversationId = conversationId
    let isShared = isSharingTyping
    Task { await model.leaveCanvasPage(conversationId, pageId: pageId, isShared: isShared) }
  }

  private struct ReadKey: Equatable {
    let changes: Int
    let pageId: String?
    let editor: ObjectIdentifier?
  }

  private struct PresenceKey: Equatable {
    let changes: Int
    let pageId: String?
    let isActive: Bool
  }
}

/// One block: its marker (bullet, number, check mark, quote bar) and its formatted text. With an
/// editor (#2361) its text is tapped to type in it and its check mark changes.
struct ChannelNoteBlock: View {
  let block: CanvasBlockItem
  let maybeFile: FileInfo?
  let maybeMeId: String?
  var maybeEditor: NotesEditor? = nil

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
        checkMark
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
        Group {
          if let editor = maybeEditor {
            NoteText(block: block, editor: editor) {
              Text(verbatim: ChannelNotes.plain(block.runs))
            }
          } else {
            Text(verbatim: ChannelNotes.plain(block.runs))
          }
        }
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

  @ViewBuilder
  private func text(_ style: Font.TextStyle) -> some View {
    let formatted = Text(ChannelNotes.attributed(block.runs, maybeMeId: maybeMeId))
    if let editor = maybeEditor {
      NoteText(block: block, editor: editor) { formatted }.font(.app(style))
    } else {
      formatted.font(.app(style))
    }
  }

  /// The check mark; a button that changes it where the page can be edited.
  @ViewBuilder
  private var checkMark: some View {
    let image = Image(systemName: block.isChecked ? "checkmark.square.fill" : "square")
      .foregroundStyle(block.isChecked ? Theme.accent : Theme.textMuted)
    let state = Text(block.isChecked ? "channel_notes_checked" : "channel_notes_unchecked")
    if let editor = maybeEditor {
      Button { editor.edit(.setChecked(blockId: block.id, isChecked: !block.isChecked)) } label: { image }
        .buttonStyle(.plain)
        .accessibilityLabel(state)
        .accessibilityAddTraits(block.isChecked ? .isSelected : [])
    } else {
      image.accessibilityLabel(state)
    }
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

/// A block's text where the page can be edited (#2361): `shown` until tapped, then a field the user
/// types in. The field's text is the user's while they type; a change of the block from elsewhere
/// (another member) is taken in once the own edits got their answer. Return splits the block (in
/// code it is a line break).
private struct NoteText<Shown: View>: View {
  let block: CanvasBlockItem
  let editor: NotesEditor
  @ViewBuilder let shown: () -> Shown

  var body: some View {
    if editor.maybeFocus?.blockId == block.id {
      NoteField(block: block, editor: editor)
    } else {
      let plain = ChannelNotes.plain(block.runs)
      Group {
        if plain.isEmpty {
          // An empty block still takes a tap.
          Text("channel_notes_empty_block").foregroundStyle(Theme.textMuted)
        } else {
          shown()
        }
      }
      .frame(maxWidth: .infinity, alignment: .leading)
      .contentShape(Rectangle())
      .onTapGesture { editor.startEditing(block.id, caret: plain.utf16.count) }
      .accessibilityAddTraits(.isButton)
      .accessibilityHint(Text("channel_notes_edit_block"))
    }
  }
}

/// The field of the block the user types in.
private struct NoteField: View {
  let block: CanvasBlockItem
  let editor: NotesEditor
  @State private var text: String
  /// The block's text as the own edits left it: what the next one starts from.
  @State private var synced: String
  @FocusState private var isFocused: Bool

  init(block: CanvasBlockItem, editor: NotesEditor) {
    self.block = block
    self.editor = editor
    let shown = ChannelNotes.plain(block.runs)
    _text = State(initialValue: shown)
    _synced = State(initialValue: shown)
  }

  var body: some View {
    let shown = ChannelNotes.plain(block.runs)
    TextField("", text: $text, axis: .vertical)
      .focused($isFocused)
      .onAppear { isFocused = true }
      .onChange(of: text) { _, next in typed(next) }
      .onChange(of: SyncKey(shown: shown, inFlight: editor.inFlight)) { _, key in
        if key.inFlight == 0, key.shown != synced, text == synced {
          synced = key.shown
          text = key.shown
        }
      }
  }

  private func typed(_ next: String) {
    guard next != synced else { return }
    if block.kind != .code, let lineBreak = next.firstIndex(of: "\n") {
      // Return: the first line break splits the block; further ones (a paste) become spaces.
      let caret = next[..<lineBreak].utf16.count
      var joined = next
      joined.remove(at: lineBreak)
      joined = joined.replacingOccurrences(of: "\n", with: " ")
      editor.edit(.enter(blockId: block.id, base: synced, text: joined, caret: UInt32(caret)))
      synced = joined
      text = joined
      return
    }
    editor.edit(.type(blockId: block.id, base: synced, text: next))
    synced = next
  }

  private struct SyncKey: Equatable {
    let shown: String
    let inFlight: Int
  }
}

/// The menu of the block typed in, on the keyboard's bar: its kind, moving it a place up or down,
/// removing it (undo brings it back).
struct NoteBlockMenu: View {
  let block: CanvasBlockItem
  let blocks: [CanvasBlockItem]
  let editor: NotesEditor

  private static let kinds: [(CanvasBlockKind, LocalizedStringKey)] = [
    (.paragraph, "channel_notes_kind_paragraph"),
    (.heading, "channel_notes_kind_heading"),
    (.heading2, "channel_notes_kind_heading2"),
    (.heading3, "channel_notes_kind_heading3"),
    (.bullet, "channel_notes_kind_bullet"),
    (.numbered, "channel_notes_kind_numbered"),
    (.check, "channel_notes_kind_check"),
    (.quote, "channel_notes_kind_quote"),
    (.code, "channel_notes_kind_code"),
  ]

  var body: some View {
    let index = blocks.firstIndex { $0.id == block.id } ?? -1
    Menu {
      ForEach(Self.kinds, id: \.0) { kind, label in
        Button {
          if kind != block.kind { editor.edit(.setKind(blockId: block.id, kind: kind)) }
        } label: {
          if kind == block.kind { Label(label, systemImage: "checkmark") } else { Text(label) }
        }
      }
      Divider()
      Button("channel_notes_move_up", systemImage: "arrow.up") { editor.edit(.moveUp(blockId: block.id)) }
        .disabled(index <= 0)
      Button("channel_notes_move_down", systemImage: "arrow.down") { editor.edit(.moveDown(blockId: block.id)) }
        .disabled(index < 0 || index >= blocks.count - 1)
      Button("channel_notes_delete_block", systemImage: "trash", role: .destructive) {
        editor.stopEditing()
        editor.edit(.remove(blockId: block.id))
      }
    } label: {
      Image(systemName: "ellipsis.circle")
    }
    .accessibilityLabel(Text("channel_notes_block_actions"))
  }
}

/// A page's version history as this device holds it (#2361, e2ee.md §4.11): who changed it and when,
/// newest first; a version opens read-only and, for who may edit, is restored as a new change on top.
struct NotesHistoryView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let conversationId: String
  let pageId: String?
  let files: [FileInfo]
  let canRestore: Bool
  let onRestore: (CanvasVersionKey) -> Void
  @State private var maybeVersions: [CanvasVersionItem]?

  var body: some View {
    NavigationStack {
      List {
        Section {
          if let versions = maybeVersions {
            if versions.isEmpty {
              Text("channel_notes_history_empty").foregroundStyle(Theme.textMuted)
            }
            ForEach(versions, id: \.key) { version in
              NavigationLink {
                NotesVersionView(
                  conversationId: conversationId, pageId: pageId, version: version, files: files,
                  canRestore: canRestore
                ) {
                  onRestore(version.key)
                  dismiss()
                }
              } label: {
                VStack(alignment: .leading, spacing: 2) {
                  Text(verbatim: Self.name(version)).font(.app(.body))
                  Text(verbatim: Self.when(version)).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
                }
              }
            }
          } else {
            ProgressView().frame(maxWidth: .infinity)
          }
        } footer: {
          Text("channel_notes_history_hint")
        }
      }
      .navigationTitle(Text("channel_notes_history"))
      .navigationBarTitleDisplayMode(.inline)
      .toolbar { ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } } }
    }
    .task(id: model.changeCount) {
      if let versions = await model.canvasVersions(conversationId, pageId: pageId) { maybeVersions = versions }
    }
  }

  static func name(_ version: CanvasVersionItem) -> String {
    version.displayName.isEmpty ? String(localized: "channel_notes_history_someone") : version.displayName
  }

  static func when(_ version: CanvasVersionItem) -> String {
    let from = Date(timeIntervalSince1970: TimeInterval(version.key.fromMs) / 1000)
    let until = Date(timeIntervalSince1970: TimeInterval(version.key.untilMs) / 1000)
    let formatter = DateIntervalFormatter()
    formatter.dateStyle = .medium
    formatter.timeStyle = .short
    return formatter.string(from: from, to: until)
  }
}

/// One version of a page, read-only, with "Restore this version" for who may edit.
private struct NotesVersionView: View {
  @Environment(AppModel.self) private var model
  let conversationId: String
  let pageId: String?
  let version: CanvasVersionItem
  let files: [FileInfo]
  let canRestore: Bool
  let onRestore: () -> Void
  @State private var maybeBlocks: [CanvasBlockItem]?
  @State private var isLoaded = false

  var body: some View {
    ScrollView {
      LazyVStack(alignment: .leading, spacing: 10) {
        Text(verbatim: "\(NotesHistoryView.name(version)) · \(NotesHistoryView.when(version))")
          .font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        if canRestore {
          Button("channel_notes_history_restore", action: onRestore).buttonStyle(.borderedProminent)
        }
        if let blocks = maybeBlocks {
          ForEach(blocks, id: \.id) { block in
            ChannelNoteBlock(block: block, maybeFile: ChannelNotes.file(of: block, in: files), maybeMeId: model.maybeUserId)
          }
        } else if isLoaded {
          Text("channel_notes_history_gone").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        } else {
          ProgressView().frame(maxWidth: .infinity)
        }
      }
      .padding(.horizontal, 16)
      .padding(.bottom, 32)
    }
    .background(Theme.background)
    .navigationTitle(Text("channel_notes_history"))
    .navigationBarTitleDisplayMode(.inline)
    .task(id: model.changeCount) {
      maybeBlocks = await model.canvasVersionPage(conversationId, pageId: pageId, version: version.key)
      isLoaded = true
    }
  }
}
