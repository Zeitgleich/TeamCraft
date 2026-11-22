import Foundation
import UserNotifications

/// The meeting watch while the app runs (#2258), as desktop keeps one beside its reminders: the
/// core's `meetingWatch` runs once per foreground, whenever the user's meetings change (a meeting
/// changed, somebody knocked, a newly verified admitters list) and after joining a call. Each run
/// keeps the organizer's upkeep current and lets in, by itself, whoever came with the meeting link
/// where this device may (the organizer's "no lobby", or the organizer's signed admitters while in
/// the meeting's call; docs/meetings.md, "Lobby"). The core decides who; the app only says it, with
/// "Remove" ([LinkArrivalNotice]). Runs one after the other; one asked for meanwhile runs once after.
extension AppModel {
  /// A store change: the core's meetings revision is a cheap read, the list only read when it moved.
  func checkMeetingsRevision() async {
    guard case .ready = phase, let core = try? await core(), let revision = try? await core.meetingsRevision()
    else { return }
    guard revision != maybeMeetingsRevision else { return }
    let isFirst = maybeMeetingsRevision == nil
    maybeMeetingsRevision = revision
    // The first read only sets the mark: coming to the foreground already asked for a run.
    if !isFirst { meetingsChanged() }
  }

  /// The meetings changed, or the app came back: the meeting screens read again and the watch runs.
  func meetingsChanged() {
    meetingsChanges += 1
    requestMeetingWatch()
  }

  func requestMeetingWatch() {
    guard maybeMeetingWatch == nil else {
      isMeetingWatchAsked = true
      return
    }
    maybeMeetingWatch = Task { [weak self] in
      guard let self else { return }
      repeat {
        isMeetingWatchAsked = false
        await runMeetingWatch()
      } while isMeetingWatchAsked
      maybeMeetingWatch = nil
    }
  }

  /// "Remove" on a link arrival's notice (in the background, after unlocking): takes the person out
  /// of the meeting chat and its encryption. Only for the app's own notice (its tag); a removal that
  /// doesn't go through shows the notice again with why, so it never fails silently.
  func handle(linkArrivalRemoval info: [AnyHashable: Any], tag: String?, notificationId: String) async {
    let center = UNUserNotificationCenter.current()
    guard NotificationAuthenticator().isValid(tag: tag, for: notificationId),
      let arrival = LinkArrivalNotice.arrival(from: info),
      let core = try? await core()
    else {
      center.removeDeliveredNotifications(withIdentifiers: [notificationId])
      return
    }
    do {
      try await core.removeLinkArrival(conversationId: arrival.conversationId, userId: arrival.userId)
      center.removeDeliveredNotifications(withIdentifiers: [notificationId])
    } catch {
      let notice = LinkArrivalNotice(arrival, showsNames: await joinRequestShowsNames(core), isFailed: true)
      try? await center.add(notice.request())
    }
  }

  // MARK: Private

  private func runMeetingWatch() async {
    guard case .ready = phase, let core = try? await core() else { return }
    if !isAdmitterMarked {
      // Guests of the user's meetings are told otherwise that the organizer's app needs an update.
      isAdmitterMarked = (try? await core.markAdmitter()) != nil
    }
    // Offline or refused: the next change or foreground tries again.
    guard let arrivals = try? await core.meetingWatch(), !arrivals.isEmpty else { return }
    let showsNames = await joinRequestShowsNames(core)
    let center = UNUserNotificationCenter.current()
    for arrival in arrivals {
      try? await center.add(LinkArrivalNotice(arrival, showsNames: showsNames).request())
    }
  }
}

/// The notice that somebody was let into a meeting by this device without a click (#2258), with
/// "Remove" as desktop's notice has it. Names and the action only where messages would show them;
/// otherwise it says only that somebody joined, and removing is done in the app.
struct LinkArrivalNotice {
  let identifier: String
  let title: String
  let body: String
  let categoryIdentifier: String
  let isQuiet: Bool
  let arrival: LinkArrival

  init(_ arrival: LinkArrival, showsNames: Bool, isFailed: Bool = false) {
    self.arrival = arrival
    identifier = "link-arrival-\(arrival.meetingId)-\(arrival.userId)"
    isQuiet = isFailed
    guard showsNames else {
      title = String(localized: "link_arrival_hidden_title")
      body = String(localized: "link_arrival_hidden_text")
      categoryIdentifier = ""
      return
    }
    title = String(localized: "link_arrival_title \(arrival.displayName)")
    body =
      isFailed
      ? String(localized: "link_arrival_remove_failed")
      : String(localized: "link_arrival_text \(arrival.meetingTitle)")
    categoryIdentifier = NotificationCategories.linkArrivalCategory
  }

  /// The request to hand to the notification center. The tag over its identifier marks it as this
  /// app's own: "Remove" is refused without it. A tap opens the meeting's chat like a message.
  func request() -> UNNotificationRequest {
    let content = UNMutableNotificationContent()
    content.title = title
    content.body = body
    content.categoryIdentifier = categoryIdentifier
    content.threadIdentifier = NotificationKeys.meetingThread(arrival.meetingId)
    content.sound = isQuiet ? nil : .default
    var info: [AnyHashable: Any] = [
      NotificationCategories.conversationKey: arrival.conversationId,
      Self.meetingKey: arrival.meetingId,
      Self.meetingTitleKey: arrival.meetingTitle,
      Self.userKey: arrival.userId,
      Self.nameKey: arrival.displayName,
    ]
    if let tag = NotificationAuthenticator().tag(for: identifier) {
      info[NotificationAuthenticator.tagKey] = tag
    }
    content.userInfo = info
    return UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
  }

  /// The arrival a notice of this app's carries, for "Remove".
  static func arrival(from info: [AnyHashable: Any]) -> LinkArrival? {
    guard let conversationId = info[NotificationCategories.conversationKey] as? String,
      let meetingId = info[meetingKey] as? String,
      let userId = info[userKey] as? String
    else { return nil }
    return LinkArrival(
      meetingId: meetingId, meetingTitle: info[meetingTitleKey] as? String ?? "",
      conversationId: conversationId, userId: userId, displayName: info[nameKey] as? String ?? "")
  }

  private static let meetingKey = "link_arrival_meeting"
  private static let meetingTitleKey = "link_arrival_meeting_title"
  private static let userKey = "link_arrival_user"
  private static let nameKey = "link_arrival_name"
}
