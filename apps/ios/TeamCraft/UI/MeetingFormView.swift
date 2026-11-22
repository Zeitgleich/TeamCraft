import SwiftUI

/// The lengths the form offers, as on desktop; a saved meeting's own length is offered too.
let meetingLengths: [UInt32] = [15, 30, 45, 60, 90, 120]

/// The fields of the form an error can belong to, in their order on the form.
enum MeetingField: Int, Comparable, Hashable {
  case title, start, length, externals, interval, weekdays, count, until

  static func < (lhs: MeetingField, rhs: MeetingField) -> Bool { lhs.rawValue < rhs.rawValue }
}

extension MeetingFormProblem {
  var field: MeetingField {
    switch self {
    case .missingTitle: return .title
    case .startInThePast: return .start
    case .invalidDuration: return .length
    case .invalidEmail: return .externals
    case .invalidInterval: return .interval
    case .noWeekday, .startOffWeekdays: return .weekdays
    case .invalidCount: return .count
    case .invalidUntil, .untilBeforeStart: return .until
    }
  }

  var text: LocalizedStringKey {
    switch self {
    case .missingTitle: return "meeting_error_title"
    case .startInThePast: return "meeting_error_past"
    case .invalidDuration: return "meeting_error_duration"
    case .invalidEmail(let email): return "meeting_error_email \(email)"
    case .invalidInterval: return "meeting_error_interval"
    case .noWeekday: return "meeting_error_no_weekday"
    case .startOffWeekdays: return "meeting_error_start_weekday"
    case .invalidCount: return "meeting_error_count"
    case .invalidUntil: return "meeting_error_until"
    case .untilBeforeStart: return "meeting_error_until_before"
    }
  }
}

extension MeetingForm {
  /// The form starting at `date`, with the user's offset from UTC at that start: a series' days of
  /// the week are local days.
  func starting(at date: Date, in zone: TimeZone = .current) -> MeetingForm {
    var form = self
    form.startsAtMs = Int64(date.timeIntervalSince1970 * 1000)
    form.utcOffsetMinutes = Int32(zone.secondsFromGMT(for: date) / 60)
    return form
  }

  var startDate: Date { Date(timeIntervalSince1970: TimeInterval(startsAtMs) / 1000) }
}

/// The last second of the local day of `date`: a series that ends on it still meets that day.
func endOfDayMs(_ date: Date, calendar: Calendar = .current) -> Int64 {
  let start = calendar.startOfDay(for: date)
  let next = calendar.date(byAdding: .day, value: 1, to: start) ?? start.addingTimeInterval(86_400)
  return Int64(next.timeIntervalSince1970 * 1000) - 1000
}

/// Schedules a meeting (`maybeMeeting` nil) or changes the user's own, with what desktop's dialog
/// has: title, description, start, length, invited people and e-mail addresses, co-hosts, the
/// notetaker, the repeat rule and the lobby. The checks are the core's (`meetingFormProblems`).
/// Errors show at their field once a save was refused and the first one is scrolled to and focused
/// (forms.md); a failure the server answered stays above the fields with the form as it is.
struct MeetingFormView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  let account: Account
  let maybeMeeting: Meeting?
  /// A new meeting is listed in this space (its meetings tab).
  var maybeSpaceId: String?
  let onSaved: () async -> Void
  /// A new meeting was scheduled: the caller hands out its link once this sheet is gone.
  var onCreated: (SavedMeeting) -> Void = { _ in }

  @State private var maybeForm: MeetingForm?
  @State private var maybeSavedStartMs: Int64?
  @State private var isSavedSeries = false
  @State private var maybeNotetaker: String?
  /// Names of the people invited, for the co-hosts.
  @State private var names: [String: String] = [:]
  @State private var externalEmail = ""
  @State private var externalName = ""
  @State private var isSaving = false
  @State private var wasRefused = false
  @State private var maybeFailure: AppFailure?
  @FocusState private var focused: MeetingField?

  var body: some View {
    NavigationStack {
      Group {
        if let form = Binding($maybeForm) {
          ScrollViewReader { proxy in
            fields(form, proxy: proxy)
          }
        } else {
          ProgressView("loading")
        }
      }
      .navigationTitle(maybeMeeting == nil ? "meeting_form_title_new" : "meeting_form_title_edit")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }.disabled(isSaving)
        }
      }
    }
    .interactiveDismissDisabled(isSaving)
    .task { await load() }
  }

  @ViewBuilder
  private func fields(_ form: Binding<MeetingForm>, proxy: ScrollViewProxy) -> some View {
    let problems =
      wasRefused ? meetingFormProblems(form: form.wrappedValue, nowMs: nowMs, maybeSavedStartMs: maybeSavedStartMs) : []
    Form {
      if let maybeFailure {
        Section {
          Label { Text(maybeFailure.message) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
            .foregroundStyle(Theme.danger)
        }
      }
      if isSavedSeries {
        Section { Text("meeting_form_series_hint").foregroundStyle(Theme.textMuted) }
      }
      Section {
        TextField("meeting_form_name", text: form.title)
          .focused($focused, equals: .title)
          .accessibilityHint(errorHint(problems, .title))
          .id(MeetingField.title)
        fieldError(problems, .title)
        TextField("meeting_form_description", text: form.description, axis: .vertical)
          .lineLimit(2...6)
      }
      Section {
        DatePicker(
          "meeting_form_start",
          selection: Binding(get: { form.wrappedValue.startDate }, set: { form.wrappedValue = form.wrappedValue.starting(at: $0) }),
          displayedComponents: [.date, .hourAndMinute]
        )
        .accessibilityHint(errorHint(problems, .start))
        .id(MeetingField.start)
        fieldError(problems, .start)
        Picker("meeting_form_duration", selection: form.durationMinutes) {
          ForEach(Array(Set(meetingLengths + [form.wrappedValue.durationMinutes])).sorted(), id: \.self) { minutes in
            Text("meeting_form_minutes \(Int(minutes))").tag(minutes)
          }
        }
        .accessibilityHint(errorHint(problems, .length))
        .id(MeetingField.length)
        fieldError(problems, .length)
      }
      peopleSection(form)
      if !form.wrappedValue.inviteeUserIds.isEmpty {
        coHostsSection(form)
      }
      externalsSection(form, problems: problems)
      repeatSection(form, problems: problems)
      Section {
        Toggle("meeting_form_lobby", isOn: form.hasLobby)
      }
      Section {
        Button {
          save(proxy: proxy)
        } label: {
          HStack {
            Spacer()
            if isSaving {
              ProgressView()
            } else {
              Text(maybeMeeting == nil ? "meeting_form_submit" : "meeting_form_save").bold()
            }
            Spacer()
          }
        }
        .disabled(isSaving)
      }
    }
    .disabled(isSaving)
  }

  @ViewBuilder
  private func peopleSection(_ form: Binding<MeetingForm>) -> some View {
    Section {
      if let bot = maybeNotetaker {
        Toggle(
          isOn: Binding(
            get: { form.wrappedValue.maybeNotetakerUserId != nil },
            set: { form.wrappedValue.maybeNotetakerUserId = $0 ? bot : nil })
        ) {
          VStack(alignment: .leading) {
            Text("meeting_form_notetaker")
            Text("meeting_form_notetaker_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
          }
        }
      }
      NavigationLink {
        PeoplePicker(
          picked: Binding(
            get: { form.wrappedValue.inviteeUserIds },
            set: { invitees in
              form.wrappedValue.inviteeUserIds = invitees
              form.wrappedValue.coHostUserIds.removeAll { !invitees.contains($0) }
            }),
          excluded: Set([account.userId] + (maybeNotetaker.map { [$0] } ?? [])),
          isEnabled: !isSaving
        )
        .navigationTitle("meeting_form_invite")
        .navigationBarTitleDisplayMode(.inline)
      } label: {
        LabeledContent("meeting_form_invite") { Text("\(form.wrappedValue.inviteeUserIds.count)") }
      }
    }
  }

  /// Who hosts with the organizer, among the invited members (#1720); this device signs them.
  private func coHostsSection(_ form: Binding<MeetingForm>) -> some View {
    Section {
      ForEach(form.wrappedValue.inviteeUserIds, id: \.self) { id in
        Toggle(
          isOn: Binding(
            get: { form.wrappedValue.coHostUserIds.contains(id) },
            set: { isOn in
              form.wrappedValue.coHostUserIds.removeAll { $0 == id }
              if isOn { form.wrappedValue.coHostUserIds.append(id) }
            })
        ) {
          if let name = names[id] {
            Text(verbatim: name)
          } else {
            Text("meeting_form_unknown_person")
          }
        }
      }
    } header: {
      Text("meeting_form_co_hosts")
    } footer: {
      Text("meeting_form_co_hosts_hint")
    }
  }

  /// People without an account, invited by e-mail address; each address that can't be one is named.
  private func externalsSection(_ form: Binding<MeetingForm>, problems: [MeetingFormProblem]) -> some View {
    let badEmails: [String] = problems.compactMap {
      if case .invalidEmail(let email) = $0 { return email }
      return nil
    }
    return Section {
      ForEach(Array(form.wrappedValue.externalInvitees.enumerated()), id: \.offset) { index, external in
        HStack {
          Text(verbatim: external.name.isEmpty ? external.email : "\(external.name) <\(external.email)>")
            .foregroundStyle(badEmails.contains(external.email.trimmingCharacters(in: .whitespaces)) ? Theme.danger : Theme.text)
          Spacer()
          Button {
            form.wrappedValue.externalInvitees.remove(at: index)
          } label: {
            Image(systemName: "xmark.circle.fill").foregroundStyle(Theme.textMuted)
          }
          .buttonStyle(.borderless)
          .accessibilityLabel(Text("meeting_form_external_remove \(external.email)"))
        }
      }
      TextField("meeting_form_external_email", text: $externalEmail)
        .keyboardType(.emailAddress)
        .textContentType(.emailAddress)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .focused($focused, equals: .externals)
        .accessibilityHint(errorHint(problems, .externals))
        .id(MeetingField.externals)
      HStack {
        TextField("meeting_form_external_name", text: $externalName)
        Button("meeting_form_add") {
          let email = externalEmail.trimmingCharacters(in: .whitespacesAndNewlines)
          guard !email.isEmpty else { return }
          form.wrappedValue.externalInvitees.append(
            MeetingExternalInvitee(email: email, name: externalName.trimmingCharacters(in: .whitespacesAndNewlines)))
          externalEmail = ""
          externalName = ""
        }
        .buttonStyle(.borderless)
        .disabled(externalEmail.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
      }
      fieldError(problems, .externals)
    } header: {
      Text("meeting_form_external")
    }
  }

  /// How the meeting repeats: every N days, weeks or months, on which days, and until when.
  @ViewBuilder
  private func repeatSection(_ form: Binding<MeetingForm>, problems: [MeetingFormProblem]) -> some View {
    let repeatForm = form.wrappedValue.repeatRule
    Section {
      Picker(
        "meeting_form_repeat",
        selection: Binding(
          get: { repeatForm.frequency },
          set: { choice in
            if let changed = try? meetingFormWithRepeat(form: form.wrappedValue, frequency: choice) {
              form.wrappedValue = changed
            }
          })
      ) {
        Text("meeting_repeat_never").tag(MeetingRepeat.never)
          // A series stays a series when edited (its end is cancelling the following meetings).
          .selectionDisabled(isSavedSeries)
        Text("meeting_repeat_daily").tag(MeetingRepeat.daily)
        Text("meeting_repeat_weekly").tag(MeetingRepeat.weekly)
        Text("meeting_repeat_monthly").tag(MeetingRepeat.monthly)
      }
      if repeatForm.frequency != .never {
        Stepper(value: form.repeatRule.interval, in: 1...100) {
          Text(everyText(repeatForm))
        }
        .accessibilityHint(errorHint(problems, .interval))
        .id(MeetingField.interval)
        fieldError(problems, .interval)
        if repeatForm.frequency == .daily || repeatForm.frequency == .weekly {
          WeekdayToggles(weekdays: form.repeatRule.weekdays)
            .accessibilityHint(errorHint(problems, .weekdays))
            .id(MeetingField.weekdays)
          fieldError(problems, .weekdays)
        }
        Toggle(isOn: form.repeatRule.skipsHolidays) {
          VStack(alignment: .leading) {
            Text("meeting_form_skip_holidays")
            Text("meeting_form_skip_holidays_hint").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
          }
        }
        Picker("meeting_form_ends", selection: form.repeatRule.ends) {
          Text("meeting_form_ends_never").tag(MeetingSeriesEnd.never)
          Text("meeting_form_ends_after").tag(MeetingSeriesEnd.afterCount)
          Text("meeting_form_ends_on").tag(MeetingSeriesEnd.onDate)
        }
        switch repeatForm.ends {
        case .never:
          EmptyView()
        case .afterCount:
          Stepper(value: form.repeatRule.count, in: 1...1000) {
            Text("meeting_ends_after_count \(Int(repeatForm.count))")
          }
          .accessibilityHint(errorHint(problems, .count))
          .id(MeetingField.count)
          fieldError(problems, .count)
        case .onDate:
          DatePicker(
            "meeting_form_ends_on",
            selection: Binding(
              get: {
                repeatForm.maybeUntilMs.map { Date(timeIntervalSince1970: TimeInterval($0) / 1000) } ?? form.wrappedValue.startDate
              },
              set: { form.wrappedValue.repeatRule.maybeUntilMs = endOfDayMs($0) }),
            displayedComponents: [.date]
          )
          .accessibilityHint(errorHint(problems, .until))
          .id(MeetingField.until)
          .onAppear {
            // The picker shows a date: the series ends on it unless another is picked.
            if form.wrappedValue.repeatRule.maybeUntilMs == nil {
              form.wrappedValue.repeatRule.maybeUntilMs = endOfDayMs(form.wrappedValue.startDate)
            }
          }
          fieldError(problems, .until)
        }
      }
    }
  }

  private func everyText(_ repeatForm: MeetingRepeatForm) -> LocalizedStringKey {
    let count = Int(repeatForm.interval)
    switch repeatForm.frequency {
    case .daily: return "meeting_every_days \(count)"
    case .weekly: return "meeting_every_weeks \(count)"
    default: return "meeting_every_months \(count)"
    }
  }

  @ViewBuilder
  private func fieldError(_ problems: [MeetingFormProblem], _ field: MeetingField) -> some View {
    if let problem = problems.first(where: { $0.field == field }) {
      Text(problem.text).font(.app(.footnote)).foregroundStyle(Theme.danger)
    }
  }

  /// VoiceOver reads a field's error with the field.
  private func errorHint(_ problems: [MeetingFormProblem], _ field: MeetingField) -> Text {
    problems.first(where: { $0.field == field }).map { Text($0.text) } ?? Text(verbatim: "")
  }

  private var nowMs: Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

  private func load() async {
    guard maybeForm == nil else { return }
    maybeNotetaker = await model.meetingNotetaker()
    if let people = await model.searchPeople("") {
      names = Dictionary(people.map { ($0.id, $0.displayName) }, uniquingKeysWith: { first, _ in first })
    }
    guard let meeting = maybeMeeting else {
      let form = newMeetingForm(nowMs: nowMs, utcOffsetMinutes: Int32(TimeZone.current.secondsFromGMT() / 60))
      maybeForm = form.starting(at: form.startDate)
      return
    }
    do {
      let edit = try await model.meetingEditForm(meetingId: meeting.id)
      maybeSavedStartMs = edit.form.startsAtMs
      isSavedSeries = edit.isRepeating
      // The core doesn't know the time zone: the offset is set here, for the saved start.
      maybeForm = edit.form.starting(at: edit.form.startDate)
    } catch {
      maybeFailure = AppFailure(CoreFailure(error))
    }
  }

  private func save(proxy: ScrollViewProxy) {
    guard let form = maybeForm, !isSaving else { return }
    let problems = meetingFormProblems(form: form, nowMs: nowMs, maybeSavedStartMs: maybeSavedStartMs)
    if let first = problems.map(\.field).min() {
      wasRefused = true
      scrollToMiddle(first, in: proxy, reduceMotion: reduceMotion)
      if first == .title || first == .externals { focused = first }
      return
    }
    isSaving = true
    maybeFailure = nil
    Task {
      defer { isSaving = false }
      do {
        let outcome: MeetingSave
        if let meeting = maybeMeeting {
          outcome = try await model.saveMeetingEdit(meetingId: meeting.id, form: form)
        } else {
          outcome = try await model.scheduleMeeting(form, spaceId: maybeSpaceId)
        }
        switch outcome {
        case .saved(let saved):
          await onSaved()
          dismiss()
          if maybeMeeting == nil { onCreated(saved) }
        case .refused:
          wasRefused = true
        }
      } catch {
        // The form stays as it is, with its request key: a retry schedules the meeting once.
        maybeFailure = AppFailure(CoreFailure(error))
      }
    }
  }
}

/// The days of the week a daily or weekly series meets on, Monday first, as toggle buttons.
private struct WeekdayToggles: View {
  @Binding var weekdays: [Bool]

  var body: some View {
    VStack(alignment: .leading) {
      Text("meeting_form_on_days").font(.app(.footnote)).foregroundStyle(Theme.textMuted)
      HStack(spacing: 6) {
        ForEach(0..<7, id: \.self) { index in
          let isOn = weekdays.indices.contains(index) && weekdays[index]
          Button {
            if weekdays.indices.contains(index) { weekdays[index].toggle() }
          } label: {
            Text(verbatim: Self.symbols[index])
              .frame(maxWidth: .infinity, minHeight: 32)
              .background(isOn ? Theme.accent : Color.clear, in: Capsule())
              .overlay(Capsule().stroke(isOn ? Color.clear : Theme.controlBorder))
              .foregroundStyle(isOn ? Theme.onAccent : Theme.text)
          }
          .buttonStyle(.borderless)
          .accessibilityLabel(Text(verbatim: Self.names[index]))
          .accessibilityAddTraits(isOn ? .isSelected : [])
        }
      }
    }
  }

  /// The calendar's weekday symbols start on Sunday; the form's on Monday.
  private static let symbols: [String] = mondayFirst(Calendar.current.veryShortStandaloneWeekdaySymbols)
  private static let names: [String] = mondayFirst(Calendar.current.standaloneWeekdaySymbols)

  private static func mondayFirst(_ symbols: [String]) -> [String] {
    guard symbols.count == 7 else { return symbols }
    return Array(symbols[1...]) + [symbols[0]]
  }
}
