import SwiftUI

/// Upcoming meetings: join (the meeting's chat call), answer the invitation, start one now,
/// schedule one, and change or cancel the user's own (#1841). The chats screen's Meetings tab
/// (#1786): it shows in place, under the screen's header and above its bottom bar, in that
/// screen's navigation; it has none of its own. The core saves the meetings and sets up their
/// encrypted chats; this is the list and the form.
struct MeetingsView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var meetings: [Meeting]?
  @State private var isRsvpFailed = false
  @State private var isFormOpen = false
  @State private var isStarting = false
  /// One key per "Meet now" until it went through: a retry after a lost answer starts no second one.
  @State private var maybeNowKey: String?
  @State private var maybeFailure: AppFailure?

  var body: some View {
    VStack(spacing: 0) {
      actions
      Group {
        if let meetings = meetings?.filter({ $0.status == "scheduled" }) {
          if meetings.isEmpty {
            ContentUnavailableView {
              Label("meetings_title", systemImage: "calendar")
            } description: {
              Text("meetings_empty")
            }
          } else {
            // A repeating meeting is listed once per occurrence, all with the series' ID.
            List(meetings, id: \.occurrenceKey) { meeting in
              // Joining starts the meeting's call as before; the tab stays where it is.
              MeetingRow(
                account: account, meeting: meeting, onJoin: {},
                onChanged: { self.meetings = await model.meetings() },
                onAnswer: { answer in await respond(to: meeting, with: answer) })
            }
            .listStyle(.plain)
          }
        } else {
          ProgressView("loading")
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    .task(id: model.changeCount) { meetings = await model.meetings() }
    .alert("meetings_rsvp_failed", isPresented: $isRsvpFailed) {
      Button("done", role: .cancel) {}
    }
    .alert(
      maybeFailure?.message ?? "error_generic",
      isPresented: Binding(get: { maybeFailure != nil }, set: { if !$0 { maybeFailure = nil } })
    ) {
      Button("done", role: .cancel) {}
    }
    .sheet(isPresented: $isFormOpen) {
      MeetingFormView(account: account, maybeMeeting: nil) {
        meetings = await model.meetings()
      }
    }
  }

  /// "Meet now" and "Schedule", above the list.
  private var actions: some View {
    HStack {
      Button(action: meetNow) {
        if isStarting {
          ProgressView()
        } else {
          Label("meetings_meet_now", systemImage: "video")
        }
      }
      .buttonStyle(.bordered)
      .disabled(isStarting)
      Spacer()
      Button {
        isFormOpen = true
      } label: {
        Label("meetings_schedule", systemImage: "calendar.badge.plus")
      }
      .buttonStyle(.borderedProminent)
    }
    .padding(.horizontal)
    .padding(.vertical, 8)
  }

  private func meetNow() {
    guard !isStarting else { return }
    isStarting = true
    let key = maybeNowKey ?? newRequestKey()
    maybeNowKey = key
    Task {
      defer { isStarting = false }
      do {
        let meeting = try await model.startMeetingNow(requestKey: key)
        maybeNowKey = nil
        model.calls.startCall(conversationId: meeting.conversationId, displayName: meeting.title)
        meetings = await model.meetings()
      } catch {
        maybeFailure = AppFailure(CoreFailure(error))
      }
    }
  }

  private func respond(to meeting: Meeting, with answer: RsvpAnswer) async {
    if await model.setRsvp(meetingId: meeting.id, answer: answer) {
      meetings = await model.meetings()
    } else {
      isRsvpFailed = true
    }
  }
}

/// A meeting's title and time, with the buttons to join its call and to answer the invitation;
/// its organizer also gets "Edit meeting" and "Cancel meeting". `onChanged` runs once one of them
/// went through.
struct MeetingRow: View {
  @Environment(AppModel.self) private var model
  let account: Account
  let meeting: Meeting
  let onJoin: () -> Void
  let onChanged: () async -> Void
  let onAnswer: (RsvpAnswer) async -> Void

  @State private var isEditing = false
  @State private var isAskingToCancel = false
  @State private var isCancelling = false
  @State private var maybeFailure: AppFailure?

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      Text(meeting.title).font(.app(.headline))
      Text(timeText).font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
      HStack {
        Button("meeting_join") {
          onJoin()
          model.joinMeeting(meeting, userId: account.userId)
        }
        .buttonStyle(.borderedProminent)
        if let answer = meeting.rsvp(of: account.userId) {
          Menu {
            ForEach([RsvpAnswer.accepted, .tentative, .declined]) { choice in
              Button { Task { await onAnswer(choice) } } label: {
                if choice == answer { Label(choice.title, systemImage: "checkmark") } else { Text(choice.title) }
              }
            }
          } label: {
            Label(answer.title, systemImage: answer.symbol)
          }
          .accessibilityLabel(Text("meeting_rsvp"))
        }
        if isOrganizer {
          Spacer()
          Menu {
            Button("meeting_edit", systemImage: "pencil") { isEditing = true }
            Button("meeting_cancel", systemImage: "xmark.circle", role: .destructive) { isAskingToCancel = true }
          } label: {
            Image(systemName: "ellipsis.circle")
          }
          .accessibilityLabel(Text("meeting_actions"))
          .disabled(isCancelling)
        }
      }
      .buttonStyle(.borderless)
    }
    .padding(.vertical, 4)
    .sheet(isPresented: $isEditing) {
      MeetingFormView(account: account, maybeMeeting: meeting, onSaved: onChanged)
    }
    .confirmationDialog(
      meeting.isRepeating ? "meeting_cancel_title_series" : "meeting_cancel_title",
      isPresented: $isAskingToCancel, titleVisibility: .visible
    ) {
      if meeting.isRepeating {
        Button("meeting_cancel_occurrence", role: .destructive) { cancel(.occurrence) }
        Button("meeting_cancel_following", role: .destructive) { cancel(.following) }
        Button("meeting_cancel_series", role: .destructive) { cancel(.series) }
      } else {
        Button("meeting_cancel_confirm", role: .destructive) { cancel(.series) }
      }
      Button("meeting_cancel_keep", role: .cancel) {}
    } message: {
      Text("meeting_cancel_hint")
    }
    .alert(
      maybeFailure?.message ?? "error_generic",
      isPresented: Binding(get: { maybeFailure != nil }, set: { if !$0 { maybeFailure = nil } })
    ) {
      Button("done", role: .cancel) {}
    }
  }

  private var isOrganizer: Bool { meeting.organizerUserId == account.userId }

  private func cancel(_ scope: MeetingCancelScope) {
    isCancelling = true
    Task {
      defer { isCancelling = false }
      do {
        try await model.cancelMeeting(meeting, scope: scope)
        await onChanged()
      } catch {
        maybeFailure = AppFailure(CoreFailure(error))
      }
    }
  }

  private var timeText: String {
    let start = meeting.startDate.formatted(date: .abbreviated, time: .shortened)
    let end = meeting.endDate.formatted(
      Calendar.current.isDate(meeting.startDate, inSameDayAs: meeting.endDate)
        ? Date.FormatStyle(date: .omitted, time: .shortened) : Date.FormatStyle(date: .abbreviated, time: .shortened))
    return "\(start) – \(end)"
  }
}

/// The lengths the form offers, as on desktop; a saved meeting's own length is offered too.
let meetingLengths: [UInt32] = [15, 30, 45, 60, 90, 120]

/// Schedules a meeting (`maybeMeeting` nil) or changes the user's own: title, description, start,
/// length, the lobby and the invited people. Errors show at their field once a save was refused
/// (forms.md); a failure the server answered stays above the fields with the form as it is.
struct MeetingFormView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let account: Account
  let maybeMeeting: Meeting?
  let onSaved: () async -> Void

  @State private var maybeForm: MeetingForm?
  @State private var maybeSavedStartMs: Int64?
  @State private var isRepeating = false
  @State private var isSaving = false
  @State private var wasRefused = false
  @State private var maybeFailure: AppFailure?
  @FocusState private var isTitleFocused: Bool

  var body: some View {
    NavigationStack {
      Group {
        if let form = Binding($maybeForm) {
          fields(form)
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
        ToolbarItem(placement: .confirmationAction) {
          if isSaving {
            ProgressView()
          } else {
            Button(maybeMeeting == nil ? "meeting_form_submit" : "meeting_form_save", action: save)
              .disabled(maybeForm == nil)
          }
        }
      }
    }
    .interactiveDismissDisabled(isSaving)
    .task { await load() }
  }

  @ViewBuilder
  private func fields(_ form: Binding<MeetingForm>) -> some View {
    let problems =
      wasRefused ? meetingFormProblems(form: form.wrappedValue, nowMs: nowMs, maybeSavedStartMs: maybeSavedStartMs) : []
    Form {
      if let maybeFailure {
        Section {
          Label { Text(maybeFailure.message) } icon: { Image(systemName: "exclamationmark.triangle.fill") }
            .foregroundStyle(Theme.danger)
        }
      }
      if isRepeating {
        Section { Text("meeting_form_series_hint").foregroundStyle(Theme.textMuted) }
      }
      Section {
        TextField("meeting_form_name", text: form.title)
          .focused($isTitleFocused)
        fieldError(problems.contains(.missingTitle) ? "meeting_error_title" : nil)
        TextField("meeting_form_description", text: form.description, axis: .vertical)
          .lineLimit(2...6)
      }
      Section {
        DatePicker("meeting_form_start", selection: startDate(form), displayedComponents: [.date, .hourAndMinute])
        fieldError(problems.contains(.startInThePast) ? "meeting_error_past" : nil)
        Picker("meeting_form_duration", selection: form.durationMinutes) {
          ForEach(Array(Set(meetingLengths + [form.wrappedValue.durationMinutes])).sorted(), id: \.self) { minutes in
            Text("meeting_form_minutes \(Int(minutes))").tag(minutes)
          }
        }
        fieldError(problems.contains(.invalidDuration) ? "meeting_error_duration" : nil)
        Toggle("meeting_form_lobby", isOn: form.hasLobby)
      }
      Section {
        NavigationLink {
          PeoplePicker(picked: form.inviteeUserIds, excluded: [account.userId], isEnabled: !isSaving)
            .navigationTitle("meeting_form_invite")
            .navigationBarTitleDisplayMode(.inline)
        } label: {
          LabeledContent("meeting_form_invite") { Text("\(form.wrappedValue.inviteeUserIds.count)") }
        }
      }
    }
    .disabled(isSaving)
  }

  @ViewBuilder
  private func fieldError(_ maybeText: LocalizedStringKey?) -> some View {
    if let maybeText {
      Text(maybeText).font(.app(.footnote)).foregroundStyle(Theme.danger)
    }
  }

  private var nowMs: Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

  private func startDate(_ form: Binding<MeetingForm>) -> Binding<Date> {
    Binding(
      get: { Date(timeIntervalSince1970: TimeInterval(form.wrappedValue.startsAtMs) / 1000) },
      set: { date in form.wrappedValue.startsAtMs = Int64(date.timeIntervalSince1970 * 1000) })
  }

  private func load() async {
    guard maybeForm == nil else { return }
    guard let meeting = maybeMeeting else {
      maybeForm = newMeetingForm(nowMs: nowMs)
      return
    }
    do {
      let edit = try await model.meetingEditForm(meetingId: meeting.id)
      maybeSavedStartMs = edit.form.startsAtMs
      isRepeating = edit.isRepeating
      maybeForm = edit.form
    } catch {
      maybeFailure = AppFailure(CoreFailure(error))
    }
  }

  private func save() {
    guard let form = maybeForm, !isSaving else { return }
    let problems = meetingFormProblems(form: form, nowMs: nowMs, maybeSavedStartMs: maybeSavedStartMs)
    guard problems.isEmpty else {
      wasRefused = true
      if problems.first == .missingTitle { isTitleFocused = true }
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
          outcome = try await model.scheduleMeeting(form)
        }
        switch outcome {
        case .saved:
          await onSaved()
          dismiss()
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

extension RsvpAnswer {
  var title: LocalizedStringKey {
    switch self {
    case .needsAction: return "rsvp_needs_action"
    case .accepted: return "rsvp_accepted"
    case .declined: return "rsvp_declined"
    case .tentative: return "rsvp_tentative"
    }
  }

  var symbol: String {
    switch self {
    case .needsAction: return "questionmark.circle"
    case .accepted: return "checkmark.circle"
    case .declined: return "xmark.circle"
    case .tentative: return "circle.dashed"
    }
  }
}
