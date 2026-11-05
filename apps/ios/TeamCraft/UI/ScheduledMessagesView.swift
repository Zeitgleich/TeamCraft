import SwiftUI

/// How often the open list looks at the clock: a message's time passing changes its line.
private let clockRefreshSeconds: UInt64 = 30

/// Everything waiting to be sent, soonest first, each with what happens next
/// (docs/scheduled-messages.md).
struct ScheduledMessagesView: View {
  @Environment(AppModel.self) private var model
  @State private var maybeOverview: ScheduledOverview?
  @State private var nowMs = ScheduleTimes.ms(Date())
  @State private var maybeEditing: ScheduledMessage?
  @State private var maybeCancelling: ScheduledMessage?

  var body: some View {
    List {
      Section {
        Text("scheduled_note").font(.footnote).foregroundStyle(Theme.textMuted)
      }
      let messages = maybeOverview?.messages ?? []
      if messages.isEmpty {
        Text("scheduled_empty").foregroundStyle(Theme.textMuted)
      }
      ForEach(messages) { message in
        row(message)
      }
    }
    .navigationTitle("scheduled_title")
    .task(id: model.changeCount) { maybeOverview = await model.scheduledOverview() }
    .task {
      while !Task.isCancelled {
        try? await Task.sleep(nanoseconds: clockRefreshSeconds * 1_000_000_000)
        nowMs = ScheduleTimes.ms(Date())
      }
    }
    .sheet(item: $maybeEditing) { message in
      if let overview = maybeOverview {
        ScheduledEditor(message: message, overview: overview)
      }
    }
    .confirmationDialog(
      "scheduled_cancel_question",
      isPresented: Binding(get: { maybeCancelling != nil }, set: { if !$0 { maybeCancelling = nil } }),
      titleVisibility: .visible,
      presenting: maybeCancelling
    ) { message in
      Button("scheduled_cancel_confirm", role: .destructive) {
        Task { await model.perform(.cancelScheduled(id: message.id)) }
      }
      Button("scheduled_keep", role: .cancel) {}
    }
  }

  private func row(_ message: ScheduledMessage) -> some View {
    VStack(alignment: .leading, spacing: 4) {
      if let name = message.conversationName, !name.isEmpty {
        Text(verbatim: name).font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
      } else {
        Text("scheduled_unknown_conversation").font(.caption.weight(.semibold)).foregroundStyle(Theme.accent)
      }
      Text(ScheduleTimes.date(ms: message.dueAt), format: .dateTime.weekday().day().month().hour().minute())
        .font(.subheadline.weight(.semibold))
      Text(message.shownStatus(nowMs: nowMs).text)
        .font(.caption)
        .foregroundStyle(message.needsAttention ? Theme.danger : Theme.textMuted)
      Text(verbatim: message.body).lineLimit(4)
    }
    .swipeActions(edge: .trailing, allowsFullSwipe: false) {
      Button("scheduled_cancel", role: .destructive) { maybeCancelling = message }
    }
    .contextMenu {
      if message.status != .unavailable {
        Button("scheduled_send_now", systemImage: "paperplane") {
          Task { await model.perform(.sendScheduledNow(id: message.id)) }
        }
        Button("scheduled_edit", systemImage: "pencil") { maybeEditing = message }
      }
      Button("scheduled_cancel", systemImage: "trash", role: .destructive) { maybeCancelling = message }
    }
    .accessibilityActions {
      if message.status != .unavailable {
        Button("scheduled_send_now") { Task { await model.perform(.sendScheduledNow(id: message.id)) } }
        Button("scheduled_edit") { maybeEditing = message }
      }
    }
  }
}

/// Changes the text and time of a waiting message.
private struct ScheduledEditor: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let message: ScheduledMessage
  let overview: ScheduledOverview
  @State private var text = ""
  @State private var dueAt = Date()
  @State private var errors = FieldErrors<ScheduleField, ScheduleProblem>()
  @State private var wasRefused = false
  @FocusState private var isBodyFocused: Bool
  @AccessibilityFocusState private var focusedField: ScheduleField?

  var body: some View {
    NavigationStack {
      Form {
        Section {
          TextField("scheduled_edit_text", text: $text, axis: .vertical)
            .lineLimit(3...8)
            .focused($isBodyFocused)
            .accessibilityFocused($focusedField, equals: .body)
            .fieldError(errors[.body]?.message)
          FieldMessage(maybeError: errors[.body]?.message)
        }
        Section {
          DatePicker("schedule_custom_label", selection: $dueAt)
            .accessibilityFocused($focusedField, equals: .time)
            .fieldError(errors[.time]?.message)
          FieldMessage(maybeError: errors[.time]?.message)
        }
      }
      .navigationTitle("scheduled_edit")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }
        }
        ToolbarItem(placement: .confirmationAction) {
          Button("scheduled_save") { save() }
        }
      }
      .onChange(of: text) { _, _ in follow() }
      .onChange(of: dueAt) { _, _ in follow() }
    }
    .onAppear {
      text = message.body
      dueAt = ScheduleTimes.date(ms: message.dueAt)
    }
  }

  private func follow() {
    guard wasRefused else { return }
    errors = ScheduleTimes.errors(body: text, dueAtMs: ScheduleTimes.ms(dueAt), overview: overview)
  }

  private func save() {
    let dueAtMs = ScheduleTimes.ms(dueAt)
    let found = ScheduleTimes.errors(body: text, dueAtMs: dueAtMs, overview: overview)
    guard found.isValid else {
      errors = found
      wasRefused = true
      isBodyFocused = found.maybeFirst == .body
      focusedField = found.maybeFirst
      return
    }
    Task {
      await model.perform(.updateScheduled(id: message.id, body: text, dueAtMs: dueAtMs))
      dismiss()
    }
  }
}

/// The times to choose from in the composer. `onPick` gets the time; the composer does the rest.
struct SendLaterSheet: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let onPick: (Int64) -> Void
  @State private var maybeOverview: ScheduledOverview?
  @State private var customAt = Date(timeIntervalSinceNow: 3600)
  @State private var maybeCustomProblem: ScheduleProblem?

  var body: some View {
    NavigationStack {
      Form {
        Section {
          Text("schedule_online_hint").font(.footnote).foregroundStyle(Theme.textMuted)
        }
        if let overview = maybeOverview {
          Section {
            ForEach(presets(overview), id: \.0) { label, dueAtMs in
              Button { pick(dueAtMs) } label: {
                VStack(alignment: .leading, spacing: 2) {
                  Text(LocalizedStringKey(label))
                  Text(ScheduleTimes.date(ms: dueAtMs), format: .dateTime.weekday().day().month().hour().minute())
                    .font(.caption)
                    .foregroundStyle(Theme.textMuted)
                }
              }
            }
          }
          Section {
            DatePicker("schedule_custom_label", selection: $customAt)
              .fieldError(maybeCustomProblem?.message)
              .onChange(of: customAt) { _, new in
                if maybeCustomProblem != nil {
                  maybeCustomProblem = ScheduleTimes.timeProblem(dueAtMs: ScheduleTimes.ms(new), overview: overview)
                }
              }
            FieldMessage(maybeError: maybeCustomProblem?.message)
            Button("schedule_confirm") {
              let dueAtMs = ScheduleTimes.ms(customAt)
              maybeCustomProblem = ScheduleTimes.timeProblem(dueAtMs: dueAtMs, overview: overview)
              if maybeCustomProblem == nil { pick(dueAtMs) }
            }
          }
        }
      }
      .navigationTitle("schedule_heading")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }
        }
      }
      .task { maybeOverview = await model.scheduledOverview() }
    }
  }

  private func pick(_ dueAtMs: Int64) {
    onPick(dueAtMs)
    dismiss()
  }

  /// The offered times the workspace's rule allows.
  private func presets(_ overview: ScheduledOverview) -> [(String, Int64)] {
    let now = ScheduleTimes.ms(Date())
    let all: [(String, Int64?)] = [
      ("schedule_in_one_hour", ScheduleTimes.inOneHour(serverNowMs: overview.serverNow)),
      ("schedule_tomorrow", ScheduleTimes.tomorrowMorning(nowMs: now, calendar: .current)),
      ("schedule_monday", ScheduleTimes.mondayMorning(nowMs: now, calendar: .current)),
    ]
    return all.compactMap { label, maybeTime in
      guard let time = maybeTime, ScheduleTimes.timeProblem(dueAtMs: time, overview: overview) == nil else {
        return nil
      }
      return (label, time)
    }
  }
}

/// In the conversation list while a scheduled message waits for the user: nothing waits silently.
struct ScheduledAttentionSection: View {
  @Environment(AppModel.self) private var model

  var body: some View {
    if model.scheduledAttentionCount > 0 {
      Section {
        NavigationLink { ScheduledMessagesView() } label: {
          Label("scheduled_attention", systemImage: "clock.badge.exclamationmark")
        }
      }
    }
  }
}
