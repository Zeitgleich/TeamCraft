import SwiftUI

/// Do not disturb (notifications.md, "Mobile push delivery"): quick DND for a while, the weekly
/// schedule, and who or what still gets through. Everything is stored on the server, so every
/// device and the push routing see the same.
struct DndSettingsView: View {
  @Environment(AppModel.self) private var model

  @State private var schedule = DndSchedule.empty
  @State private var savedSchedule = DndSchedule.empty
  @State private var isLoaded = false
  @State private var isLoadFailed = false
  @State private var maybeUntil: Date?
  @State private var customEnd = Date().addingTimeInterval(2 * 3600)
  @State private var isPickingEnd = false
  @State private var names: [String: String] = [:]
  @State private var maybeFailure: LocalizedStringKey?
  @State private var isSaving = false

  var body: some View {
    Form {
      quickSection
      scheduleSection
      exceptionsSection
    }
    .navigationTitle("dnd_title")
    .navigationBarTitleDisplayMode(.inline)
    .toolbar {
      ToolbarItem(placement: .confirmationAction) {
        Button("dnd_save") { Task { await save() } }
          .disabled(!isLoaded || schedule == savedSchedule || isSaving)
      }
    }
    .task { await load() }
    .alert(
      "error_generic", isPresented: Binding(get: { maybeFailure != nil }, set: { if !$0 { maybeFailure = nil } })
    ) {
      Button("done") { maybeFailure = nil }
    } message: {
      if let failure = maybeFailure { Text(failure) }
    }
    .sheet(isPresented: $isPickingEnd) { customEndSheet }
  }

  // MARK: Quick DND

  private var quickSection: some View {
    Section {
      if let until = maybeUntil {
        Label("dnd_active_until \(until.formatted(date: .abbreviated, time: .shortened))", systemImage: "moon.fill")
        Button("dnd_end", role: .destructive) { Task { await endDnd() } }
      }
      Button("dnd_one_hour") { Task { await startDnd(.oneHour) } }
      Button("dnd_until_morning") { Task { await startDnd(.untilMorning) } }
      Button("dnd_until_custom") { isPickingEnd = true }
    } header: {
      Text("dnd_quick")
    } footer: {
      Text("dnd_quick_hint")
    }
  }

  private var customEndSheet: some View {
    NavigationStack {
      Form {
        DatePicker("dnd_end_time", selection: $customEnd, in: Date()...)
          .datePickerStyle(.graphical)
      }
      .navigationTitle("dnd_until_custom")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) { Button("cancel") { isPickingEnd = false } }
        ToolbarItem(placement: .confirmationAction) {
          Button("dnd_start") {
            isPickingEnd = false
            Task { await startDnd(.until(customEnd)) }
          }
        }
      }
    }
  }

  // MARK: Schedule

  private var scheduleSection: some View {
    Section {
      if isLoadFailed {
        Text("dnd_load_failed").foregroundStyle(Theme.danger)
      }
      ForEach($schedule.windows) { $window in
        NavigationLink {
          DndWindowEditor(window: $window)
        } label: {
          DndWindowSummary(window: window)
        }
      }
      .onDelete { offsets in schedule.windows.remove(atOffsets: offsets) }
      Button("dnd_add_range") { schedule.windows.append(.weeknights) }
        .disabled(!isLoaded || !schedule.canAddWindow)
    } header: {
      Text("dnd_schedule")
    } footer: {
      Text("dnd_schedule_hint \(DndSchedule.maxWindows)")
    }
  }

  // MARK: Exceptions

  private var exceptionsSection: some View {
    Section {
      Toggle("dnd_allows_urgent", isOn: $schedule.allowsUrgent)
      ForEach(schedule.allowUserIds, id: \.self) { userId in
        Text(names[userId] ?? userId)
      }
      .onDelete { offsets in schedule.allowUserIds.remove(atOffsets: offsets) }
      NavigationLink("dnd_add_person") {
        PersonPickerView(excluded: Set(schedule.allowUserIds)) { person in
          names[person.id] = person.displayName
          if !schedule.allowUserIds.contains(person.id) { schedule.allowUserIds.append(person.id) }
        }
      }
      .disabled(!isLoaded)
    } header: {
      Text("dnd_exceptions")
    } footer: {
      Text("dnd_exceptions_hint")
    }
  }

  // MARK: Actions

  private func load() async {
    maybeUntil = QuickDndMemory.until
    guard let json = await model.notificationSettingsJSON() else {
      isLoadFailed = true
      return
    }
    let loaded = NotificationSettingsDocument.schedule(in: json) ?? .empty
    schedule = loaded
    savedSchedule = loaded
    isLoaded = true
    isLoadFailed = false
    for userId in loaded.allowUserIds where names[userId] == nil {
      if let person = await model.person(userId) { names[userId] = person.displayName }
    }
  }

  private func save() async {
    isSaving = true
    defer { isSaving = false }
    if await model.saveDndSchedule(schedule) {
      savedSchedule = schedule
    } else {
      maybeFailure = "dnd_save_failed"
    }
  }

  private func startDnd(_ quick: QuickDnd) async {
    let end = quick.endDate(now: Date(), calendar: .current)
    if let until = await model.startDnd(until: end) {
      maybeUntil = until
    } else {
      maybeFailure = "dnd_save_failed"
    }
  }

  private func endDnd() async {
    if await model.endDnd() {
      maybeUntil = nil
    } else {
      maybeFailure = "dnd_save_failed"
    }
  }
}

/// "Mon, Tue … 22:00–07:00" for a schedule range.
struct DndWindowSummary: View {
  let window: DndWindow

  var body: some View {
    VStack(alignment: .leading, spacing: 2) {
      Text(DndWindowText.times(window))
      Text(DndWindowText.days(window)).font(.caption).foregroundStyle(Theme.textMuted)
    }
  }
}

enum DndWindowText {
  static func times(_ window: DndWindow, calendar: Calendar = .current) -> String {
    let today = Date()
    let start = MinuteOfDay.date(window.startMinute, on: today, calendar: calendar)
    let end = MinuteOfDay.date(window.endMinute, on: today, calendar: calendar)
    return "\(start.formatted(date: .omitted, time: .shortened)) – \(end.formatted(date: .omitted, time: .shortened))"
  }

  static func days(_ window: DndWindow, calendar: Calendar = .current) -> String {
    guard !window.weekdays.isEmpty else { return String(localized: "dnd_no_days") }
    return Weekday.sorted(window.weekdays).map { $0.shortName(calendar: calendar) }.joined(separator: ", ")
  }
}

/// Edits one range: weekdays, start and end.
struct DndWindowEditor: View {
  @Binding var window: DndWindow

  var body: some View {
    Form {
      Section("dnd_days") {
        ForEach(Weekday.allCases) { day in
          Toggle(day.shortName(), isOn: binding(for: day))
        }
      }
      Section {
        DatePicker("dnd_from", selection: minuteBinding(\.startMinute), displayedComponents: .hourAndMinute)
        DatePicker("dnd_to", selection: minuteBinding(\.endMinute), displayedComponents: .hourAndMinute)
      } footer: {
        if window.isOvernight { Text("dnd_overnight_hint") }
      }
    }
    .navigationTitle("dnd_range")
    .navigationBarTitleDisplayMode(.inline)
  }

  private func binding(for day: Weekday) -> Binding<Bool> {
    Binding(
      get: { window.weekdays.contains(day) },
      set: { isOn in
        var days = Set(window.weekdays)
        if isOn { days.insert(day) } else { days.remove(day) }
        window.weekdays = Weekday.sorted(days)
      })
  }

  private func minuteBinding(_ keyPath: WritableKeyPath<DndWindow, Int>) -> Binding<Date> {
    Binding(
      get: { MinuteOfDay.date(window[keyPath: keyPath], on: Date(), calendar: .current) },
      set: { window[keyPath: keyPath] = MinuteOfDay.minute(of: $0, calendar: .current) })
  }
}

/// Searches the directory for a person to let through DND.
struct PersonPickerView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let excluded: Set<String>
  let onPick: (PersonSummary) -> Void

  @State private var query = ""
  @State private var people: [PersonSummary] = []

  var body: some View {
    List(people.filter { !excluded.contains($0.id) }) { person in
      Button {
        onPick(person)
        dismiss()
      } label: {
        VStack(alignment: .leading) {
          Text(person.displayName)
          if let username = person.username { Text(verbatim: "@\(username)").font(.caption).foregroundStyle(Theme.textMuted) }
        }
      }
    }
    .searchable(text: $query, prompt: Text("dnd_search_people"))
    .task(id: query) {
      // Waits for typing to pause before asking the server.
      try? await Task.sleep(for: .milliseconds(300))
      guard !Task.isCancelled else { return }
      people = await model.searchPeople(query)
    }
    .navigationTitle("dnd_add_person")
    .navigationBarTitleDisplayMode(.inline)
  }
}

/// Mutes a conversation for a while (its notification settings on the server; the push routing
/// and the devices' own check follow it).
struct ConversationMuteMenu: View {
  @Environment(AppModel.self) private var model
  let conversationId: String

  @State private var maybeMutedUntil: Date?

  var body: some View {
    Menu {
      if let until = maybeMutedUntil {
        Text("mute_until \(until.formatted(date: .abbreviated, time: .shortened))")
        Button("mute_end", systemImage: "bell") { Task { await setMuted(until: nil) } }
      }
      ForEach(MutePreset.allCases) { preset in
        Button(preset.title) {
          Task { await setMuted(until: preset.mutedUntil(now: Date(), calendar: .current)) }
        }
      }
    } label: {
      // A toolbar shows the icon, a menu the title too.
      Label("mute_title", systemImage: maybeMutedUntil == nil ? "bell" : "bell.slash")
    }
    .task(id: conversationId) { maybeMutedUntil = await model.conversationMutedUntil(conversationId) }
  }

  private func setMuted(until maybeDate: Date?) async {
    if await model.setConversationMuted(conversationId, until: maybeDate) { maybeMutedUntil = maybeDate }
  }
}

extension MutePreset {
  var title: LocalizedStringKey {
    switch self {
    case .oneHour: return "mute_one_hour"
    case .eightHours: return "mute_eight_hours"
    case .untilMorning: return "mute_until_morning"
    case .oneWeek: return "mute_one_week"
    }
  }
}
