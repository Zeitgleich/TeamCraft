import SwiftUI

/// Upcoming meetings: join (the meeting's chat call) and answer the invitation. The chats screen's
/// Meetings tab (#1786): it shows in place, under the screen's header and above its bottom bar, in
/// that screen's navigation; it has none of its own.
struct MeetingsView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var meetings: [Meeting]?
  @State private var isRsvpFailed = false

  var body: some View {
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
            MeetingRow(account: account, meeting: meeting, onJoin: {}) { answer in
              await respond(to: meeting, with: answer)
            }
          }
          .listStyle(.plain)
        }
      } else {
        ProgressView("loading")
      }
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .task(id: model.changeCount) { meetings = await model.meetings() }
    .alert("meetings_rsvp_failed", isPresented: $isRsvpFailed) {
      Button("done", role: .cancel) {}
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

/// A meeting's title and time, with the buttons to join its call and to answer the invitation.
struct MeetingRow: View {
  @Environment(AppModel.self) private var model
  let account: Account
  let meeting: Meeting
  let onJoin: () -> Void
  let onAnswer: (RsvpAnswer) async -> Void

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      Text(meeting.title).font(.app(.headline))
      Text(timeText).font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
      HStack {
        Button("meeting_join") {
          onJoin()
          model.calls.startCall(conversationId: meeting.conversationId, displayName: meeting.title)
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
      }
    }
    .padding(.vertical, 4)
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
