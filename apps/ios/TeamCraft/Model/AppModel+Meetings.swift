import Foundation
import OSLog

private let meetingsLog = Logger(subsystem: "de.zeitgleich.teamcraft", category: "meetings")

/// Scheduling, changing, cancelling and starting meetings (#1841), as desktop and web do it: the
/// core saves the meeting and sets up its chat's encryption with the people invited here (e2ee.md
/// §4.6, "Meeting chats"); these are the calls the screens make.
extension AppModel {
  /// Schedules `form`, listed in the space `maybeSpaceId`; `.refused` sent nothing.
  func scheduleMeeting(_ form: MeetingForm, spaceId maybeSpaceId: String? = nil) async throws -> MeetingSave {
    try await core().scheduleMeeting(form: form, maybeSpaceId: maybeSpaceId)
  }

  /// The Past view: meetings of the last 30 days that ended, latest first (#1905); nil if unread.
  func pastMeetings() async -> [Meeting]? {
    guard let json = try? await core().pastMeetings() else { return nil }
    return Wire.decode(MeetingList.self, from: json)?.meetings
  }

  /// The meeting's link with the organizer's key fingerprint as this device pinned it (e2ee.md
  /// §4.6), never the server's; throws when this device can't verify it.
  func meetingLink(meetingId: String) async throws -> String {
    try await core().meetingLink(meetingId: meetingId)
  }

  /// Takes a cancellation back: the meeting's, or of a series this occurrence's.
  func restoreMeeting(_ meeting: Meeting) async throws {
    let maybeOccurrence = meeting.isOccurrenceCancelled == true ? meeting.startsAt : nil
    try await core().restoreMeeting(meetingId: meeting.id, maybeOccurrenceStartsAtMs: maybeOccurrence)
  }

  /// "Remove from list": a cancelled meeting the user organizes.
  func hideMeeting(_ meeting: Meeting) async throws {
    try await core().hideMeeting(meetingId: meeting.id)
  }

  /// "Meet now"; `requestKey` is made once per tap, so a retry after a lost answer starts no second
  /// meeting.
  func startMeetingNow(requestKey: String) async throws -> SavedMeeting {
    try await core().startMeetingNow(maybeTitle: nil, maybeSpaceId: nil, requestKey: requestKey)
  }

  /// The notetaker "Notetaker joins the calls" asks in, or nil to hide the box (e2ee.md §8.2).
  func meetingNotetaker() async -> String? {
    do {
      return try await core().meetingNotetaker()
    } catch {
      meetingsLog.warning("the notetaker policy could not be read")
      return nil
    }
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
    measureLink(meetingId: meeting.id)
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
