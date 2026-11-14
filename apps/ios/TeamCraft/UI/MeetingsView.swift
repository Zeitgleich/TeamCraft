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
