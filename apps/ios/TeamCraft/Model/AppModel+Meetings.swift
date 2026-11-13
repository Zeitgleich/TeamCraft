import Foundation
import OSLog

private let meetingsLog = Logger(subsystem: "de.zeitgleich.teamcraft", category: "meetings")

/// Scheduling, changing, cancelling and starting meetings (#1841), as desktop and web do it: the
/// core saves the meeting and sets up its chat's encryption with the people invited here (e2ee.md
/// §4.6, "Meeting chats"); these are the calls the screens make.
extension AppModel {
  /// Schedules `form`; `.refused` sent nothing.
  func scheduleMeeting(_ form: MeetingForm) async throws -> MeetingSave {
    try await core().scheduleMeeting(form: form, maybeSpaceId: nil)
  }

  /// "Meet now"; `requestKey` is made once per tap, so a retry after a lost answer starts no second
  /// meeting.
  func startMeetingNow(requestKey: String) async throws -> SavedMeeting {
    try await core().startMeetingNow(maybeTitle: nil, maybeSpaceId: nil, requestKey: requestKey)
  }

  /// The edit form of a meeting the user organizes, as the server has it now.
  func meetingEditForm(meetingId: String) async throws -> MeetingEditForm {
    try await core().meetingEditForm(meetingId: meetingId)
  }

  func saveMeetingEdit(meetingId: String, form: MeetingForm) async throws -> MeetingSave {
    try await core().saveMeetingEdit(meetingId: meetingId, form: form)
  }

  func cancelMeeting(_ meeting: Meeting, scope: MeetingCancelScope) async throws {
    try await core().cancelMeeting(meetingId: meeting.id, scope: scope, occurrenceStartsAtMs: meeting.startsAt)
  }

  /// Joins a meeting's call. On its organizer's device the core first sets up a meeting chat that
  /// has no encryption group yet (meetings from before 1.0.3); the call starts either way.
  func joinMeeting(_ meeting: Meeting, userId: String) {
    guard meeting.organizerUserId == userId else {
      calls.startCall(conversationId: meeting.conversationId, displayName: meeting.title)
      return
    }
    Task {
      do {
        try await core().prepareMeetingJoin(meetingId: meeting.id)
      } catch {
        meetingsLog.warning("the meeting chat could not be prepared before joining")
      }
      calls.startCall(conversationId: meeting.conversationId, displayName: meeting.title)
    }
  }
}
