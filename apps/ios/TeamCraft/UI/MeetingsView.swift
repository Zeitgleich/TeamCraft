import SwiftUI
import UIKit

/// The meetings screen's views, as desktop's (#956): what is still on, and what ended lately.
enum MeetingsTab: Hashable {
  case upcoming, past
}

/// Upcoming and past meetings: join (the meeting's chat call), answer the invitation, start one
/// now, schedule one (then its link is handed out), and change, cancel, restore or remove the
/// user's own (#1841, #1905). The chats screen's Meetings tab (#1786): it shows in place, under the
/// screen's header and above its bottom bar, in that screen's navigation; it has none of its own.
/// The core saves the meetings and sets up their encrypted chats; this is the list.
struct MeetingsView: View {
  @Environment(AppModel.self) private var model
  let account: Account

  @State private var tab = MeetingsTab.upcoming
  @State private var meetings: [Meeting]?
  @State private var maybePast: [Meeting]?
  @State private var isRsvpFailed = false
  @State private var isFormOpen = false
  @State private var maybeCreated: SavedMeeting?
  @State private var isStarting = false
  /// One key per "Meet now" until it went through: a retry after a lost answer starts no second one.
  @State private var maybeNowKey: String?
  @State private var maybeFailure: AppFailure?

  var body: some View {
    VStack(spacing: 0) {
      actions
      Picker("meetings_title", selection: $tab) {
        Text("meetings_tab_upcoming").tag(MeetingsTab.upcoming)
        Text("meetings_tab_past").tag(MeetingsTab.past)
      }
      .pickerStyle(.segmented)
      .padding(.horizontal)
      .padding(.bottom, 8)
      Group {
        switch tab {
        case .upcoming: list(meetings, empty: "meetings_empty", refresh: { meetings = await model.meetings() })
        case .past: list(maybePast, empty: "meetings_past_empty", refresh: { maybePast = await model.pastMeetings() })
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    .task(id: model.changeCount) { meetings = await model.meetings() }
    // The past list is only read once it is looked at.
    .task(id: tab) { if tab == .past { maybePast = await model.pastMeetings() } }
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
      MeetingFormView(
        account: account, maybeMeeting: nil, onSaved: { meetings = await model.meetings() },
        onCreated: { maybeCreated = $0 })
    }
    .sheet(item: $maybeCreated) { saved in
      MeetingCreatedView(account: account, saved: saved)
    }
  }

  @ViewBuilder
  private func list(_ maybeMeetings: [Meeting]?, empty: LocalizedStringKey, refresh: @escaping () async -> Void) -> some View {
    if let meetings = maybeMeetings {
      if meetings.isEmpty {
        ContentUnavailableView {
          Label("meetings_title", systemImage: "calendar")
        } description: {
          Text(empty)
        }
      } else {
        // A repeating meeting is listed once per occurrence, all with the series' ID.
        List(meetings, id: \.occurrenceKey) { meeting in
          // Joining starts the meeting's call as before; the tab stays where it is.
          MeetingRow(
            account: account, meeting: meeting, onJoin: {},
            onChanged: refresh,
            onAnswer: { answer in await respond(to: meeting, with: answer) })
        }
        .listStyle(.plain)
        .refreshable { await refresh() }
      }
    } else {
      ProgressView("loading")
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

/// A meeting's title and time, with the buttons to join its call (while it is on) and to answer
/// the invitation, and its menu: "Copy link", and for its organizer "Edit meeting", "Cancel
/// meeting", "Restore" and "Remove from list", as the core's rules for the row say
/// (`meetingRowActions`). `onChanged` runs once one of them went through.
struct MeetingRow: View {
  @Environment(AppModel.self) private var model
  let account: Account
  let meeting: Meeting
  let onJoin: () -> Void
  let onChanged: () async -> Void
  let onAnswer: (RsvpAnswer) async -> Void

  @State private var isEditing = false
  @State private var isAskingToCancel = false
  @State private var isBusy = false
  @State private var maybeFailure: AppFailure?

  var body: some View {
    let actions = rowActions
    VStack(alignment: .leading, spacing: 8) {
      Text(meeting.title).font(.app(.headline))
        .foregroundStyle(actions.canCopyLink ? Theme.text : Theme.textMuted)
      Text(timeText).font(.app(.subheadline)).foregroundStyle(Theme.textMuted)
      if isCancelled {
        Text(meeting.isOccurrenceHoliday == true ? "meeting_holiday" : "meeting_cancelled")
          .font(.app(.footnote)).foregroundStyle(Theme.danger)
      }
      HStack {
        if actions.canCopyLink {
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
        }
        if actions.canCopyLink || actions.canChange || actions.canRestore || actions.canHide {
          Spacer()
          Menu {
            if actions.canCopyLink {
              Button("meeting_copy_link", systemImage: "link") { run { try await copyLink() } }
            }
            if actions.canChange {
              Button("meeting_edit", systemImage: "pencil") { isEditing = true }
              Button("meeting_cancel", systemImage: "xmark.circle", role: .destructive) { isAskingToCancel = true }
            }
            if actions.canRestore {
              Button(
                meeting.isOccurrenceHoliday == true ? "meeting_restore_holiday" : "meeting_restore",
                systemImage: "arrow.uturn.backward"
              ) { run { try await model.restoreMeeting(meeting) } }
            }
            if actions.canHide {
              Button("meeting_remove", systemImage: "trash", role: .destructive) {
                run { try await model.hideMeeting(meeting) }
              }
            }
          } label: {
            Image(systemName: "ellipsis.circle")
          }
          .accessibilityLabel(Text("meeting_actions"))
          .disabled(isBusy)
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
  private var isCancelled: Bool { meeting.status == "cancelled" }

  private var rowActions: MeetingRowActions {
    meetingRowActions(
      isOrganizer: isOrganizer, isCancelled: isCancelled,
      isOccurrenceCancelled: meeting.isOccurrenceCancelled == true, isEnded: meeting.status == "ended",
      endsAtMs: meeting.endsAt, nowMs: Int64(Date().timeIntervalSince1970 * 1000))
  }

  /// "Copy link": the core makes it with the organizer's key fingerprint as this device pinned it
  /// (e2ee.md §4.6), never the server's; without that, nothing is copied.
  private func copyLink() async throws {
    UIPasteboard.general.string = try await model.meetingLink(meetingId: meeting.id)
  }

  private func cancel(_ scope: MeetingCancelScope) {
    run { try await model.cancelMeeting(meeting, scope: scope) }
  }

  /// Runs one of the row's actions; on success the list is read again.
  private func run(_ action: @escaping () async throws -> Void) {
    guard !isBusy else { return }
    isBusy = true
    Task {
      defer { isBusy = false }
      do {
        try await action()
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
