import SwiftUI

/// Upcoming meetings: join (the meeting's chat call) and answer the invitation.
struct MeetingsView: View {
  @Environment(AppModel.self) private var model
  @Environment(\.dismiss) private var dismiss
  let account: Account

  @State private var meetings: [Meeting]?
  @State private var isRsvpFailed = false

  var body: some View {
    NavigationStack {
      Group {
        if let meetings {
          if meetings.isEmpty {
            Text("meetings_empty").foregroundStyle(Theme.textMuted)
          } else {
            List(meetings) { meeting in
              row(meeting)
            }
          }
        } else {
          ProgressView("loading")
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity)
      .navigationTitle("meetings_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
      .task(id: model.changeCount) { meetings = await model.meetings() }
      .alert("meetings_rsvp_failed", isPresented: $isRsvpFailed) {
        Button("done", role: .cancel) {}
      }
    }
  }

  private func row(_ meeting: Meeting) -> some View {
    VStack(alignment: .leading, spacing: 8) {
      Text(meeting.title).font(.headline)
      Text(timeText(meeting)).font(.subheadline).foregroundStyle(Theme.textMuted)
      HStack {
        Button("meeting_join") {
          dismiss()
          model.calls.startCall(conversationId: meeting.conversationId, displayName: meeting.title)
        }
        .buttonStyle(.borderedProminent)
        if let answer = meeting.rsvp(of: account.userId) {
          Menu {
            ForEach([RsvpAnswer.accepted, .tentative, .declined]) { choice in
              Button { Task { await respond(to: meeting, with: choice) } } label: {
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

  private func timeText(_ meeting: Meeting) -> String {
    let start = meeting.startDate.formatted(date: .abbreviated, time: .shortened)
    let end = meeting.endDate.formatted(
      Calendar.current.isDate(meeting.startDate, inSameDayAs: meeting.endDate)
        ? Date.FormatStyle(date: .omitted, time: .shortened) : Date.FormatStyle(date: .abbreviated, time: .shortened))
    return "\(start) – \(end)"
  }

  private func respond(to meeting: Meeting, with answer: RsvpAnswer) async {
    if await model.setRsvp(meetingId: meeting.id, answer: answer) {
      meetings = await model.meetings()
    } else {
      isRsvpFailed = true
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
