import QuickLook
import SwiftUI

/// A text or delimited file of a message, in the app (#2342): the same decoding, separator sniffing,
/// table parser and find as desktop and web (the core, through `TextPreview`). Rows are drawn as they
/// scroll into view and asked from the core one at a time, so a large file costs the app no memory
/// of its own; reading and parsing run off the main thread inside the core (#2184).
struct TextPreviewView: View {
  @Environment(\.dismiss) private var dismiss
  let target: TextPreviewTarget

  @State private var phase = Phase.loading
  @State private var maybePreview: TextPreview?
  @State private var maybeInfo: TextPreviewInfo?
  @State private var table = TableState.none
  @State private var maybeQuickLookURL: URL?

  @State private var showsTable: Bool
  @State private var separator: SeparatorChoice
  @State private var maybeHeader: Bool?
  @State private var isWrapped: Bool
  @State private var isFinding = false
  @State private var isCustomSeparatorShown = false
  @State private var customSeparatorText = ""
  @State private var query = ""
  @State private var lineHits: [LineMatch] = []
  @State private var cellHits: [CellHit] = []
  @State private var isCapped = false
  @State private var currentHit = 0

  private let typeKey: String

  init(target: TextPreviewTarget) {
    self.target = target
    let key = TextPreviewKind.typeKey(fileName: target.fileName, mediaType: target.mediaType)
    typeKey = key
    let defaults = UserDefaults.standard
    _showsTable = State(initialValue: defaults.object(forKey: "textPreview.\(key).table") as? Bool ?? true)
    _separator = State(initialValue: SeparatorChoice(stored: defaults.string(forKey: "textPreview.\(key).sep")))
    _maybeHeader = State(initialValue: defaults.object(forKey: "textPreview.\(key).header") as? Bool)
    _isWrapped = State(initialValue: defaults.bool(forKey: "textPreview.\(key).wrap"))
  }

  var body: some View {
    NavigationStack {
      content
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .navigationTitle(target.fileName)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .searchable(text: $query, isPresented: $isFinding, placement: .navigationBarDrawer(displayMode: .automatic), prompt: Text("text_preview_find_hint"))
        .onSubmit(of: .search) { step(1) }
        .quickLookPreview($maybeQuickLookURL)
    }
    .task(id: target.id) { await open() }
    .task(id: separator) { await parse() }
    .task(id: FindKey(query: query, showsTable: effectiveShowsTable, table: table.info)) { await find() }
    .alert("text_preview_separator_custom_label", isPresented: $isCustomSeparatorShown) {
      TextField("text_preview_separator_custom_label", text: $customSeparatorText)
        .autocorrectionDisabled()
        .textInputAutocapitalization(.never)
      Button("text_preview_apply") { applyCustomSeparator() }
      Button("text_preview_cancel", role: .cancel) {}
    } message: {
      Text("text_preview_separator_custom_invalid")
    }
  }

  // MARK: - Content

  @ViewBuilder
  private var content: some View {
    switch phase {
    case .loading:
      ProgressView()
    case .failed(let message):
      VStack(spacing: 12) {
        Text(message).multilineTextAlignment(.center)
        Button("text_preview_open_elsewhere") { maybeQuickLookURL = URL(fileURLWithPath: target.path) }
      }
      .padding()
    case .ready:
      if let preview = maybePreview, let info = maybeInfo {
        VStack(spacing: 0) {
          notes(info)
          if effectiveShowsTable, let tableInfo = table.info {
            TableBody(
              preview: preview, info: tableInfo, hasHeader: hasHeader, isWrapped: isWrapped,
              hits: cellHits, currentHit: currentCellHit)
          } else if table == .parsing && showsTable {
            ProgressView().frame(maxHeight: .infinity)
          } else {
            LinesBody(
              preview: preview, lineCount: Int(info.lineCount), isWrapped: isWrapped,
              hits: lineHits, currentHit: currentLineHit)
          }
        }
      }
    }
  }

  @ViewBuilder
  private func notes(_ info: TextPreviewInfo) -> some View {
    VStack(alignment: .leading, spacing: 2) {
      if info.isTruncated { note("text_preview_truncated") }
      if info.encoding == .utf8Repaired { note("text_preview_encoding_repaired") }
      if case .refused(let reason) = table {
        switch reason {
        case .notATable: note("text_preview_not_a_table")
        case .unterminatedQuote(let line):
          Text(String(format: String(localized: "text_preview_unterminated_quote"), Int(line)))
            .font(.app(.footnote)).foregroundStyle(Theme.textMuted)
        }
      }
      if table.info?.hasDroppedColumns == true { note("text_preview_columns_dropped") }
    }
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.horizontal)
  }

  private func note(_ key: LocalizedStringKey) -> some View {
    Text(key).font(.app(.footnote)).foregroundStyle(Theme.textMuted)
  }

  // MARK: - Toolbar

  @ToolbarContentBuilder
  private var toolbar: some ToolbarContent {
    ToolbarItem(placement: .cancellationAction) {
      Button("done") { dismiss() }
    }
    ToolbarItem(placement: .primaryAction) {
      Menu {
        if table.info != nil {
          Picker("text_preview_view", selection: viewBinding) {
            Text("text_preview_view_table").tag(true)
            Text("text_preview_view_text").tag(false)
          }
        }
        if effectiveShowsTable {
          separatorMenu
          Toggle("text_preview_first_row_header", isOn: headerBinding)
        }
        Toggle("text_preview_wrap", isOn: wrapBinding)
        Button("text_preview_open_elsewhere", systemImage: "square.and.arrow.up") {
          maybeQuickLookURL = URL(fileURLWithPath: target.path)
        }
      } label: {
        Image(systemName: "ellipsis.circle").accessibilityLabel(Text("text_preview_options"))
      }
    }
    if matchCount > 0 {
      ToolbarItemGroup(placement: .bottomBar) {
        Button { step(-1) } label: { Image(systemName: "chevron.up") }
          .accessibilityLabel(Text("text_preview_find_previous"))
        Text(matchSummary).font(.app(.footnote)).monospacedDigit()
        Button { step(1) } label: { Image(systemName: "chevron.down") }
          .accessibilityLabel(Text("text_preview_find_next"))
      }
    }
  }

  @ViewBuilder
  private var separatorMenu: some View {
    Menu("text_preview_separator") {
      Picker("text_preview_separator", selection: separatorBinding) {
        Text("text_preview_separator_auto").tag(SeparatorChoice.auto)
        ForEach(SeparatorChoice.fixed, id: \.self) { choice in Text(choice.label).tag(choice) }
        if case .custom(let text) = separator { Text("\"\(text)\"").tag(separator) }
      }
      Button("text_preview_separator_custom") {
        customSeparatorText = ""
        isCustomSeparatorShown = true
      }
    }
  }

  // MARK: - State

  private var effectiveShowsTable: Bool { showsTable && table.info != nil }

  private var hasHeader: Bool { table.info.map { maybeHeader ?? $0.looksLikeHeader } ?? false }

  private var viewBinding: Binding<Bool> {
    Binding(
      get: { showsTable },
      set: {
        showsTable = $0
        UserDefaults.standard.set($0, forKey: "textPreview.\(typeKey).table")
      })
  }

  private var headerBinding: Binding<Bool> {
    Binding(
      get: { hasHeader },
      set: {
        maybeHeader = $0
        UserDefaults.standard.set($0, forKey: "textPreview.\(typeKey).header")
      })
  }

  private var wrapBinding: Binding<Bool> {
    Binding(
      get: { isWrapped },
      set: {
        isWrapped = $0
        UserDefaults.standard.set($0, forKey: "textPreview.\(typeKey).wrap")
      })
  }

  private var separatorBinding: Binding<SeparatorChoice> {
    Binding(
      get: { separator },
      set: {
        separator = $0
        UserDefaults.standard.set($0.stored, forKey: "textPreview.\(typeKey).sep")
      })
  }

  private func applyCustomSeparator() {
    guard SeparatorChoice.isUsable(customSeparatorText) else {
      // Keep the field open with its message: the alert's text says what is allowed.
      isCustomSeparatorShown = true
      return
    }
    separatorBinding.wrappedValue = .custom(customSeparatorText)
  }

  // MARK: - Loading and finding

  private func open() async {
    phase = .loading
    do {
      let preview = try await openTextPreview(path: target.path, maxBytes: TextPreviewKind.maxBytes)
      maybePreview = preview
      maybeInfo = preview.info()
      phase = .ready
      await parse()
    } catch TextPreviewError.notText {
      phase = .failed(String(localized: "text_preview_not_text"))
    } catch {
      phase = .failed(String(localized: "text_preview_unreadable"))
    }
  }

  private func parse() async {
    guard let preview = maybePreview, let info = maybeInfo else { return }
    let maybeChoice = separator.choice
    if maybeChoice == nil && info.maybeDetected == nil {
      table = .none
      return
    }
    table = .parsing
    do {
      table = .ready(try await preview.parseTable(maybeDelimiter: maybeChoice))
    } catch TextPreviewError.unterminatedQuote(let line) {
      table = .refused(.unterminatedQuote(line))
    } catch {
      table = .refused(.notATable)
    }
  }

  private func find() async {
    guard let preview = maybePreview else { return }
    currentHit = 0
    guard !query.isEmpty else {
      lineHits = []
      cellHits = []
      isCapped = false
      return
    }
    try? await Task.sleep(for: .milliseconds(250))
    if Task.isCancelled { return }
    if effectiveShowsTable {
      let found = await preview.findCells(query: query)
      cellHits = found.matches
      isCapped = found.isCapped
    } else {
      let found = await preview.findLines(query: query)
      lineHits = found.matches
      isCapped = found.isCapped
    }
  }

  private var matchCount: Int { effectiveShowsTable ? cellHits.count : lineHits.count }

  private var matchSummary: String {
    let format = String(localized: isCapped ? "text_preview_find_count_capped" : "text_preview_find_count")
    return String(format: format, currentHit + 1, matchCount)
  }

  private var currentLineHit: LineMatch? { lineHits.indices.contains(currentHit) ? lineHits[currentHit] : nil }
  private var currentCellHit: CellHit? { cellHits.indices.contains(currentHit) ? cellHits[currentHit] : nil }

  private func step(_ delta: Int) {
    guard matchCount > 0 else { return }
    currentHit = (currentHit + delta + matchCount) % matchCount
  }

  private struct FindKey: Hashable {
    let query: String
    let showsTable: Bool
    let table: TableInfo?
  }

  private enum Phase {
    case loading
    case failed(String)
    case ready
  }
}

// MARK: - Table state and choices

private enum TableState: Equatable {
  case none
  case parsing
  case ready(TableInfo)
  case refused(Refusal)

  enum Refusal: Equatable {
    case notATable
    case unterminatedQuote(UInt32)
  }

  var info: TableInfo? {
    if case .ready(let info) = self { return info }
    return nil
  }
}

/// The separator the user chose for this type of file.
private enum SeparatorChoice: Hashable {
  case auto
  case picked(String)
  case custom(String)

  static let fixed: [SeparatorChoice] = ["comma", "semicolon", "tab", "pipe", "colon"].map { .picked($0) }
  static let maxBytes = 8

  init(stored: String?) {
    guard let stored else {
      self = .auto
      return
    }
    if stored.hasPrefix("custom:") {
      let text = String(stored.dropFirst("custom:".count))
      self = Self.isUsable(text) ? .custom(text) : .auto
    } else {
      self = Self.fixed.contains(.picked(stored)) ? .picked(stored) : .auto
    }
  }

  var stored: String? {
    switch self {
    case .auto: return nil
    case .picked(let slug): return slug
    case .custom(let text): return "custom:\(text)"
    }
  }

  var choice: DelimiterChoice? {
    switch self {
    case .auto: return nil
    case .custom(let text): return .custom(text: text)
    case .picked(let slug):
      switch slug {
      case "comma": return .comma
      case "semicolon": return .semicolon
      case "tab": return .tab
      case "pipe": return .pipe
      default: return .colon
      }
    }
  }

  var label: LocalizedStringKey {
    switch self {
    case .auto: return "text_preview_separator_auto"
    case .custom(let text): return LocalizedStringKey("\"\(text)\"")
    case .picked(let slug):
      switch slug {
      case "comma": return "text_preview_separator_comma"
      case "semicolon": return "text_preview_separator_semicolon"
      case "tab": return "text_preview_separator_tab"
      case "pipe": return "text_preview_separator_pipe"
      default: return "text_preview_separator_colon"
      }
    }
  }

  /// 1 to 8 bytes without line breaks or quotes, as the core accepts.
  static func isUsable(_ text: String) -> Bool {
    !text.isEmpty && text.utf8.count <= maxBytes && !text.contains(where: { "\n\r\"'".contains($0) })
  }
}

// MARK: - Lines

private struct LinesBody: View {
  let preview: TextPreview
  let lineCount: Int
  let isWrapped: Bool
  let hits: [LineMatch]
  let currentHit: LineMatch?

  var body: some View {
    let byLine = Dictionary(grouping: hits, by: { Int($0.line) })
    let gutter = CGFloat(String(lineCount).count) * 9 + 16
    ScrollViewReader { proxy in
      ScrollView(isWrapped ? [.vertical] : [.vertical, .horizontal]) {
        LazyVStack(alignment: .leading, spacing: 0) {
          ForEach(0..<lineCount, id: \.self) { index in
            HStack(alignment: .firstTextBaseline, spacing: 8) {
              Text(String(index + 1))
                .font(.system(.footnote, design: .monospaced)).foregroundStyle(Theme.textMuted)
                .frame(width: gutter, alignment: .trailing)
              lineText(index, matches: byLine[index] ?? [])
                .font(.system(.footnote, design: .monospaced))
                .lineLimit(isWrapped ? nil : 1)
                .fixedSize(horizontal: !isWrapped, vertical: isWrapped)
                .textSelection(.enabled)
            }
            .id(index)
          }
        }
        .padding(.vertical, 4)
      }
      .onChange(of: currentHit) { _, hit in
        if let hit { withAnimation { proxy.scrollTo(Int(hit.line), anchor: .center) } }
      }
    }
  }

  private func lineText(_ index: Int, matches: [LineMatch]) -> Text {
    let line = preview.lines(start: UInt32(index), count: 1).first ?? ""
    return Text(highlighted(line, ranges: matches.map { Int($0.start)..<Int($0.end) }, current: currentHit.flatMap { Int($0.line) == index ? Int($0.start) : nil }))
  }
}

// MARK: - Table

private struct TableBody: View {
  let preview: TextPreview
  let info: TableInfo
  let hasHeader: Bool
  let isWrapped: Bool
  let hits: [CellHit]
  let currentHit: CellHit?

  var body: some View {
    let profile = preview.tableProfile(hasHeader: hasHeader)
    let firstBody = hasHeader ? 1 : 0
    let bodyRows = max(0, Int(info.rows) - firstBody)
    let gutter = CGFloat(String(info.rows).count) * 9 + 16
    let byCell = Dictionary(grouping: hits, by: { CellKey(row: Int($0.row), column: Int($0.column)) })
    ScrollViewReader { proxy in
      ScrollView([.vertical, .horizontal]) {
        LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
          Section {
            ForEach(0..<bodyRows, id: \.self) { index in
              rowView(index + firstBody, profile: profile, gutter: gutter, byCell: byCell, isHeader: false)
                .id(index)
            }
          } header: {
            if hasHeader {
              rowView(0, profile: profile, gutter: gutter, byCell: byCell, isHeader: true)
                .background(.bar)
            }
          }
        }
      }
      .onChange(of: currentHit) { _, hit in
        guard let hit else { return }
        let index = Int(hit.row) - firstBody
        if index >= 0 { withAnimation { proxy.scrollTo(index, anchor: .center) } }
      }
    }
  }

  private func rowView(_ row: Int, profile: [ColumnInfo], gutter: CGFloat, byCell: [CellKey: [CellHit]], isHeader: Bool) -> some View {
    let cells = preview.tableRows(start: UInt32(row), count: 1).first ?? []
    return HStack(alignment: .top, spacing: 0) {
      Text(String(row + 1))
        .font(.app(.caption)).foregroundStyle(Theme.textMuted)
        .frame(width: gutter, alignment: .trailing).padding(.trailing, 8).padding(.vertical, 6)
      ForEach(profile.indices, id: \.self) { column in
        cell(row: row, column: column, text: cells.indices.contains(column) ? cells[column] : "", info: profile[column], isHeader: isHeader, hits: byCell[CellKey(row: row, column: column)] ?? [])
      }
    }
  }

  private func cell(row: Int, column: Int, text: String, info: ColumnInfo, isHeader: Bool, hits: [CellHit]) -> some View {
    let shown = isHeader && text.isEmpty ? String(format: String(localized: "text_preview_header_cell"), column + 1) : text
    let current = currentHit.flatMap { Int($0.row) == row && Int($0.column) == column ? Int($0.start) : nil }
    return Text(highlighted(shown, ranges: hits.map { Int($0.start)..<Int($0.end) }, current: current))
      .font(.app(.caption).weight(isHeader ? .semibold : .regular))
      .lineLimit(isWrapped ? 6 : 1)
      .multilineTextAlignment(info.isNumeric && !isHeader ? .trailing : .leading)
      .frame(width: CGFloat(info.widthChars) * 8 + 20, alignment: info.isNumeric && !isHeader ? .trailing : .leading)
      .padding(.vertical, 6)
      .padding(.horizontal, 10)
      .contentShape(Rectangle())
      .contextMenu {
        Button("text_preview_copy_cell", systemImage: "doc.on.doc") {
          UIPasteboard.general.string = preview.tableCell(row: UInt32(row), column: UInt32(column))
        }
        Button("text_preview_copy_row", systemImage: "list.bullet.rectangle") {
          UIPasteboard.general.string = preview.tableRowText(row: UInt32(row))
        }
      }
      .accessibilityLabel(Text(shown))
  }

  private struct CellKey: Hashable {
    let row: Int
    let column: Int
  }
}

/// `text` with the match ranges (UTF-16 offsets) marked; the one starting at `current` stronger.
private func highlighted(_ text: String, ranges: [Range<Int>], current: Int?) -> AttributedString {
  var result = AttributedString(text)
  guard !ranges.isEmpty else { return result }
  let units = text.utf16
  for range in ranges where range.upperBound <= units.count && range.lowerBound < range.upperBound {
    let start = String.Index(utf16Offset: range.lowerBound, in: text)
    let end = String.Index(utf16Offset: range.upperBound, in: text)
    guard let lower = AttributedString.Index(start, within: result), let upper = AttributedString.Index(end, within: result)
    else { continue }
    result[lower..<upper].backgroundColor = Color.yellow.opacity(range.lowerBound == current ? 0.75 : 0.35)
  }
  return result
}
