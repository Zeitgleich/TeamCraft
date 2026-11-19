import SwiftUI

/// Names listed under an option before "and N more".
private let maxListedVoters = 5
/// After the closing time, how long the card waits before the core counts again.
private let closingMargin: TimeInterval = 0.5
/// Longest wait for a closing time while the card is shown; a later one is counted on a change.
private let maxClosingWait: TimeInterval = 86_400

/// A poll in a message (#205, e2ee.md §4.12), as web, desktop and Android show it: the question,
/// each option with its count and share in words and a bar that only repeats them, voting in
/// place. A single choice picks or withdraws; with several answers each option toggles. Counts
/// stay hidden until the user voted or the poll closed when the poll says so. The poll's sender
/// can close it; a closing time ends it by itself (the server's clock decides what counts).
struct PollCardView: View {
  @Environment(AppModel.self) private var model
  let message: Message
  let poll: Poll
  /// The user may vote here (the conversation takes their messages).
  let canVote: Bool
  let displayNames: [String: String]
  @State private var isConfirmingClose = false

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      HStack(alignment: .firstTextBaseline, spacing: 8) {
        Image(systemName: "chart.bar")
          .foregroundStyle(.tint)
          .accessibilityHidden(true)
        Text(verbatim: poll.question)
          .font(.app(.subheadline).weight(.semibold))
          .fixedSize(horizontal: false, vertical: true)
      }
      .accessibilityElement(children: .combine)
      .accessibilityAddTraits(.isHeader)
      Text(verbatim: modeText)
        .font(.app(.caption))
        .foregroundStyle(.secondary)
      ForEach(Array(poll.options.enumerated()), id: \.offset) { index, option in
        PollOptionRow(
          option: option, poll: poll, isLeader: leader == index, maybeVoters: votersLine(option),
          maybeOnPress: pressAction(for: index))
      }
      if !poll.areResultsShown {
        Text(poll.results == "after_vote" ? "poll_results_hidden_vote" : "poll_results_hidden_close")
          .font(.app(.footnote))
          .foregroundStyle(.secondary)
      }
      if !poll.isKnownChoice {
        Text("poll_unknown_choice").font(.app(.footnote)).foregroundStyle(.secondary)
      }
      footer
    }
    .frame(minWidth: 220, alignment: .leading)
    .padding(.vertical, 4)
    .task(id: poll.isClosed ? nil : poll.maybeClosesAtMs) { await recountAtClosing() }
    .confirmationDialog("poll_close", isPresented: $isConfirmingClose, titleVisibility: .visible) {
      Button("poll_close", role: .destructive) {
        Task { await model.perform(.closePoll(conversationId: message.conversationId, messageId: message.id)) }
      }
      Button("cancel", role: .cancel) {}
    } message: {
      Text("poll_close_confirm")
    }
  }

  /// "3 votes · Closed" or "3 votes · Ends 18 Nov, 15:00", and "Close poll" for its sender.
  private var footer: some View {
    HStack(alignment: .firstTextBaseline) {
      Group {
        if poll.isClosed {
          Text("poll_votes \(Int(poll.voterCount))") + Text(verbatim: " · ") + Text("poll_closed")
        } else if let closesAt = poll.closesAt {
          Text("poll_votes \(Int(poll.voterCount))") + Text(verbatim: " · ")
            + Text("poll_closes_on \(closesAt.formatted(date: .abbreviated, time: .shortened))")
        } else {
          Text("poll_votes \(Int(poll.voterCount))")
        }
      }
      .font(.app(.caption))
      .foregroundStyle(.secondary)
      .monospacedDigit()
      Spacer(minLength: 8)
      if poll.canClose && !poll.isClosed {
        Button("poll_close") { isConfirmingClose = true }
          .font(.app(.caption).weight(.semibold))
          .buttonStyle(.borderless)
      }
    }
  }

  private var leader: Int? { poll.isClosed && poll.areResultsShown ? poll.maybeLeader : nil }

  /// "Choose one · Names hidden · After voting": how the poll is answered and what it shows.
  private var modeText: String {
    var parts = [String(localized: poll.isMultipleChoice ? "poll_multiple_choice" : "poll_single_choice")]
    if !poll.areVotersShown { parts.append(String(localized: "poll_hidden_voters")) }
    switch poll.results {
    case "always": break
    case "after_vote": parts.append(String(localized: "poll_results_after_vote"))
    default: parts.append(String(localized: "poll_results_after_close"))
    }
    return parts.joined(separator: " · ")
  }

  /// What pressing the option at `index` sends, as the core decides it; nil when nothing can be sent.
  private func pressAction(for index: Int) -> (() -> Void)? {
    guard canVote, poll.isKnownChoice, !poll.isClosed,
      let choices = pollChoicesAfterPressing(
        choice: poll.choice, chosen: poll.myChoices, optionCount: UInt32(poll.options.count),
        isClosed: poll.isClosed, option: UInt32(index))
    else { return nil }
    return {
      UIImpactFeedbackGenerator(style: .light).impactOccurred()
      Task {
        await model.perform(.voteInPoll(conversationId: message.conversationId, messageId: message.id, choices: choices))
      }
    }
  }

  /// The first names of who chose an option, then "and N more"; nil when nobody is named.
  private func votersLine(_ option: PollOption) -> String? {
    guard poll.areVotersShown, !option.voters.isEmpty else { return nil }
    let names = option.voters.prefix(maxListedVoters).map { displayNames[$0] ?? "" }.joined(separator: ", ")
    let rest = option.voters.count - maxListedVoters
    return rest > 0 ? "\(names) \(String(localized: "poll_voters_more \(rest)"))" : names
  }

  /// One wait, while the card is shown, until the closing time: then the core counts again (it
  /// takes the time as passed), so voting ends and held-back counts appear.
  private func recountAtClosing() async {
    guard !poll.isClosed, let closesAt = poll.closesAt else { return }
    let wait = closesAt.timeIntervalSinceNow + closingMargin
    guard wait <= maxClosingWait else { return }
    try? await Task.sleep(for: .seconds(max(wait, 0)))
    guard !Task.isCancelled else { return }
    model.noteTimePassed()
  }
}

/// One option: mark, text, count and share in words, the bar, and who chose it.
private struct PollOptionRow: View {
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let option: PollOption
  let poll: Poll
  let isLeader: Bool
  let maybeVoters: String?
  let maybeOnPress: (() -> Void)?

  var body: some View {
    VStack(alignment: .leading, spacing: 4) {
      Button {
        maybeOnPress?()
      } label: {
        VStack(alignment: .leading, spacing: 6) {
          HStack(spacing: 10) {
            Image(systemName: markSymbol)
              .foregroundStyle(option.isChosen ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
              .accessibilityHidden(true)
            Text(verbatim: option.text)
              .font(.app(.body).weight(isLeader ? .semibold : .regular))
              .frame(maxWidth: .infinity, alignment: .leading)
            if poll.areResultsShown {
              Text("poll_option_count \(Int(option.count)) \(Int(share))")
                .font(.app(.caption))
                .foregroundStyle(.secondary)
                .monospacedDigit()
                .accessibilityHidden(true)
            }
          }
          if poll.areResultsShown {
            Capsule()
              .fill(.secondary.opacity(0.25))
              .frame(height: 4)
              .overlay(alignment: .leading) {
                GeometryReader { geometry in
                  Capsule()
                    .fill(.tint)
                    .frame(width: geometry.size.width * CGFloat(share) / 100)
                }
              }
              .animation(reduceMotion ? nil : .smooth, value: share)
              .accessibilityHidden(true)
          }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .contentShape(RoundedRectangle(cornerRadius: 12))
        .background(
          RoundedRectangle(cornerRadius: 12).fill(option.isChosen ? AnyShapeStyle(.tint.opacity(0.12)) : AnyShapeStyle(.clear))
        )
        .overlay(
          RoundedRectangle(cornerRadius: 12).stroke(
            option.isChosen ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary.opacity(0.4)), lineWidth: 1))
      }
      .buttonStyle(.plain)
      .disabled(maybeOnPress == nil)
      .accessibilityLabel(Text(verbatim: option.text))
      .accessibilityValue(accessibilityValue)
      .accessibilityAddTraits(option.isChosen ? .isSelected : [])
      if let voters = maybeVoters {
        Text(verbatim: voters)
          .font(.app(.caption2))
          .foregroundStyle(.secondary)
          .padding(.leading, 42)
      }
    }
  }

  private var share: UInt32 {
    poll.areResultsShown ? pollSharePercent(count: option.count, voterCount: poll.voterCount) : 0
  }

  private var markSymbol: String {
    switch (poll.isMultipleChoice, option.isChosen) {
    case (true, true): "checkmark.square.fill"
    case (true, false): "square"
    case (false, true): "checkmark.circle.fill"
    case (false, false): "circle"
    }
  }

  private var accessibilityValue: Text {
    poll.areResultsShown
      ? Text("poll_votes \(Int(option.count))") + Text(verbatim: ", ") + Text("poll_percent \(Int(share))")
      : Text(verbatim: "")
  }
}

/// A field of the poll sheet, for the focus after a refused send.
private enum PollSheetField: Hashable {
  case question
  case option(Int)
  case closing
}

/// Makes a poll (#205), as the web dialog and Android do: a question, two or more options, several
/// answers or one, whether it names who voted, when it shows counts and when it ends. The rules are
/// the core's (`pollProblems`): every error at its field at once, the first one focused on a
/// refused send, then they follow the input (forms.md). "Hide who voted" says it isn't anonymous.
struct PollSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let conversationId: String
  @State private var question = ""
  @State private var options = ["", ""]
  @State private var isMultiple = false
  @State private var hidesVoters = false
  @State private var results = PollResultsItem.always
  @State private var closing = PollClosing.never
  @State private var closesAt = Date().addingTimeInterval(86_400)
  @State private var wasRefused = false
  @State private var isSending = false
  @State private var maybeFailure: AppFailure?
  @FocusState private var focused: PollSheetField?
  @AccessibilityFocusState private var accessibilityFocused: PollSheetField?
  /// The server's limits (#2120); the built-in ones show until they are read.
  @State private var limits = PollLimitsItem(maxOptions: 12, maxQuestionChars: 300, maxOptionChars: 100, maxOpenDays: 365)

  var body: some View {
    NavigationStack {
      ScrollViewReader { proxy in
        form(proxy: proxy)
      }
      .navigationTitle("poll_new_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }.disabled(isSending)
        }
      }
    }
    .interactiveDismissDisabled(isSending)
    .task { limits = await model.pollLimits() }
  }

  private func form(proxy: ScrollViewProxy) -> some View {
    let problems = wasRefused ? currentProblems() : []
    return Form {
      if let maybeFailure {
        Section {
          Label { Text(maybeFailure.message) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
            .foregroundStyle(Theme.danger)
        }
      }
      Section {
        TextField("poll_question", text: $question, axis: .vertical)
          .lineLimit(1...4)
          .focused($focused, equals: .question)
          .accessibilityFocused($accessibilityFocused, equals: .question)
          .fieldError(message(problems, .question))
          .onChange(of: question) { _, text in
            if text.count > Int(limits.maxQuestionChars) { question = String(text.prefix(Int(limits.maxQuestionChars))) }
          }
          .id(PollSheetField.question)
        FieldMessage(maybeError: message(problems, .question))
      }
      Section("poll_options") {
        ForEach(options.indices, id: \.self) { index in
          HStack {
            TextField(String(localized: "poll_option \(index + 1)"), text: optionBinding(index))
              .focused($focused, equals: .option(index))
              .accessibilityFocused($accessibilityFocused, equals: .option(index))
              .fieldError(message(problems, .option(index)))
            if options.count > 2 {
              Button {
                options.remove(at: index)
              } label: {
                Image(systemName: "minus.circle.fill").foregroundStyle(Theme.danger)
              }
              .buttonStyle(.borderless)
              .accessibilityLabel(Text("poll_remove_option \(index + 1)"))
            }
          }
          .id(PollSheetField.option(index))
          FieldMessage(maybeError: message(problems, .option(index)))
        }
        Button("poll_add_option", systemImage: "plus") { options.append("") }
          .disabled(options.count >= Int(limits.maxOptions))
      }
      Section {
        Toggle("poll_multiple", isOn: $isMultiple)
        Toggle("poll_hide_voters", isOn: $hidesVoters)
      } footer: {
        Text("poll_hide_voters_hint")
      }
      Section {
        Picker("poll_results", selection: $results) {
          Text("poll_results_always").tag(PollResultsItem.always)
          Text("poll_results_after_vote").tag(PollResultsItem.afterVote)
          Text("poll_results_after_close").tag(PollResultsItem.afterClose)
        }
        Picker("poll_closes", selection: $closing) {
          ForEach(PollClosing.allCases) { choice in
            Text(choice.label).tag(choice)
          }
        }
        if closing == .at {
          DatePicker("poll_closes_at_label", selection: $closesAt, in: Date()..., displayedComponents: [.date, .hourAndMinute])
            .focused($focused, equals: .closing)
            .accessibilityFocused($accessibilityFocused, equals: .closing)
            .fieldError(message(problems, .closing))
            .id(PollSheetField.closing)
        }
        FieldMessage(maybeError: message(problems, .closing))
      }
      Section {
        Button {
          send(proxy: proxy)
        } label: {
          HStack {
            Spacer()
            if isSending { ProgressView() } else { Text("poll_send").bold() }
            Spacer()
          }
        }
        .disabled(isSending)
      }
    }
    .disabled(isSending)
  }

  private func optionBinding(_ index: Int) -> Binding<String> {
    Binding(
      get: { options.indices.contains(index) ? options[index] : "" },
      set: { text in
        guard options.indices.contains(index) else { return }
        options[index] = String(text.prefix(Int(limits.maxOptionChars)))
      })
  }

  private func draft(nowMs: Int64) -> PollDraftItem {
    let maybeClosesAtMs: Int64? =
      switch closing {
      case .never: nil
      case .at: Int64(closesAt.timeIntervalSince1970 * 1000)
      default: closing.maybeAfter.map { nowMs + Int64($0 * 1000) }
      }
    return PollDraftItem(
      question: question, options: options, isMultipleChoice: isMultiple, hidesVoters: hidesVoters,
      results: results, maybeClosesAtMs: maybeClosesAtMs)
  }

  private var nowMs: Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

  private func currentProblems() -> [PollProblemItem] {
    let now = nowMs
    return pollProblems(draft: draft(nowMs: now), limits: limits, nowMs: now)
  }

  private func send(proxy: ScrollViewProxy) {
    let now = nowMs
    let problems = pollProblems(draft: draft(nowMs: now), limits: limits, nowMs: now)
    if let first = problems.first {
      wasRefused = true
      let field = sheetField(first.field)
      focused = field
      accessibilityFocused = field
      scrollToMiddle(field, in: proxy, reduceMotion: reduceMotion)
      return
    }
    let title = String(localized: "poll_title")
    let note = String(localized: "poll_fallback_note")
    guard
      let json = try? pollPostAction(
        conversation: conversationId, draft: draft(nowMs: now), limits: limits, title: title, note: note, nowMs: now)
    else {
      wasRefused = true
      return
    }
    isSending = true
    Task {
      maybeFailure = await model.performReporting(.postPoll(actionJson: json))
      isSending = false
      // The sheet closes once the core took the poll; on failure it stays with the input.
      if maybeFailure == nil { dismiss() }
    }
  }

  private func sheetField(_ field: PollFieldItem) -> PollSheetField {
    switch field {
    case .question: .question
    case .option(let index): .option(Int(index))
    case .closing: .closing
    }
  }

  /// The message of `field`'s first problem, in the user's words.
  private func message(_ problems: [PollProblemItem], _ field: PollSheetField) -> LocalizedStringKey? {
    guard let problem = problems.first(where: { sheetField($0.field) == field }) else { return nil }
    switch problem.kind {
    case .emptyQuestion: return "poll_error_question"
    case .questionTooLong(let max): return "poll_error_question_long \(Int(max))"
    case .tooFewOptions(let min): return "poll_error_options \(Int(min))"
    case .tooManyOptions(let max): return "poll_error_options_many \(Int(max))"
    case .optionTooLong(let max): return "poll_error_option_long \(Int(max))"
    case .duplicateOption: return "poll_error_option_duplicate"
    case .closingPassed: return "poll_error_closing_passed"
    case .closingTooFar(let maxDays): return "poll_error_closing_far \(Int(maxDays))"
    }
  }
}
